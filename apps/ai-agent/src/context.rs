//! Room in the model's context for long replies: how much a model reads at
//! once, whether it sees pictures, fitting a long chat into it, and the
//! pictures of app windows the model gets to see.
//!
//! A reply runs until the model is done (an hour or more of work in an app),
//! so its turns grow with every step. Before each step the chat is fitted
//! into the model's window ([`fit`]): the oldest tool results are shortened
//! in fixed chunks, so the start of the request stays the same for many
//! steps (and the provider's cache of it stays good).

use std::path::Path;

use base64::Engine;
use serde_json::Value;

use super::files;
use super::provider::{Attachment, Turn, UserTurn};

/// The fewest characters a request's turns may take.
const MIN_TURN_CHARS: usize = 60_000;
/// The most characters a request's turns take, however large the model's
/// window (every step sends them again).
const MAX_TURN_CHARS: usize = 900_000;
/// Results kept whole, however long the chat: the newest ones.
const KEEP_WHOLE: usize = 12;
/// Results shortened at a time, oldest first.
const CUT_CHUNK: usize = 25;
/// What a shortened result keeps.
const KEPT_CHARS: usize = 300;
/// A string in an earlier call's input longer than this is shortened too.
const LONG_INPUT_CHARS: usize = 2000;
/// Pictures of app windows a request carries: the newest ones.
pub const MAX_PICTURES: usize = 3;

/// About how many tokens `model` reads at once, by its family (the same
/// table as the window's usage bar); 128k when its name says nothing known.
pub fn context_window(model: &str) -> usize {
    let lower = model.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let has = |part: &str| name.contains(part);
    let digit_after = |prefix: &str, digits: &str| {
        name.match_indices(prefix).any(|(at, _)| {
            name[at + prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| digits.contains(c))
        })
    };
    if has("gemini") {
        1_048_576
    } else if ["claude", "opus", "sonnet", "haiku"].iter().any(|p| has(p)) {
        200_000
    } else if has("gpt-4.1") {
        1_047_576
    } else if has("gpt-oss") {
        131_072
    } else if digit_after("gpt-", "56789") || has("codex") || is_o_series(name, "134") {
        400_000
    } else if has("gpt-4o") || has("gpt-4-turbo") {
        128_000
    } else if has("grok-4") || has("grok-code") {
        256_000
    } else if has("grok") || has("gpt-oss") {
        131_072
    } else if has("deepseek") {
        128_000
    } else if has("qwen3-coder")
        || has("qwen3-max")
        || digit_after("qwen3.", "56789")
        || digit_after("qwen", "456789")
    {
        262_144
    } else if has("qwen") || has("qwq") {
        131_072
    } else if has("kimi") || has("moonshot") {
        262_144
    } else if digit_after("glm-4.", "56789") || digit_after("glm-", "56789") {
        200_000
    } else if has("glm") {
        128_000
    } else if has("minimax") {
        204_800
    } else if has("codestral") {
        256_000
    } else if [
        "mistral",
        "mixtral",
        "pixtral",
        "voxtral",
        "devstral",
        "magistral",
        "ministral",
    ]
    .iter()
    .any(|p| has(p))
    {
        128_000
    } else if has("llama-4") || has("llama4") {
        1_000_000
    } else {
        128_000
    }
}

/// o1, o3, o4 (alone or followed by a dash).
fn is_o_series(name: &str, digits: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('o')
        && chars.next().is_some_and(|c| digits.contains(c))
        && matches!(chars.next(), None | Some('-'))
}

/// Whether `model` reads pictures, by its family (models known to; others
/// get a picture's description only).
pub fn sees_images(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let has = |part: &str| name.contains(part);
    has("gemini")
        || has("gemma-3")
        || has("gemma3")
        || has("gemma-4")
        || ["claude", "opus", "sonnet", "haiku"].iter().any(|p| has(p))
        || has("gpt-4o")
        || has("gpt-4.1")
        || ["gpt-5", "gpt-6", "gpt-7", "gpt-8", "gpt-9"]
            .iter()
            .any(|p| has(p))
        || is_o_series(name, "34")
        || has("codex")
        || has("grok-4")
        || has("llama-4")
        || has("llama4")
        || has("pixtral")
        || has("vision")
        || has("-vl")
        || has("vl-")
        || ["glm-4.5v", "glm-4.6v", "glm-4.1v", "glm-5v"]
            .iter()
            .any(|p| has(p))
}

/// The characters a request's turns may take with `model` (about 60% of its
/// window at three characters a token), less the system prompt's.
pub fn budget(model: &str, system_chars: usize) -> usize {
    (context_window(model) * 18 / 10)
        .clamp(MIN_TURN_CHARS, MAX_TURN_CHARS)
        .saturating_sub(system_chars)
        .max(MIN_TURN_CHARS)
}

/// Fit `turns` into `budget` characters: shorten the oldest tool results
/// (and the long strings of their calls' inputs), [`CUT_CHUNK`] at a time,
/// never the newest [`KEEP_WHOLE`]. Returns how many were shortened.
pub fn fit(turns: &mut [Turn], budget: usize) -> usize {
    let mut total: usize = turns.iter().map(Turn::chars).sum();
    if total <= budget {
        return 0;
    }
    let places: Vec<(usize, usize)> = turns
        .iter()
        .enumerate()
        .flat_map(|(at, turn)| {
            let count = match turn {
                Turn::Results(results) => results.len(),
                _ => 0,
            };
            (0..count).map(move |index| (at, index))
        })
        .collect();
    let cuttable = places.len().saturating_sub(KEEP_WHOLE);
    let mut done = 0;
    while total > budget && done < cuttable {
        let end = (done + CUT_CHUNK).min(cuttable);
        for &(at, index) in &places[done..end] {
            total = total.saturating_sub(shorten(turns, at, index));
        }
        done = end;
    }
    done
}

/// Shorten one result and its call's input; returns the characters saved.
fn shorten(turns: &mut [Turn], at: usize, index: usize) -> usize {
    let mut saved = 0;
    let id = match &mut turns[at] {
        Turn::Results(results) => {
            let result = &mut results[index];
            if result.output.len() > KEPT_CHARS + 200 {
                let all = result.output.chars().count();
                let head: String = result.output.chars().take(KEPT_CHARS).collect();
                let shorter = format!(
                    "{head}\n[… shortened to make room in this long chat: {all} characters in all; run the call again to see it whole]"
                );
                saved += result.output.len().saturating_sub(shorter.len());
                result.output = shorter;
            }
            result.id.clone()
        }
        _ => return 0,
    };
    for turn in turns[..at].iter_mut().rev() {
        if let Turn::Assistant { calls, .. } = turn {
            if let Some(call) = calls.iter_mut().find(|c| c.id == id) {
                let before = call.input.to_string().len();
                shorten_strings(&mut call.input);
                saved += before.saturating_sub(call.input.to_string().len());
                break;
            }
        }
    }
    saved
}

fn shorten_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > LONG_INPUT_CHARS => {
            let all = text.chars().count();
            let head: String = text.chars().take(KEPT_CHARS).collect();
            *text = format!("{head}… [shortened here: {all} characters in all]");
        }
        Value::Array(items) => items.iter_mut().for_each(shorten_strings),
        Value::Object(map) => map.values_mut().for_each(shorten_strings),
        _ => {}
    }
}

/// A picture a tool call kept (an app window's): the call and the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub call_id: String,
    pub file_id: String,
    pub name: String,
    pub mime: String,
}

/// The newest pictures of a reply's tool blocks, oldest first.
pub fn pictures(blocks: &[Value]) -> Vec<Picture> {
    let mut found: Vec<Picture> = blocks
        .iter()
        .rev()
        .filter(|b| super::is_tool_block(b) && b["status"] == "done")
        .filter_map(|b| {
            let image = b.get("image")?;
            let mime = image.get("mime").and_then(Value::as_str).unwrap_or("");
            if !mime.starts_with("image/") {
                return None;
            }
            Some(Picture {
                call_id: b.get("id")?.as_str()?.to_string(),
                file_id: image.get("id")?.as_str()?.to_string(),
                name: image
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("picture")
                    .to_string(),
                mime: mime.to_string(),
            })
        })
        .take(MAX_PICTURES)
        .collect();
    found.reverse();
    found
}

/// Put each picture after the results of its call, as what the person's
/// side shows the model. Returns how many went in (a file that is gone is
/// left out).
pub async fn show_pictures(
    turns: &mut Vec<Turn>,
    pictures: &[Picture],
    data_dir: &Path,
    user_id: i64,
) -> usize {
    let mut placed = 0;
    for picture in pictures {
        let Some(at) = turns.iter().position(|turn| {
            matches!(turn, Turn::Results(results) if results.iter().any(|r| r.id == picture.call_id))
        }) else {
            continue;
        };
        let Ok(bytes) = tokio::fs::read(files::path_of(data_dir, user_id, &picture.file_id)).await
        else {
            continue;
        };
        let turn = Turn::User(UserTurn {
            text: format!(
                "[{} — the picture call {} took]",
                picture.name, picture.call_id
            ),
            files: vec![Attachment {
                id: picture.file_id.clone(),
                name: picture.name.clone(),
                mime: picture.mime.clone(),
                size: bytes.len(),
                is_text: false,
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            }],
        });
        turns.insert(at + 1, turn);
        placed += 1;
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::{ToolCall, ToolResult};
    use serde_json::json;

    #[test]
    fn windows_and_pictures_by_model_family() {
        assert_eq!(context_window("gemini-4-neon-xhigh"), 1_048_576);
        assert_eq!(context_window("anthropic/claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window("gpt-5.2-codex"), 400_000);
        assert_eq!(context_window("o3-mini"), 400_000);
        assert_eq!(context_window("gpt-4o-mini"), 128_000);
        assert_eq!(context_window("glm-4.6"), 200_000);
        assert_eq!(context_window("qwen3-coder-plus"), 262_144);
        assert_eq!(context_window("some-new-model"), 128_000);
        assert!(sees_images("gemini-4-neon-xhigh"));
        assert!(sees_images("claude-opus-4-6"));
        assert!(sees_images("gpt-5.2"));
        assert!(sees_images("o4-mini"));
        assert!(sees_images("qwen2.5-vl-72b"));
        assert!(!sees_images("deepseek-chat"));
        assert!(!sees_images("glm-4.6"));
        assert!(!sees_images("ws-model"));
        assert_eq!(budget("gemini-x", 0), MAX_TURN_CHARS);
        assert_eq!(budget("deepseek-chat", 10_000), 230_400 - 10_000);
        assert_eq!(budget("tiny", 1_000_000), MIN_TURN_CHARS);
    }

    fn step(id: &str, output: &str) -> [Turn; 2] {
        [
            Turn::Assistant {
                reasoning: String::new(),
                text: String::new(),
                calls: vec![ToolCall {
                    id: id.into(),
                    name: "read_file".into(),
                    input: json!({"path": "/a", "content": "c".repeat(5000)}),
                }],
            },
            Turn::Results(vec![ToolResult {
                id: id.into(),
                name: "read_file".into(),
                output: output.into(),
                is_error: false,
            }]),
        ]
    }

    fn chat(steps: usize) -> Vec<Turn> {
        let mut turns = vec![Turn::User("go".into())];
        for n in 0..steps {
            turns.extend(step(&format!("c{n}"), &"x".repeat(10_000)));
        }
        turns
    }

    #[test]
    fn a_long_chat_fits_by_shortening_its_oldest_results_in_chunks() {
        let mut short = chat(5);
        assert_eq!(fit(&mut short, 1_000_000), 0);
        assert_eq!(short, chat(5));

        let mut turns = chat(100);
        let budget = 600_000;
        let cut = fit(&mut turns, budget);
        assert_eq!(cut % CUT_CHUNK, 0, "{cut}");
        assert!(turns.iter().map(Turn::chars).sum::<usize>() <= budget);
        let Turn::Results(first) = &turns[2] else {
            panic!()
        };
        assert!(first[0].output.contains("shortened to make room"));
        assert!(first[0].output.contains("10000 characters in all"));
        let Turn::Assistant { calls, .. } = &turns[1] else {
            panic!()
        };
        assert!(calls[0].input["content"]
            .as_str()
            .unwrap()
            .contains("shortened here"));
        assert_eq!(calls[0].input["path"], "/a");
        let Turn::Results(newest) = turns.last().unwrap() else {
            panic!()
        };
        assert_eq!(newest[0].output.len(), 10_000);

        // One more step: the same results stay shortened the same way (the
        // start of the request does not change) until the next chunk.
        let mut more = chat(101);
        assert_eq!(fit(&mut more, budget), cut);
        assert_eq!(more[..turns.len()], turns[..]);

        // However long, the newest results stay whole.
        let mut huge = chat(30);
        assert_eq!(fit(&mut huge, 1), 30 - KEEP_WHOLE);
    }

    #[test]
    fn the_newest_pictures_go_after_their_calls_results() {
        let block = |id: &str, image: Option<&str>| {
            let mut b = json!({"type": "tool", "id": id, "name": "use_app", "status": "done", "output": "ok"});
            if let Some(file) = image {
                b["image"] =
                    json!({"id": file, "name": "WordCraft window.png", "mime": "image/png"});
            }
            b
        };
        let blocks: Vec<Value> = (0..5)
            .map(|n| block(&format!("c{n}"), Some(&format!("f{n}"))))
            .chain([block("c5", None)])
            .collect();
        let found = pictures(&blocks);
        assert_eq!(
            found.iter().map(|p| p.call_id.as_str()).collect::<Vec<_>>(),
            ["c2", "c3", "c4"]
        );
    }

    #[tokio::test]
    async fn a_picture_is_read_and_placed_after_the_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = files::path_of(dir.path(), 7, "f1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nxx").unwrap();
        let mut turns: Vec<Turn> = chat(2);
        let pictures = vec![
            Picture {
                call_id: "c0".into(),
                file_id: "f1".into(),
                name: "WordCraft window.png".into(),
                mime: "image/png".into(),
            },
            Picture {
                call_id: "c1".into(),
                file_id: "gone".into(),
                name: "x.png".into(),
                mime: "image/png".into(),
            },
        ];
        assert_eq!(show_pictures(&mut turns, &pictures, dir.path(), 7).await, 1);
        let Turn::User(user) = &turns[3] else {
            panic!("{:?}", turns[3])
        };
        assert!(user.text.contains("the picture call c0 took"));
        assert_eq!(user.files[0].mime, "image/png");
        assert_eq!(
            user.files[0].data,
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nxx")
        );
        assert!(matches!(turns[4], Turn::Assistant { .. }));
    }
}
