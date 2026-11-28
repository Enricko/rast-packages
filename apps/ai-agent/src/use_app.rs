//! `use_app`: work inside one of the person's RAST apps whose window takes
//! commands (an app that follows ArtCraft's Craft web host contract, such as
//! WordCraft or PdfCraft): read what it shows, run its commands, type into it.
//!
//! The app runs in the person's browser, so the call does too: the page that
//! started the reply hands the request to the app's window
//! (`ctx.desktop.request`, RAST 0.9.20 and later) and posts what the window
//! answered (`POST /api/ai/conversations/{id}/tools/{callId}/answer`), while
//! the reply waits for it ([`Pages`]).
//!
//! Reading (`engine.commands`, `document.inspect`, `ui.inspect`,
//! `ui.screenshot`, `ui.render`) never asks first; every other method changes
//! what the app shows and asks, as other tools that act do.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::host::{Agent, HostApp};
use super::provider::ToolSpec;
use super::tools::{Available, Output};
use super::{files, tools};
use crate::auth::AuthUser;

/// Methods that only read what the app shows.
pub const READ_METHODS: [&str; 5] = [
    "engine.commands",
    "document.inspect",
    "ui.inspect",
    "ui.screenshot",
    "ui.render",
];
/// How long a call waits for the page: the app may still be starting (its
/// files are tens of megabytes the first time).
pub const WAIT: Duration = Duration::from_secs(150);
/// Largest `params` a call sends.
const MAX_PARAMS_BYTES: usize = 200 * 1024;
/// Longest error a page's answer carries.
const MAX_ERROR_CHARS: usize = 2000;

/// The page's answer to a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Done(Value),
    Failed(String),
}

/// A waiting call: its conversation and call id.
type CallKey = (String, String);
/// Who may answer it (the owner's account) and where the answer goes.
type Waiter = (i64, oneshot::Sender<Answer>);

/// Calls waiting for the person's page, by conversation and call id.
#[derive(Debug, Default)]
pub struct Pages {
    waiting: Mutex<HashMap<CallKey, Waiter>>,
}

impl Pages {
    /// Wait for the page's answer to this call (from now: the page may answer
    /// as soon as it sees the call running).
    pub fn wait(&self, conversation: &str, call: &str, owner: i64) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert((conversation.to_string(), call.to_string()), (owner, tx));
        }
        rx
    }

    /// The call no longer waits (answered, timed out or stopped).
    pub fn forget(&self, conversation: &str, call: &str) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.remove(&(conversation.to_string(), call.to_string()));
        }
    }

    /// The owner's page answered: whether the call was waiting for it.
    pub fn answer(&self, conversation: &str, call: &str, owner: i64, answer: Answer) -> bool {
        let Ok(mut waiting) = self.waiting.lock() else {
            return false;
        };
        let key = (conversation.to_string(), call.to_string());
        match waiting.get(&key) {
            Some((who, _)) if *who == owner => {}
            _ => return false,
        }
        let Some((_, tx)) = waiting.remove(&key) else {
            return false;
        };
        tx.send(answer).is_ok()
    }
}

/// The page's answer as it posts it.
pub fn answer_of(ok: bool, result: Value, error: &str) -> Answer {
    if ok {
        Answer::Done(result)
    } else {
        let error = error.trim();
        let error: String = if error.is_empty() {
            "the app could not do it".into()
        } else {
            error.chars().take(MAX_ERROR_CHARS).collect()
        };
        Answer::Failed(error)
    }
}

/// Whether the call only reads what the app shows (it never asks first).
pub fn reads(input: &Value) -> bool {
    input
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(|method| READ_METHODS.contains(&method.trim()))
}

/// `use_app`, naming the apps whose windows may take commands.
pub fn spec(apps: &[HostApp]) -> Option<ToolSpec> {
    let apps: Vec<&HostApp> = apps.iter().filter(|app| app.commands).collect();
    if apps.is_empty() {
        return None;
    }
    let list = apps
        .iter()
        .map(|app| format!("- {} ({})", app.id, app.name))
        .collect::<Vec<_>>()
        .join("\n");
    let ids: Vec<&str> = apps.iter().map(|app| app.id.as_str()).collect();
    Some(ToolSpec {
        name: tools::USE_APP.into(),
        description: format!(
            "Work inside one of the person's RAST apps, in its window beside this chat, as the person would: read what it shows, run its commands, type into it. It works on what the window has open: to work on one of the person's files, open it first with open_app and the file's path. Changes stay in the app until it saves: run its save command when the person wants the file saved (Save writes the open file back to their files; Save As and exports ask the person where). Methods (the app's control channel):\n\
- engine.commands {{search?}}: the app's commands (id, label, params, enabled); search with a word, e.g. \"insert\", since the full list is long\n\
- engine.execute {{command, params?}}: run a command by its id (WordCraft: text.insert {{text}}, file.save)\n\
- document.inspect {{text?}}: what the open document holds\n\
- ui.inspect {{}}: the window's state (tab, dialog, selection, open documents)\n\
- ui.text {{text}}: type text at the cursor; ui.key {{key, shift?, alt?, cmd?}}: press a key (Enter, Tab, Left, B with cmd)\n\
- ui.click {{x, y}}: click at a point of the window\n\
- ui.screenshot {{}}: a picture of the window, shown to the person in the chat (you get only its description)\n\
Check your work with document.inspect. An app that has no commands says so. The apps:\n{list}"
        ),
        parameters: json!({
            "type": "object",
            "properties": {
                "app": {"type": "string", "enum": ids, "description": "The app's id"},
                "method": {"type": "string", "description": "A control method, e.g. engine.execute"},
                "params": {"type": "object", "description": "The method's parameters"},
            },
            "required": ["app", "method"],
        }),
    })
}

/// The call, checked: the app, the method and its parameters.
pub fn check<'a>(
    available: &'a Available,
    input: &Value,
) -> Result<(&'a HostApp, String, Value), String> {
    let app_id = input
        .get("app")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let Some(app) = available
        .apps
        .iter()
        .find(|app| app.id == app_id && app.commands)
    else {
        let ids: Vec<&str> = available
            .apps
            .iter()
            .filter(|app| app.commands)
            .map(|app| app.id.as_str())
            .collect();
        return Err(format!(
            "{app_id:?} is not an app that takes commands here; these may: {}",
            ids.join(", ")
        ));
    };
    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let well_formed = (3..=64).contains(&method.len())
        && method.contains('.')
        && method.starts_with(|c: char| c.is_ascii_lowercase())
        && method
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !well_formed {
        return Err("give a control method, such as engine.execute or document.inspect".into());
    }
    let params = match input.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(_) => return Err("params is an object".into()),
    };
    if params.to_string().len() > MAX_PARAMS_BYTES {
        return Err(format!(
            "params is larger than {} KB",
            MAX_PARAMS_BYTES / 1024
        ));
    }
    Ok((app, method.to_string(), params))
}

/// Run the call: wait for the person's page to answer it.
pub async fn run(
    state: &Agent,
    user: &AuthUser,
    available: &Available,
    conversation: &str,
    input: &Value,
    answer: oneshot::Receiver<Answer>,
) -> Result<Output, String> {
    let (app, method, _) = check(available, input)?;
    match tokio::time::timeout(WAIT, answer).await {
        Ok(Ok(Answer::Done(result))) => Ok(output(state, user, conversation, app, &method, result).await),
        Ok(Ok(Answer::Failed(error))) => Err(format!("{}: {error}", app.name)),
        Ok(Err(_)) => Err("the reply stopped waiting for the person's page".into()),
        Err(_) => Err(format!(
            "{} did not answer in {} seconds. The app works in the browser tab that started this reply: RAST must stay open there (a tab in the background may pause the app).",
            app.name,
            WAIT.as_secs()
        )),
    }
}

/// What the model gets: the answer as text; a picture goes to the chat.
async fn output(
    state: &Agent,
    user: &AuthUser,
    conversation: &str,
    app: &HostApp,
    method: &str,
    mut result: Value,
) -> Output {
    let mut note = String::new();
    let image = match take_png(&mut result) {
        Some(png) => match picture(state, user, conversation, app, &png).await {
            Ok(file) => {
                note = format!(
                    "The picture of {}'s window ({} KB PNG) is in the chat for the person; you get only this text.\n",
                    app.name,
                    png.len() / 1024
                );
                Some(file)
            }
            Err(error) => {
                note = format!("The picture could not be kept: {error}\n");
                None
            }
        },
        None => None,
    };
    let text = match &result {
        Value::Null => format!("{method}: done."),
        Value::String(text) => text.clone(),
        Value::Object(map) if map.is_empty() => format!("{method}: done."),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    Output {
        text: format!("{note}{text}"),
        image,
    }
}

/// A PNG the answer carries as base64 (taken out of it).
fn take_png(result: &mut Value) -> Option<Vec<u8>> {
    let map = result.as_object_mut()?;
    for key in ["png", "pngBase64", "png_base64", "data", "base64"] {
        let Some(Value::String(encoded)) = map.get(key) else {
            continue;
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
            continue;
        };
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            map.remove(key);
            return Some(bytes);
        }
    }
    None
}

async fn picture(
    state: &Agent,
    user: &AuthUser,
    conversation: &str,
    app: &HostApp,
    png: &[u8],
) -> Result<Value, String> {
    let name = format!("{} window.png", app.name);
    let file = files::store(
        &state.db,
        state.data_dir(),
        user.id,
        Some(&name),
        "image/png",
        png,
    )
    .await
    .map_err(|e| e.public_message())?;
    files::attach(
        &state.db,
        user.id,
        conversation,
        std::slice::from_ref(&file.id),
    )
    .await
    .map_err(|e| e.public_message())?;
    Ok(file.json())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, commands: bool) -> HostApp {
        HostApp {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            opens: Vec::new(),
            commands,
        }
    }

    #[test]
    fn only_apps_that_take_commands_are_offered() {
        assert!(spec(&[app("files", "File Station", false)]).is_none());
        let apps = [
            app("files", "File Station", false),
            app("wordcraft", "WordCraft", true),
        ];
        let spec = spec(&apps).unwrap();
        assert_eq!(spec.name, "use_app");
        assert!(spec
            .description
            .ends_with("The apps:\n- wordcraft (WordCraft)"));
        assert_eq!(
            spec.parameters["properties"]["app"]["enum"],
            json!(["wordcraft"])
        );

        let available = Available {
            names: vec![tools::USE_APP],
            apps: apps.to_vec(),
            ..Available::default()
        };
        let (found, method, params) = check(
            &available,
            &json!({"app": "wordcraft", "method": "engine.execute", "params": {"command": "text.insert", "params": {"text": "Hi"}}}),
        )
        .unwrap();
        assert_eq!(
            (found.id.as_str(), method.as_str()),
            ("wordcraft", "engine.execute")
        );
        assert_eq!(params["command"], "text.insert");
        let (_, _, empty) = check(
            &available,
            &json!({"app": "wordcraft", "method": "ui.inspect"}),
        )
        .unwrap();
        assert_eq!(empty, json!({}));
        assert!(
            check(&available, &json!({"app": "files", "method": "ui.inspect"}))
                .unwrap_err()
                .contains("these may: wordcraft")
        );
        for method in ["", "inspect", "Engine.execute", "engine execute", "a.b;rm"] {
            assert!(
                check(&available, &json!({"app": "wordcraft", "method": method})).is_err(),
                "{method}"
            );
        }
        assert!(check(
            &available,
            &json!({"app": "wordcraft", "method": "ui.text", "params": "hi"})
        )
        .is_err());
        let big = "x".repeat(MAX_PARAMS_BYTES);
        assert!(check(
            &available,
            &json!({"app": "wordcraft", "method": "ui.text", "params": {"text": big}})
        )
        .is_err());
    }

    #[test]
    fn reading_never_asks_first_and_the_rest_does() {
        assert!(reads(&json!({"method": "document.inspect"})));
        assert!(reads(&json!({"method": "ui.screenshot"})));
        assert!(!reads(&json!({"method": "engine.execute"})));
        assert!(!reads(&json!({"method": "ui.text"})));
        assert!(!reads(&json!({})));
        assert!(!tools::asks_first(
            tools::USE_APP,
            &json!({"method": "engine.commands"})
        ));
        assert!(tools::asks_first(
            tools::USE_APP,
            &json!({"method": "ui.key"})
        ));
    }

    #[test]
    fn only_the_owners_page_answers_and_only_once() {
        let pages = Pages::default();
        let mut rx = pages.wait("c1", "call_1", 7);
        assert!(!pages.answer("c1", "call_1", 8, Answer::Done(json!(1))));
        assert!(!pages.answer("c2", "call_1", 7, Answer::Done(json!(1))));
        assert!(pages.answer("c1", "call_1", 7, Answer::Done(json!({"ok": 1}))));
        assert_eq!(rx.try_recv().unwrap(), Answer::Done(json!({"ok": 1})));
        assert!(!pages.answer("c1", "call_1", 7, Answer::Done(json!(2))));
        let _rx = pages.wait("c1", "call_2", 7);
        pages.forget("c1", "call_2");
        assert!(!pages.answer("c1", "call_2", 7, Answer::Failed("x".into())));
        assert_eq!(
            answer_of(false, Value::Null, "  "),
            Answer::Failed("the app could not do it".into())
        );
        assert_eq!(answer_of(true, json!([1]), ""), Answer::Done(json!([1])));
    }

    #[test]
    fn a_picture_is_taken_out_of_the_answer() {
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut answer = json!({"png": encoded, "width": 800});
        assert_eq!(take_png(&mut answer), Some(png));
        assert_eq!(answer, json!({"width": 800}));
        let mut text = json!({"data": "not base64!"});
        assert_eq!(take_png(&mut text), None);
        let mut other =
            json!({"data": base64::engine::general_purpose::STANDARD.encode(b"GIF89a")});
        assert_eq!(take_png(&mut other), None);
        assert_eq!(take_png(&mut json!("text")), None);
    }
}
