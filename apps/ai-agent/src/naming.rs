//! A chat's title from its first answer.
//!
//! A new chat is first called after the first line of its first message
//! ([`super::title_from`]). Once the model's first step is written, the same
//! model (same provider account) is asked, in a short request of its own,
//! for a few words that say what the chat is doing, and they become the
//! title, unless the owner renamed the chat meanwhile. The request counts in
//! the owner's usage like any other (its estimate joins the first reply's);
//! when it fails, the first title stays.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde_json::{json, Value};

use super::host::Agent;
use super::provider::{self, ChatRequest, Delta, ToolCall, Turn, UserTurn};
use super::run::ReplyJob;
use super::Effort;

/// What goes into the request, at most, from the message and the answer.
const ASKED_CHARS: usize = 1500;
const ANSWERED_CHARS: usize = 1500;
/// A title at most this long (as [`super::title_from`]).
const TITLE_CHARS: usize = 60;
/// The answer is cut off past this.
const ENOUGH_CHARS: usize = 300;

const SYSTEM: &str = "You name chats. Reply with only a title for this chat: 3 to 7 words, \
in the language of the person's message, that say what is being done (for example \
\"Fixing the nginx redirect loop\" or \"Plan for a week in Kyoto\"). \
No quotes, no emoji, no full stop at the end.";

/// Ask for the chat's title in the background (see the module docs).
pub fn start(
    state: &Agent,
    job: &ReplyJob,
    title: String,
    history: &[Turn],
    calls: &[ToolCall],
    blocks: &[Value],
) {
    let asked = history
        .iter()
        .rev()
        .find_map(|turn| match turn {
            Turn::User(user) => Some(asked_text(user)),
            _ => None,
        })
        .unwrap_or_default();
    let answered = answered_text(&super::text_of(&Value::Array(blocks.to_vec())), calls);
    if asked.trim().is_empty() && answered.trim().is_empty() {
        return;
    }
    let request = ChatRequest {
        kind: job.request.kind,
        base_url: job.request.base_url.clone(),
        api_key: job.request.api_key.clone(),
        model: job.model.clone(),
        system: SYSTEM.to_string(),
        messages: vec![Turn::User(UserTurn {
            text: prompt(&asked, &answered),
            files: Vec::new(),
        })],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        effort: Some(Effort::Low),
        session: job.conversation_id.clone(),
        account: job.request.account.clone(),
    };
    let state = state.clone();
    let owner = job.owner.clone();
    let conversation_id = job.conversation_id.clone();
    let message_id = job.message_id;
    let (provider_id, account_id, model) = (
        job.provider_id.clone(),
        job.account_id.clone(),
        job.model.clone(),
    );
    tokio::spawn(async move {
        let prompt_chars =
            request.system.len() + request.messages.iter().map(Turn::chars).sum::<usize>();
        let asked = tokio::task::spawn_blocking(move || ask(&request)).await;
        let (text, usage) = match asked {
            Ok(Ok(answer)) => answer,
            Ok(Err(error)) => {
                tracing::info!(conversation = %conversation_id, error = %error, "the model could not name a chat; its first title stays");
                return;
            }
            Err(error) => {
                tracing::info!(conversation = %conversation_id, error = %error, "naming a chat stopped unexpectedly");
                return;
            }
        };
        let usage = if usage.is_empty() && !text.is_empty() {
            provider::Usage {
                input: (prompt_chars / 4) as i64,
                output: (text.len() / 4).max(1) as i64,
                ..Default::default()
            }
        } else {
            usage
        };
        // Counted as the owner's, and its estimate joins the first reply's.
        match super::record_usage(
            &state.db,
            owner.id,
            &provider_id,
            &account_id,
            &model,
            &usage,
        )
        .await
        {
            Ok(estimate) if estimate > 0 => {
                let added = sqlx::query(
                    "UPDATE ai_messages SET estimated_cost = estimated_cost + ? WHERE id = ?",
                )
                .bind(estimate)
                .bind(message_id)
                .execute(&state.db)
                .await;
                if let Err(e) = added {
                    tracing::warn!(error = %e, "could not store an AI reply's estimate");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not record AI usage"),
        }
        let Some(named) = clean(&text) else {
            return;
        };
        if named == title {
            return;
        }
        // Only over the first title: a name the owner gave stays.
        let changed =
            sqlx::query("UPDATE ai_conversations SET title = ? WHERE id = ? AND title = ?")
                .bind(&named)
                .bind(&conversation_id)
                .bind(&title)
                .execute(&state.db)
                .await;
        match changed {
            Ok(done) if done.rows_affected() == 1 => {}
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "could not store a chat's title");
                return;
            }
        }
        if let Ok(Some(conversation)) = super::conversation(&state.db, &conversation_id).await {
            let running = state.ai.running(&conversation_id);
            state.host.publish_user_and_admin(
                "ai.conversation",
                owner.id,
                json!({ "conversation": conversation.json(Some(&owner.username), running) }),
            );
        }
    });
}

/// Stream the answer (stopped when it runs long: a title is one line, and
/// the provider's usage comes at the end of a whole answer).
fn ask(request: &ChatRequest) -> Result<(String, provider::Usage), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut text = String::new();
    let outcome = provider::stream_chat(request, &cancel, &mut |delta| {
        if let Delta::Text(t) = delta {
            text.push_str(&t);
            if text.len() > ENOUGH_CHARS {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    })
    .map_err(|e| e.message)?;
    Ok((text, outcome.usage))
}

fn asked_text(user: &UserTurn) -> String {
    let mut text = cut(user.text.trim(), ASKED_CHARS);
    if text.is_empty() {
        text = user
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
    }
    text
}

fn answered_text(text: &str, calls: &[ToolCall]) -> String {
    let mut out = cut(text.trim(), ANSWERED_CHARS);
    for call in calls.iter().take(5) {
        let input = call.input.to_string();
        out.push_str(&format!("\n[uses {}: {}]", call.name, cut(&input, 160)));
    }
    out
}

/// The request's message.
pub fn prompt(asked: &str, answered: &str) -> String {
    format!("The person wrote:\n{asked}\n\nThe assistant began its answer:\n{answered}\n\nTitle for this chat:")
}

/// A title from the model's answer: its first line, without quotes,
/// markdown, a "Title:" label or a full stop; `None` when nothing is left.
pub fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut line = line.trim_start_matches(['#', '*', '-', '>', ' ']).trim();
    for label in ["title:", "chat title:", "judul:"] {
        if line.len() >= label.len() && line[..label.len()].eq_ignore_ascii_case(label) {
            line = line[label.len()..].trim();
        }
    }
    let line = line
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | '*' | '_' | '“' | '”' | '‘' | '’' | '「' | '」'
            )
        })
        .trim()
        .trim_end_matches(['.', '。', ':', ';', ','])
        .trim();
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let line = words.join(" ");
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title = format!("{}…", title.trim_end());
    }
    Some(title)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_quotes_labels_and_full_stops() {
        assert_eq!(
            clean("Fixing the nginx redirect loop").as_deref(),
            Some("Fixing the nginx redirect loop")
        );
        assert_eq!(
            clean("\n\n\"Plan for a week in Kyoto.\"\nmore").as_deref(),
            Some("Plan for a week in Kyoto")
        );
        assert_eq!(
            clean("Title: **Backup  script for /srv**").as_deref(),
            Some("Backup script for /srv")
        );
        assert_eq!(
            clean("# Memperbaiki konfigurasi nginx").as_deref(),
            Some("Memperbaiki konfigurasi nginx")
        );
        assert_eq!(clean("  \n \"\" ").as_deref(), None);
        let long = "word ".repeat(40);
        let cut = clean(&long).unwrap_or_default();
        assert!(
            cut.ends_with("word…") && cut.chars().count() <= TITLE_CHARS + 1,
            "{cut}"
        );
    }

    #[test]
    fn the_request_carries_the_message_and_the_start_of_the_answer() {
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            input: json!({"path": "/etc/nginx/nginx.conf"}),
        }];
        let answered = answered_text("Let me look at your config.", &calls);
        assert_eq!(
            answered,
            "Let me look at your config.\n[uses read_file: {\"path\":\"/etc/nginx/nginx.conf\"}]"
        );
        let text = prompt("Why does my site loop?", &answered);
        assert!(text.starts_with("The person wrote:\nWhy does my site loop?\n\nThe assistant began its answer:\nLet me look"));
        assert!(text.ends_with("Title for this chat:"));
        assert_eq!(cut("abcdef", 3), "abc…");
    }
}
