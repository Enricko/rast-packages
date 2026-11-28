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
//! One call runs one method, or several in order (`calls`, up to
//! [`MAX_CALLS`]: one step of the model and one question to the person for
//! a whole series of edits).
//!
//! Reading (`engine.commands`, `document.inspect`, `ui.inspect`,
//! `ui.screenshot`, `ui.render`) never asks first; every other method changes
//! what the app shows and asks, as other tools that act do. A picture of the
//! window goes to the chat, and to the model when it reads pictures
//! ([`super::context`]).

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
/// The wait grows by this for each further method of a series.
const STEP_WAIT: Duration = Duration::from_secs(45);
/// The longest wait, however long the series.
const MAX_WAIT: Duration = Duration::from_secs(30 * 60);
/// Methods one call runs at most.
pub const MAX_CALLS: usize = 50;
/// Largest `params` a call sends (all of a series').
const MAX_PARAMS_BYTES: usize = 200 * 1024;
/// Longest answer of one method of a series the model gets.
const MAX_STEP_CHARS: usize = 8000;
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
    let read = |call: &Value| {
        call.get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| READ_METHODS.contains(&method.trim()))
    };
    match input.get("calls").and_then(Value::as_array) {
        Some(calls) => !calls.is_empty() && calls.iter().all(read),
        None => read(input),
    }
}

/// `use_app`, naming the apps whose windows may take commands (`pictures`:
/// the model reads the window pictures).
pub fn spec(apps: &[HostApp], pictures: bool) -> Option<ToolSpec> {
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
    let screenshot = if pictures {
        "a picture of the window: you see it right after the call's result (and the person sees it in the chat); look at it to check a layout"
    } else {
        "a picture of the window, shown to the person in the chat (you get only its description)"
    };
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
- ui.screenshot {{}}: {screenshot}\n\
For a series of edits give calls: [{{method, params}}, …] instead of method and params: up to {MAX_CALLS} run in order in one go (much faster than one call each) and stop at the first that fails. Check your work with document.inspect. An app that has no commands says so. The apps:\n{list}"
        ),
        parameters: json!({
            "type": "object",
            "properties": {
                "app": {"type": "string", "enum": ids, "description": "The app's id"},
                "method": {"type": "string", "description": "A control method, e.g. engine.execute"},
                "params": {"type": "object", "description": "The method's parameters"},
                "calls": {
                    "type": "array",
                    "maxItems": MAX_CALLS,
                    "description": "Several methods to run in order, instead of method and params",
                    "items": {
                        "type": "object",
                        "properties": {
                            "method": {"type": "string"},
                            "params": {"type": "object"},
                        },
                        "required": ["method"],
                    },
                },
            },
            "required": ["app"],
        }),
    })
}

/// One method of a call and its parameters.
pub type Method = (String, Value);
/// A picture an answer carried: by which method, and its PNG.
type Taken = Option<(usize, Vec<u8>)>;

/// The call, checked: the app, its methods and whether it is a series.
pub fn check<'a>(
    available: &'a Available,
    input: &Value,
) -> Result<(&'a HostApp, Vec<Method>, bool), String> {
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
    let (methods, batch) = match input.get("calls") {
        None | Some(Value::Null) => (vec![method_of(input)?], false),
        Some(Value::Array(calls)) if calls.is_empty() => {
            return Err("calls lists at least one method".into())
        }
        Some(Value::Array(calls)) if calls.len() > MAX_CALLS => {
            return Err(format!(
                "calls lists at most {MAX_CALLS} methods: split the series"
            ))
        }
        Some(Value::Array(calls)) => (
            calls
                .iter()
                .enumerate()
                .map(|(n, call)| method_of(call).map_err(|e| format!("calls[{n}]: {e}")))
                .collect::<Result<Vec<_>, _>>()?,
            true,
        ),
        Some(_) => return Err("calls is a list of {method, params}".into()),
    };
    let size: usize = methods.iter().map(|(_, p)| p.to_string().len()).sum();
    if size > MAX_PARAMS_BYTES {
        return Err(format!(
            "params is larger than {} KB",
            MAX_PARAMS_BYTES / 1024
        ));
    }
    Ok((app, methods, batch))
}

fn method_of(call: &Value) -> Result<Method, String> {
    let method = call
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
    let params = match call.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(_) => return Err("params is an object".into()),
    };
    Ok((method.to_string(), params))
}

/// A method as the model and the person read it (`engine.execute` by its
/// command).
fn label(method: &str, params: &Value) -> String {
    match params.get("command").and_then(Value::as_str) {
        Some(command) if method == "engine.execute" => format!("{method} {command}"),
        _ => method.to_string(),
    }
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
    let (app, methods, batch) = check(available, input)?;
    let wait = (WAIT + STEP_WAIT * (methods.len() as u32).saturating_sub(1)).min(MAX_WAIT);
    let result = match tokio::time::timeout(wait, answer).await {
        Ok(Ok(Answer::Done(result))) => result,
        Ok(Ok(Answer::Failed(error))) => return Err(format!("{}: {error}", app.name)),
        Ok(Err(_)) => return Err("the reply stopped waiting for the person's page".into()),
        Err(_) => {
            return Err(format!(
                "{} did not answer in {} seconds. The app works in the browser tab that started this reply: RAST must stay open there (a tab in the background may pause the app).",
                app.name,
                wait.as_secs()
            ))
        }
    };
    let (text, png, ran) = if batch {
        series_text(&app.name, &methods, &result)?
    } else {
        let mut result = result;
        let png = take_png(&mut result);
        (
            answer_text(&methods[0].0, &result),
            png.map(|p| (0, p)),
            true,
        )
    };
    let mut note = String::new();
    let image = match png {
        Some((step, png)) => match picture(state, user, conversation, app, &png).await {
            Ok(file) => {
                let from = if batch {
                    format!(" (from method {})", step + 1)
                } else {
                    String::new()
                };
                note = format!(
                    "The picture of {}'s window{from} ({} KB PNG) is in the chat.\n",
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
    let text = format!("{note}{text}");
    if ran {
        Ok(Output { text, image })
    } else {
        Err(text)
    }
}

/// One answer as the model reads it.
fn answer_text(method: &str, result: &Value) -> String {
    match result {
        Value::Null => format!("{method}: done."),
        Value::String(text) => text.clone(),
        Value::Object(map) if map.is_empty() => format!("{method}: done."),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// A series' answers (`{"batch": [{ok, result | error}, …]}`, as the page
/// ran them, stopping at the first failure) as the model reads them, the
/// last picture taken (and by which method), and whether any method ran.
fn series_text(
    app: &str,
    methods: &[Method],
    result: &Value,
) -> Result<(String, Taken, bool), String> {
    let Some(answers) = result.get("batch").and_then(Value::as_array) else {
        return Err(format!("{app}: the page gave no answers for the series"));
    };
    let mut lines = Vec::new();
    let mut png = None;
    let mut done = 0;
    let mut failed = false;
    for (n, ((method, params), answer)) in methods.iter().zip(answers).enumerate() {
        let name = label(method, params);
        if answer.get("ok").and_then(Value::as_bool) == Some(true) {
            let mut result = answer.get("result").cloned().unwrap_or(Value::Null);
            if let Some(picture) = take_png(&mut result) {
                png = Some((n, picture));
            }
            let text = answer_text(method, &result);
            let text = tools::cap(&text, MAX_STEP_CHARS, false);
            lines.push(format!("{}. {name}: {text}", n + 1));
            done += 1;
        } else {
            let error = answer
                .get("error")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .unwrap_or("the app could not do it");
            let error: String = error.chars().take(MAX_ERROR_CHARS).collect();
            lines.push(format!("{}. {name} failed: {error}", n + 1));
            failed = true;
            break;
        }
    }
    let total = methods.len();
    let head = if failed {
        let after = total - done - 1;
        if after > 0 {
            format!(
                "{app} ran {done} of {total}; the {after} after the one that failed did not run."
            )
        } else {
            format!("{app} ran {done} of {total}; the last one failed.")
        }
    } else if done < total {
        format!("{app} answered {done} of {total}.")
    } else {
        format!("{app} ran all {total}.")
    };
    Ok((format!("{head}\n{}", lines.join("\n")), png, done > 0))
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
        assert!(spec(&[app("files", "File Station", false)], false).is_none());
        let apps = [
            app("files", "File Station", false),
            app("wordcraft", "WordCraft", true),
        ];
        let spec = spec(&apps, false).unwrap();
        assert_eq!(spec.name, "use_app");
        assert!(spec.description.contains("you get only its description"));
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
        let (found, methods, batch) = check(
            &available,
            &json!({"app": "wordcraft", "method": "engine.execute", "params": {"command": "text.insert", "params": {"text": "Hi"}}}),
        )
        .unwrap();
        assert_eq!(
            (found.id.as_str(), methods[0].0.as_str(), batch),
            ("wordcraft", "engine.execute", false)
        );
        assert_eq!(methods[0].1["command"], "text.insert");
        let (_, methods, _) = check(
            &available,
            &json!({"app": "wordcraft", "method": "ui.inspect"}),
        )
        .unwrap();
        assert_eq!(methods, vec![("ui.inspect".to_string(), json!({}))]);
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
    fn a_series_runs_several_methods_in_one_call() {
        let apps = [app("wordcraft", "WordCraft", true)];
        let available = Available {
            names: vec![tools::USE_APP],
            apps: apps.to_vec(),
            ..Available::default()
        };
        let spec = spec(&apps, true).unwrap();
        assert!(spec.description.contains("you see it right after"));
        assert!(spec.description.contains("up to 50 run in order"));
        assert_eq!(spec.parameters["required"], json!(["app"]));

        let input = json!({"app": "wordcraft", "calls": [
            {"method": "engine.execute", "params": {"command": "shape.fill", "params": {"color": "red"}}},
            {"method": "ui.screenshot"},
        ]});
        let (_, methods, batch) = check(&available, &input).unwrap();
        assert!(batch);
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[1], ("ui.screenshot".to_string(), json!({})));
        assert!(!reads(&input));
        assert!(reads(
            &json!({"calls": [{"method": "document.inspect"}, {"method": "ui.screenshot"}]})
        ));
        assert!(!reads(&json!({"calls": []})));
        assert!(tools::summary(tools::USE_APP, &input)
            .ends_with("wordcraft 2 methods: engine.execute shape.fill, ui.screenshot"));

        for (bad, says) in [
            (json!({"app": "wordcraft", "calls": []}), "at least one"),
            (json!({"app": "wordcraft", "calls": "x"}), "a list of"),
            (
                json!({"app": "wordcraft", "calls": [{"method": "ui.inspect"}, {"method": "bad"}]}),
                "calls[1]",
            ),
            (
                json!({"app": "wordcraft", "calls": vec![json!({"method": "ui.inspect"}); MAX_CALLS + 1]}),
                "at most 50",
            ),
        ] {
            let error = check(&available, &bad).unwrap_err();
            assert!(error.contains(says), "{error}");
        }

        // The page's answers: every method, or up to the one that failed.
        let png = b"\x89PNG\r\n\x1a\nimg".to_vec();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let three = vec![
            (
                "engine.execute".to_string(),
                json!({"command": "shape.fill"}),
            ),
            ("ui.screenshot".to_string(), json!({})),
            (
                "engine.execute".to_string(),
                json!({"command": "file.save"}),
            ),
        ];
        let (text, picture, ran) = series_text(
            "WordCraft",
            &three,
            &json!({"batch": [
                {"ok": true, "result": null},
                {"ok": true, "result": {"png": encoded, "width": 800}},
                {"ok": true, "result": "saved"},
            ]}),
        )
        .unwrap();
        assert!(ran);
        assert_eq!(picture, Some((1, png)));
        assert_eq!(
            text,
            "WordCraft ran all 3.\n1. engine.execute shape.fill: engine.execute: done.\n2. ui.screenshot: {\n  \"width\": 800\n}\n3. engine.execute file.save: saved"
        );
        let (text, _, ran) = series_text(
            "WordCraft",
            &three,
            &json!({"batch": [{"ok": true}, {"ok": false, "error": "no window"}]}),
        )
        .unwrap();
        assert!(ran);
        assert!(text
            .starts_with("WordCraft ran 1 of 3; the 1 after the one that failed did not run.\n"));
        assert!(text.ends_with("2. ui.screenshot failed: no window"));
        let (_, _, ran) = series_text(
            "WordCraft",
            &three,
            &json!({"batch": [{"ok": false, "error": "x"}]}),
        )
        .unwrap();
        assert!(!ran);
        assert!(series_text("WordCraft", &three, &json!({"x": 1})).is_err());
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
