//! Replies run on the server, one per conversation at a time.
//!
//! A reply goes in steps. Each step streams the model's answer: the run
//! task batches what arrives and publishes it to the account's pages as
//! `ai.delta` events (`{conversationId, messageId, seq, text, reasoning}`:
//! text added to the current step since the previous delta, see
//! [`super::apply_delta`]; `seq` counts up from 1 within a run). When the
//! model asks for tools, their blocks are added and the message is published
//! whole (`ai.message` with `running` and `seq`); calls the owner let run
//! without asking run at once, then the next step starts. A call that needs
//! an answer ends the run with the message `waiting`; answering it
//! (`POST …/tools/{callId}`) starts a new run that goes on from there.
//!
//! The run keeps the reply's blocks for pages that load the conversation
//! meanwhile (`seq` tells which deltas they still need) and stores them at
//! every tool change and when it ends. Stopping marks the reply stopped at
//! once; the provider's thread ends at its next line.
//!
//! A step that fails in passing (a timeout, the network, the provider busy:
//! [`provider::ChatError::transient`]) is tried again after
//! [`Agent::retry_wait`], up to [`RETRY_TRIES`] times in a row; what it had
//! written is taken back first. While it waits the reply's message carries
//! `retry: {attempt, of, inMs, error}` (in `ai.message` and in the
//! conversation); Stop ends the wait, Retry now (`POST …/retry`) cuts it
//! short.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tokio::sync::{mpsc, Notify};

use super::host::Agent;
use super::{
    provider::{self, ChatError, ChatRequest, Delta, Turn},
    tools, Effective,
};
use crate::{audit::AuditLog, auth::AuthUser, db::now_ms};

/// How often streamed text is published.
const PUBLISH_EVERY: Duration = Duration::from_millis(120);
/// Tries again after a passing failure, in a row (a step that works starts
/// the count again).
pub const RETRY_TRIES: u32 = 10;
/// The wait before each try again.
pub const RETRY_WAIT: Duration = Duration::from_secs(30);

/// What a reply used so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub input: i64,
    pub output: i64,
    /// Input read back from the provider's cache (part of `input`).
    pub cache_read: i64,
    /// The last step's input and output: how full the model's context is.
    pub context: i64,
    /// Time spent writing (from the first word of each step to its end).
    pub writing_ms: i64,
}

impl Tally {
    /// A stored reply's, to go on from (after an answer to a tool request).
    pub fn of(message: &super::Message) -> Self {
        Self {
            input: message.input_tokens,
            output: message.output_tokens,
            cache_read: message.cache_read_tokens,
            context: message.context_tokens,
            writing_ms: message.writing_ms,
        }
    }

    /// Add one step.
    pub fn add(&mut self, usage: &provider::Usage, writing_ms: i64) {
        let input = usage.input.max(0);
        let output = usage.output.max(0);
        self.input += input;
        self.output += output;
        self.cache_read += usage.cache_read.clamp(0, input);
        self.context = input + output;
        self.writing_ms += writing_ms.max(0);
    }
}

/// A reply waiting to try its step again.
#[derive(Debug, Clone)]
pub struct Retry {
    /// This try again, from 1 to [`RETRY_TRIES`].
    pub attempt: u32,
    /// When it starts (ms).
    pub at: i64,
    /// What failed.
    pub error: String,
}

impl Retry {
    /// As pages get it: the time left rather than a time on the server's clock.
    pub fn json(&self, now: i64) -> Value {
        json!({
            "attempt": self.attempt,
            "of": RETRY_TRIES,
            "inMs": (self.at - now).max(0),
            "error": self.error,
        })
    }
}

/// What a running reply has so far.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub blocks: Vec<Value>,
    /// Deltas published so far in this run.
    pub seq: u64,
    /// Waiting to try the step again.
    pub retry: Option<Retry>,
}

#[derive(Debug, Default)]
pub struct Active {
    message_id: AtomicI64,
    cancel: Arc<AtomicBool>,
    stopped: Notify,
    /// Retry now: ends the wait before a try again.
    retry_now: Notify,
    snapshot: Mutex<Snapshot>,
    /// The blocks pages last heard (live events carry what changed since).
    published: Mutex<Vec<Value>>,
}

impl Active {
    pub fn message_id(&self) -> i64 {
        self.message_id.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Snapshot) -> T) -> T {
        let mut snapshot = match self.snapshot.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut snapshot)
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct Runs {
    active: Mutex<HashMap<String, Arc<Active>>>,
    /// Held while answering tool requests, sending, or stopping a waiting
    /// reply: one change to a waiting reply at a time.
    pub decide: tokio::sync::Mutex<()>,
    /// `use_app` calls waiting for the person's page.
    pub pages: super::use_app::Pages,
}

impl Runs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn get(&self, conversation_id: &str) -> Option<Arc<Active>> {
        self.active.lock().ok()?.get(conversation_id).cloned()
    }

    pub fn running(&self, conversation_id: &str) -> bool {
        self.get(conversation_id).is_some()
    }

    /// Claim the conversation for a new run (`None`: one is running).
    pub fn reserve(self: &Arc<Self>, conversation_id: &str) -> Option<Reservation> {
        let mut active = self.active.lock().ok()?;
        if active.contains_key(conversation_id) {
            return None;
        }
        let run = Arc::new(Active::default());
        active.insert(conversation_id.to_string(), run.clone());
        Some(Reservation {
            runs: self.clone(),
            conversation_id: conversation_id.to_string(),
            run,
            armed: true,
        })
    }

    /// Stop the conversation's reply. Returns whether one was running.
    pub fn stop(&self, conversation_id: &str) -> bool {
        match self.get(conversation_id) {
            Some(run) => {
                run.cancel.store(true, Ordering::Relaxed);
                run.stopped.notify_one();
                true
            }
            None => false,
        }
    }

    /// Try the waiting step again now. Returns whether one was waiting.
    pub fn retry_now(&self, conversation_id: &str) -> bool {
        match self.get(conversation_id) {
            Some(run) if run.snapshot().retry.is_some() => {
                run.retry_now.notify_one();
                true
            }
            _ => false,
        }
    }

    fn finish(&self, conversation_id: &str, run: &Arc<Active>) {
        if let Ok(mut active) = self.active.lock() {
            if active
                .get(conversation_id)
                .is_some_and(|current| Arc::ptr_eq(current, run))
            {
                active.remove(conversation_id);
            }
        }
    }
}

/// A claimed conversation; released when dropped before [`Reservation::start`].
pub struct Reservation {
    runs: Arc<Runs>,
    conversation_id: String,
    run: Arc<Active>,
    armed: bool,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            self.runs.finish(&self.conversation_id, &self.run);
        }
    }
}

/// Who and what a reply is for.
pub struct ReplyJob {
    pub owner: AuthUser,
    pub conversation_id: String,
    pub message_id: i64,
    pub provider_id: String,
    /// The provider's account answering (the provider itself, or one of
    /// its other accounts); another takes over when it reaches a limit.
    pub account_id: String,
    /// Accounts taken in this reply so far.
    pub tried: Vec<String>,
    pub model: String,
    /// Its messages: the conversation before this reply.
    pub request: ChatRequest,
    pub tools: tools::Available,
    /// Tools the owner let run without asking in this chat.
    pub allowed_tools: Vec<String>,
    /// The reply so far (empty for a new one).
    pub blocks: Vec<Value>,
    pub limits: Effective,
    /// Tokens the reply used before this run.
    pub tokens: Tally,
    /// The chat's title from its first message, while the model's first
    /// answer is to name the chat (see [`super::naming`]).
    pub naming: Option<String>,
}

impl Reservation {
    /// Write the reply (stored as message `job.message_id`) in the background.
    pub fn start(mut self, state: Agent, job: ReplyJob) {
        self.armed = false;
        self.run.message_id.store(job.message_id, Ordering::Relaxed);
        self.run.with(|s| s.blocks = job.blocks.clone());
        if let Ok(mut published) = self.run.published.lock() {
            *published = job.blocks.clone();
        }
        let runs = self.runs.clone();
        let run = self.run.clone();
        tokio::spawn(async move {
            let conversation_id = job.conversation_id.clone();
            reply(&state, &runs, &run, job).await;
            runs.finish(&conversation_id, &run);
        });
    }
}

enum End {
    Done,
    Waiting,
    Stopped,
    Failed(String),
}

enum Step {
    Answered {
        calls: Vec<provider::ToolCall>,
        usage: provider::Usage,
        quota: provider::limits::Quota,
        /// Time spent writing.
        ms: i64,
    },
    Stopped(provider::Usage, i64),
    Failed {
        error: ChatError,
        wrote: bool,
    },
}

async fn reply(state: &Agent, runs: &Runs, run: &Arc<Active>, mut job: ReplyJob) {
    let history = std::mem::take(&mut job.request.messages);
    let mut tokens = job.tokens;
    let mut tools_on = !job.request.tools.is_empty();
    // A reply runs until the model is done (or the owner stops it): the chat
    // is fitted into the model's window before each step.
    let budget = super::context::budget(&job.model, job.request.system.len());
    let mut pictures_on = super::context::sees_images(&job.model);
    // Tries again since the last step that worked.
    let mut retries = 0u32;
    let end = loop {
        if !run_allowed_tools(state, run, &job, tokens).await || run.cancelled() {
            break End::Stopped;
        }
        if !job.owner.is_admin() {
            if let Err(error) = super::check_allowance(&state.db, job.owner.id, &job.limits).await {
                break End::Failed(error.public_message());
            }
        }
        let mut request = job.request.clone();
        if !tools_on {
            request.tools.clear();
        }
        request.messages = history.clone();
        let before = run.snapshot().blocks;
        request
            .messages
            .extend(super::turns_of(&Value::Array(before.clone())));
        super::context::fit(&mut request.messages, budget);
        let pictures = if pictures_on && tools_on {
            super::context::show_pictures(
                &mut request.messages,
                &super::context::pictures(&before),
                state.data_dir(),
                job.owner.id,
            )
            .await
        } else {
            0
        };
        match stream_step(state, run, &job, request).await {
            Step::Stopped(usage, ms) => {
                count(state, &job, &mut tokens, usage, ms).await;
                break End::Stopped;
            }
            Step::Failed { error, wrote } if error.limit.is_some() => {
                let limit = error.limit.clone().unwrap_or_else(|| unreachable!());
                let until =
                    super::accounts::rest(&state.db, &job.account_id, &limit, now_ms()).await;
                if !wrote && next_account(state, &mut job).await {
                    tracing::info!(provider = %job.provider_id, account = %job.account_id, reason = %limit.reason, "AI account reached a limit; the next account answers");
                    continue;
                }
                break End::Failed(limited(state, &job, error.message, until).await);
            }
            Step::Failed { error, wrote } if error.tools_refused && tools_on && !wrote => {
                tracing::info!(model = %job.model, error = %error.message, "AI model refused tools; answering without them");
                tools_on = false;
            }
            Step::Failed { error, wrote } if pictures > 0 && !wrote && !error.transient => {
                tracing::info!(model = %job.model, error = %error.message, "AI model refused the window pictures; going on without them");
                pictures_on = false;
            }
            Step::Failed { error, wrote } if error.transient && retries < RETRY_TRIES => {
                retries += 1;
                tracing::info!(provider = %job.provider_id, attempt = retries, error = %error.message, "AI reply failed in passing; trying again");
                if wrote {
                    // What the failed step wrote goes; the try writes it anew.
                    run.with(|s| s.blocks = before);
                }
                if !wait_to_retry(state, run, &job, tokens, retries, &error.message).await {
                    break End::Stopped;
                }
            }
            Step::Failed { error, .. } if error.transient => {
                break End::Failed(format!(
                    "{} (tried again {RETRY_TRIES} times, {} seconds apart)",
                    error.message.trim_end_matches('.'),
                    state.retry_wait.as_secs()
                ))
            }
            Step::Failed { error, .. } => break End::Failed(error.message),
            Step::Answered {
                calls,
                usage,
                quota,
                ms,
            } => {
                retries = 0;
                super::accounts::store_quota(&state.db, &job.account_id, &quota).await;
                count(state, &job, &mut tokens, usage, ms).await;
                if let Some(title) = job.naming.take() {
                    super::naming::start(
                        state,
                        &job,
                        title,
                        &history,
                        &calls,
                        &run.snapshot().blocks,
                    );
                }
                if calls.is_empty() {
                    break End::Done;
                }
                let waiting = run.with(|s| {
                    for call in &calls {
                        let status = if !job.tools.has(&call.name) {
                            "error"
                        } else if !tools::asks_first(&call.name, &call.input)
                            || (tools::may_skip_asking(&call.name)
                                && job.allowed_tools.iter().any(|t| t == &call.name))
                        {
                            "allowed"
                        } else {
                            "pending"
                        };
                        let mut block = super::tool_block(call, status);
                        if status == "error" {
                            block["error"] = json!(if tools::is_tool(&call.name) {
                                format!("the tool {} is not available to this account", call.name)
                            } else {
                                format!("there is no tool called {:?}", call.name)
                            });
                        }
                        s.blocks.push(block);
                    }
                    s.blocks.iter().any(|b| b["status"] == "pending")
                });
                if waiting {
                    break End::Waiting;
                }
                progress(state, run, &job, tokens).await;
            }
        }
    };

    let (status, error) = match end {
        End::Done => ("done", String::new()),
        End::Waiting => ("waiting", String::new()),
        End::Stopped => ("stopped", String::new()),
        End::Failed(error) => ("error", error),
    };
    if status != "waiting" {
        // Calls that will not run now.
        run.with(|s| {
            for block in s.blocks.iter_mut().filter(|b| super::is_tool_block(b)) {
                if matches!(
                    block["status"].as_str(),
                    Some("pending" | "allowed" | "running")
                ) {
                    block["status"] = json!("skipped");
                }
            }
        });
    }
    let content = Value::Array(run.snapshot().blocks);
    let finished = (status != "waiting").then(now_ms);
    if let Err(e) = store(&state.db, &job, &content, status, &error, tokens, finished).await {
        tracing::warn!(error = %e, "could not store an AI reply");
    }
    // Stored and counted: the conversation takes the next message now
    // (before pages hear the reply ended and send one).
    runs.finish(&job.conversation_id, run);
    publish_message(state, run, &job, false, 0, None).await;
    if let Ok(Some(conversation)) = super::conversation(&state.db, &job.conversation_id).await {
        state.host.publish_user_and_admin(
            "ai.conversation",
            job.owner.id,
            json!({ "conversation": conversation.json(Some(&job.owner.username), false) }),
        );
    }
}

/// Whether a call works in an app's window on the server: `use_app`, and
/// `open_app` of an app that takes commands.
fn server_call(available: &tools::Available, block: &Value) -> bool {
    let app = block["input"]["app"].as_str().unwrap_or("").trim();
    match block["name"].as_str().unwrap_or("") {
        tools::USE_APP => true,
        tools::OPEN_APP => available.apps.iter().any(|a| a.id == app && a.commands),
        _ => false,
    }
}

/// Run the calls the owner allowed, in order. False when stopped.
async fn run_allowed_tools(
    state: &Agent,
    run: &Arc<Active>,
    job: &ReplyJob,
    tokens: Tally,
) -> bool {
    loop {
        if run.cancelled() {
            return false;
        }
        // The apps' windows on the server do the app's part: the person's
        // page leaves those calls alone.
        let on_server = job.tools.server_apps && job.tools.has(tools::USE_APP);
        let next = run.with(|s| {
            let at = s
                .blocks
                .iter()
                .position(|b| super::is_tool_block(b) && b["status"] == "allowed")?;
            s.blocks[at]["status"] = json!("running");
            if on_server && server_call(&job.tools, &s.blocks[at]) {
                s.blocks[at]["server"] = json!(true);
            }
            Some((
                at,
                s.blocks[at]["name"].as_str().unwrap_or("").to_string(),
                s.blocks[at]["input"].clone(),
                s.blocks[at]["id"].as_str().unwrap_or("").to_string(),
            ))
        });
        let Some((at, name, input, call_id)) = next else {
            return true;
        };
        // A call the person's page runs waits for its answer from now on: the
        // page may answer as soon as it sees the call running.
        let page =
            (name == tools::USE_APP && job.tools.has(tools::USE_APP) && !on_server).then(|| {
                state
                    .ai
                    .pages
                    .wait(&job.conversation_id, &call_id, job.owner.id)
            });
        progress(state, run, job, tokens).await;
        let outcome = tokio::select! {
            biased;
            _ = run.stopped.notified() => None,
            result = async {
                match page {
                    Some(answer) => super::use_app::run(state, &job.owner, &job.tools, &job.conversation_id, &input, answer).await,
                    None if on_server && name == tools::USE_APP => super::app_window::use_app(state, &job.owner, &job.tools, &job.conversation_id, &input).await,
                    None => tools::execute(state, &job.owner, &job.tools, &job.conversation_id, &name, &input).await,
                }
            } => Some(result),
        };
        if name == tools::USE_APP {
            state.ai.pages.forget(&job.conversation_id, &call_id);
        }
        let stopped = outcome.is_none();
        let failed = !matches!(outcome, Some(Ok(_)));
        // The file the app's window on the server has open, for the card.
        let document = (on_server
            && server_call(&job.tools, &json!({"name": name, "input": input})))
        .then(|| input.get("app").and_then(Value::as_str))
        .flatten()
        .and_then(|app| super::app_window::document(&job.conversation_id, app));
        run.with(|s| {
            let block = &mut s.blocks[at];
            if let Some(path) = &document {
                block["document"] = json!(path);
            }
            match outcome {
                Some(Ok(output)) => {
                    block["status"] = json!("done");
                    block["output"] = json!(tools::cap(
                        &output.text,
                        tools::MAX_RESULT,
                        name == tools::RUN_COMMAND
                    ));
                    if let Some(image) = output.image {
                        block["image"] = image;
                    }
                }
                Some(Err(error)) => {
                    block["status"] = json!("error");
                    block["error"] = json!(tools::cap(&error, 4000, false));
                }
                None => {
                    block["status"] = json!("error");
                    block["error"] = json!("Stopped while it ran.");
                }
            }
        });
        let mut log = AuditLog::new(format!("ai.tool.{name}"))
            .actor(&job.owner)
            .object("ai_conversation", &job.conversation_id)
            .path(tools::summary(&name, &input));
        log = if failed { log.failure() } else { log.success() };
        state.host.audit(log).await;
        progress(state, run, job, tokens).await;
        if stopped {
            return false;
        }
    }
}

/// One model call: stream its answer into the current step.
async fn stream_step(
    state: &Agent,
    run: &Arc<Active>,
    job: &ReplyJob,
    request: ChatRequest,
) -> Step {
    let prompt_chars =
        request.system.len() + request.messages.iter().map(Turn::chars).sum::<usize>();
    let (tx, mut rx) = mpsc::unbounded_channel::<Delta>();
    let cancel = run.cancel.clone();
    let started = std::time::Instant::now();
    // The first word: writing starts (the wait before it is reading).
    let mut first: Option<std::time::Instant> = None;
    let mut reading = tokio::task::spawn_blocking(move || {
        provider::stream_chat(&request, &cancel, &mut |delta| {
            let _ = tx.send(delta);
        })
    });
    let mut wrote = 0usize;
    let mut pending_text = String::new();
    let mut pending_reasoning = String::new();
    let mut ticker = tokio::time::interval(PUBLISH_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let publish = |pending_text: &mut String, pending_reasoning: &mut String| {
        if pending_text.is_empty() && pending_reasoning.is_empty() {
            return;
        }
        let seq = run.with(|s| {
            super::apply_delta(&mut s.blocks, pending_text, pending_reasoning);
            s.seq += 1;
            s.seq
        });
        state.host.publish_user(
            "ai.delta",
            job.owner.id,
            json!({
                "conversationId": job.conversation_id,
                "messageId": job.message_id,
                "seq": seq,
                "text": pending_text,
                "reasoning": pending_reasoning,
            }),
        );
        pending_text.clear();
        pending_reasoning.clear();
    };
    let result = loop {
        tokio::select! {
            biased;
            _ = run.stopped.notified() => break None,
            delta = rx.recv() => match delta {
                Some(Delta::Text(t)) => { first.get_or_insert_with(std::time::Instant::now); wrote += t.len(); pending_text.push_str(&t); }
                Some(Delta::Reasoning(r)) => { first.get_or_insert_with(std::time::Instant::now); wrote += r.len(); pending_reasoning.push_str(&r); }
                None => {
                    // The reader is done (its sender dropped): take its result.
                    break Some((&mut reading).await.unwrap_or_else(|e| {
                        Err(format!("the reply stopped unexpectedly: {e}").into())
                    }));
                }
            },
            _ = ticker.tick() => publish(&mut pending_text, &mut pending_reasoning),
        }
    };
    publish(&mut pending_text, &mut pending_reasoning);
    let ms = first.unwrap_or(started).elapsed().as_millis() as i64;
    // Tokens the provider reported; otherwise about four characters each,
    // when anything was written (a refused request costs nothing).
    let tokens = |usage: provider::Usage, extra: usize| {
        if !usage.is_empty() {
            usage
        } else if wrote + extra > 0 {
            provider::Usage {
                input: (prompt_chars / 4) as i64,
                output: ((wrote + extra) / 4).max(1) as i64,
                ..Default::default()
            }
        } else {
            provider::Usage::default()
        }
    };
    match result {
        None => Step::Stopped(tokens(provider::Usage::default(), 0), ms),
        Some(Ok(outcome)) if run.cancelled() => Step::Stopped(tokens(outcome.usage, 0), ms),
        Some(Ok(outcome)) => {
            let call_chars: usize = outcome
                .calls
                .iter()
                .map(|c| c.name.len() + c.input.to_string().len())
                .sum();
            Step::Answered {
                usage: tokens(outcome.usage, call_chars),
                calls: outcome.calls,
                quota: outcome.quota,
                ms,
            }
        }
        Some(Err(error)) => Step::Failed {
            error,
            wrote: wrote > 0,
        },
    }
}

async fn count(
    state: &Agent,
    job: &ReplyJob,
    tokens: &mut Tally,
    usage: provider::Usage,
    writing_ms: i64,
) {
    if usage.is_empty() {
        return;
    }
    tokens.add(&usage, writing_ms);
    match super::record_usage(
        &state.db,
        job.owner.id,
        &job.provider_id,
        &job.account_id,
        &job.model,
        &usage,
    )
    .await
    {
        // The reply's estimate grows with each step (also across a pause
        // for a tool answer).
        Ok(estimate) if estimate > 0 => {
            let added = sqlx::query(
                "UPDATE ai_messages SET estimated_cost = estimated_cost + ? WHERE id = ?",
            )
            .bind(estimate)
            .bind(job.message_id)
            .execute(&state.db)
            .await;
            if let Err(e) = added {
                tracing::warn!(error = %e, "could not store an AI reply's estimate");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not record AI usage"),
    }
}

/// Go on with the provider's next account that is not resting (after a
/// limit). False when none is left.
async fn next_account(state: &Agent, job: &mut ReplyJob) -> bool {
    let Ok(Some(provider)) = super::provider(&state.db, &job.provider_id).await else {
        return false;
    };
    match super::accounts::pick(&state.db, &state.secrets, &provider, &mut job.tried, false).await {
        Ok(Some(picked)) => {
            job.account_id = picked.account.id;
            job.request.api_key = picked.api_key;
            job.request.account = picked.account_ref;
            true
        }
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(error = %error.public_message(), "no other AI account could take over");
            false
        }
    }
}

/// The refusal of a provider whose accounts all reached their limits.
async fn limited(state: &Agent, job: &ReplyJob, message: String, until: i64) -> String {
    let Ok(Some(provider)) = super::provider(&state.db, &job.provider_id).await else {
        return message;
    };
    let accounts = super::accounts::of(&state.db, &provider)
        .await
        .unwrap_or_default();
    if accounts.len() < 2 {
        return message;
    }
    let now = now_ms();
    let back = super::accounts::back_soonest(&accounts, now).unwrap_or(until);
    format!(
        "{message} All {} accounts of {} reached their limits; the next one is back {}.",
        accounts.len(),
        provider.name,
        super::accounts::in_words(back - now)
    )
}

/// Wait [`Agent::retry_wait`] before try again number `attempt`, showing it.
/// False when the reply was stopped meanwhile.
async fn wait_to_retry(
    state: &Agent,
    run: &Arc<Active>,
    job: &ReplyJob,
    tokens: Tally,
    attempt: u32,
    error: &str,
) -> bool {
    let wait = state.retry_wait;
    run.with(|s| {
        s.retry = Some(Retry {
            attempt,
            at: now_ms() + wait.as_millis() as i64,
            error: error.to_string(),
        })
    });
    progress(state, run, job, tokens).await;
    let go_on = tokio::select! {
        biased;
        _ = run.stopped.notified() => false,
        _ = run.retry_now.notified() => true,
        _ = tokio::time::sleep(wait) => true,
    } && !run.cancelled();
    let seq = run.with(|s| {
        s.retry = None;
        s.seq
    });
    if go_on {
        publish_message(state, run, job, true, seq, None).await;
    }
    go_on
}

/// Store the reply as it is (still being written) and show it.
async fn progress(state: &Agent, run: &Arc<Active>, job: &ReplyJob, tokens: Tally) {
    let (content, seq, retry) =
        run.with(|s| (Value::Array(s.blocks.clone()), s.seq, s.retry.clone()));
    if let Err(e) = store(&state.db, job, &content, "streaming", "", tokens, None).await {
        tracing::warn!(error = %e, "could not store an AI reply");
    }
    publish_message(state, run, job, true, seq, retry.as_ref()).await;
}

/// Tell pages about the reply: a long one only by the blocks that changed
/// since they last heard ([`super::live_message`]).
async fn publish_message(
    state: &Agent,
    run: &Arc<Active>,
    job: &ReplyJob,
    running: bool,
    seq: u64,
    retry: Option<&Retry>,
) {
    if let Ok(Some(message)) = super::message(&state.db, job.message_id).await {
        let mut message = message.json();
        let content = message["content"].as_array().cloned().unwrap_or_default();
        let from = match run.published.lock() {
            Ok(mut published) => {
                let same = published
                    .iter()
                    .zip(&content)
                    .take_while(|(a, b)| a == b)
                    .count();
                *published = content;
                same
            }
            Err(_) => 0,
        };
        if let Some(retry) = retry {
            message["retry"] = retry.json(now_ms());
        }
        let message = super::live_message(message, from);
        state.host.publish_user_and_admin(
            "ai.message",
            job.owner.id,
            json!({"conversationId": job.conversation_id, "message": message, "running": running, "seq": seq}),
        );
    }
}

async fn store(
    pool: &SqlitePool,
    job: &ReplyJob,
    content: &Value,
    status: &str,
    error: &str,
    tokens: Tally,
    finished_at: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ai_messages SET content = ?, status = ?, error = ?, input_tokens = ?, output_tokens = ?,
         cache_read_tokens = ?, context_tokens = ?, writing_ms = ?, finished_at = ? WHERE id = ?",
    )
    .bind(content.to_string())
    .bind(status)
    .bind(error)
    .bind(tokens.input)
    .bind(tokens.output)
    .bind(tokens.cache_read)
    .bind(tokens.context)
    .bind(tokens.writing_ms)
    .bind(finished_at)
    .bind(job.message_id)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE ai_conversations SET updated_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(&job.conversation_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replies that were being written when RAST stopped (replies waiting for
/// an answer stay waiting).
pub async fn recover(pool: &SqlitePool) {
    let rows = match sqlx::query("SELECT id, content FROM ai_messages WHERE status = 'streaming'")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not read interrupted AI replies");
            return;
        }
    };
    for row in rows {
        let id: i64 = row.get("id");
        let mut content: Value =
            serde_json::from_str(&row.get::<String, _>("content")).unwrap_or_else(|_| json!([]));
        if let Some(blocks) = content.as_array_mut() {
            for block in blocks.iter_mut().filter(|b| super::is_tool_block(b)) {
                match block["status"].as_str() {
                    Some("running") => {
                        block["status"] = json!("error");
                        block["error"] = json!("Interrupted: RAST restarted while it ran.");
                    }
                    Some("pending" | "allowed") => block["status"] = json!("skipped"),
                    _ => {}
                }
            }
        }
        let result = sqlx::query(
            "UPDATE ai_messages SET status = 'error', content = ?,
             error = 'Interrupted: RAST restarted while this reply was written.', finished_at = ?
             WHERE id = ?",
        )
        .bind(content.to_string())
        .bind(now_ms())
        .bind(id)
        .execute(pool)
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "could not mark an interrupted AI reply");
        }
    }
}
