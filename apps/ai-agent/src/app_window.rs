//! The apps' windows on the server: `use_app` works in a window of the app
//! that AI Agent keeps on the server, so the work goes on when the person
//! closes RAST (a reply of hours keeps working in WordCraft without their
//! browser).
//!
//! * One window per chat and app: a headless Chrome boxed in as the browser
//!   tool's ([`browser::launch`]), with a software GPU for the app's canvas.
//!   Its only network is a proxy that reaches the public web and RAST's web
//!   page apps' files at RAST's own address (`/api/appweb/…`, which RAST
//!   serves by an unguessable key alone; nothing else of RAST or the server).
//! * Its page ([`HOST_PAGE`], `app_window.js`), served in place by DevTools
//!   at RAST's address so the app's page may be framed by it, plays the RAST
//!   window's part as the person's browser does: it frames the app's page as
//!   RAST's window does (sandboxed in an opaque origin, RAST's scripts in the
//!   page), hands it documents, keeps what the page stores in the account's
//!   storage (through the host API) and takes what it saves.
//! * Documents come from and go to the person's files through the host API,
//!   with their rights. Save writes the open file; Save As and exports go
//!   into the open file's folder (Home for a new document) under the name the
//!   app gives, a name of their own (`name (2).ext`) beside a file already
//!   there that this window did not write.
//! * A window stays while its chat replies and [`IDLE`] after (what it did
//!   not save goes with it), at most [`MAX_WINDOWS`] on the server. A window
//!   that stopped starts again with its file as last saved.
//!
//! RAST 0.9.21 (host API 2) serves what these windows need; with an older
//! RAST `use_app` works in the person's page as before ([`super::use_app`]).

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, LazyLock, Mutex, Once, Weak,
    },
    time::Duration,
};

use axum::http::Method as HttpMethod;
use base64::Engine as _;
use serde_json::{json, Value};

use super::browser::{self, Cdp, Launch};
use super::host::{Agent, HostApp};
use super::run::Runs;
use super::tools::{Available, Output};
use super::use_app;
use crate::auth::AuthUser;

/// App windows open at a time on the server (a Chrome each).
pub const MAX_WINDOWS: usize = 3;
/// A window nobody used for this long closes (unless its chat is replying).
pub const IDLE: Duration = Duration::from_secs(30 * 60);
/// A window nobody used for this long rests: its page is frozen (the app
/// draws nothing and runs no timers) until the next call.
const REST: Duration = Duration::from_secs(60);
/// The longest a window's Chrome runs.
const LIFETIME_SECS: u32 = 12 * 3600;
/// How long the app may take to start (its files are tens of megabytes).
const START_LIMIT: Duration = Duration::from_secs(150);
/// How long one method may take once the app runs.
const REQUEST_LIMIT: Duration = Duration::from_secs(90);
/// How long the app may take to store what it saved after a method.
const SAVE_LIMIT: Duration = Duration::from_secs(120);
/// The largest document a window opens or saves.
const MAX_DOCUMENT: usize = 64 * 1024 * 1024;
/// Where the window's page is, at RAST's address (no key is ever this).
const HOST_PATH: &str = "/api/appweb/rast-agent-window";
/// The window's page.
const HOST_SCRIPT: &str = include_str!("app_window.js");
/// What the window's page may do: frame RAST's app pages, run its script.
const HOST_CSP: &str =
    "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-src 'self'";
/// Chrome's flags for an app window: a software GPU for the app's canvas.
const FLAGS: &[&str] = &[
    "--enable-unsafe-swiftshader",
    "--use-angle=swiftshader",
    "--ignore-gpu-blocklist",
    "--disable-renderer-backgrounding",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
];

/// The window's page: the app's frame fills it.
pub fn host_page() -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>RAST app window</title>\
<style>html,body{{margin:0;height:100%;overflow:hidden;background:#fff}}iframe{{display:block;border:0;width:100%;height:100%}}</style>\
</head><body><script>{HOST_SCRIPT}</script></body></html>"
    )
}

/// Whether `use_app` works in windows on the server here: RAST serves what
/// they need and Chrome starts (as for the browser tool).
pub async fn available(state: &Agent) -> bool {
    browser::chrome().is_some()
        && state.host.local_address().is_some()
        && state.host.terminal_ready().await
}

// ------------------------------------------------------------ the windows

struct Window {
    cdp: Arc<Cdp>,
    /// The DevTools session of the window's page.
    page: String,
    app: HostApp,
    owner: AuthUser,
    conversation: String,
    /// The document the app shows now.
    path: Mutex<Option<String>>,
    /// Documents it opened or saved, by the name the app knows them by.
    documents: Mutex<HashMap<String, String>>,
    /// Files this window wrote (Save As may replace them).
    written: Mutex<HashSet<String>>,
    /// What happened since the last call (saves, the app's words).
    notes: Mutex<Vec<String>>,
    dirty: AtomicBool,
    /// The page stopped (crashed or closed).
    gone: AtomicBool,
    /// The page is frozen ([`REST`]).
    frozen: AtomicBool,
    last_used: AtomicI64,
    busy: tokio::sync::Mutex<()>,
    /// Saves are stored one after another.
    saving: tokio::sync::Mutex<()>,
}

type Key = (String, String);

static WINDOWS: LazyLock<Mutex<HashMap<Key, Arc<Window>>>> = LazyLock::new(Default::default);
/// The document each chat's window of an app had open, for a window that
/// starts again.
static LAST_PATHS: LazyLock<Mutex<HashMap<Key, String>>> = LazyLock::new(Default::default);
static REAPER: Once = Once::new();

fn windows() -> std::sync::MutexGuard<'static, HashMap<Key, Arc<Window>>> {
    WINDOWS.lock().unwrap_or_else(|e| e.into_inner())
}

fn last_paths() -> std::sync::MutexGuard<'static, HashMap<Key, String>> {
    LAST_PATHS.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Window {
    fn touch(&self) {
        self.last_used.store(crate::db::now_ms(), Ordering::Relaxed);
    }

    fn alive(&self) -> bool {
        !self.cdp.is_closed() && !self.gone.load(Ordering::Relaxed)
    }

    fn in_use(&self) -> bool {
        self.busy.try_lock().is_err() || self.saving.try_lock().is_err()
    }

    /// Freeze the page: an idle app draws nothing (it would redraw all the
    /// time, a tenth of a core).
    async fn rest(&self) {
        if self.frozen.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = self
            .evaluate("window.__rast.flush()".into(), Duration::from_secs(10))
            .await;
        let frozen = self
            .cdp
            .call(
                Some(&self.page),
                "Page.setWebLifecycleState",
                json!({"state": "frozen"}),
            )
            .await;
        if frozen.is_err() {
            self.frozen.store(false, Ordering::Relaxed);
        }
    }

    /// The page runs again (before a call).
    async fn wake(&self) {
        if self.frozen.swap(false, Ordering::Relaxed) {
            let _ = self
                .cdp
                .call(
                    Some(&self.page),
                    "Page.setWebLifecycleState",
                    json!({"state": "active"}),
                )
                .await;
        }
    }

    fn note(&self, text: String) {
        let mut notes = lock(&self.notes);
        if notes.len() < 50 {
            notes.push(text);
        }
    }

    fn take_notes(&self) -> String {
        let notes = std::mem::take(&mut *lock(&self.notes));
        notes.iter().map(|n| format!("{n}\n")).collect::<String>()
    }

    /// Run JavaScript in the window's page: its value, or what it threw.
    async fn evaluate(&self, expression: String, limit: Duration) -> Result<Value, String> {
        let answer = self
            .cdp
            .call_for(
                Some(&self.page),
                "Runtime.evaluate",
                json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
                limit,
            )
            .await?;
        if let Some(details) = answer.get("exceptionDetails") {
            let said = details["exception"]["description"]
                .as_str()
                .or_else(|| details["exception"]["value"].as_str())
                .or_else(|| details["text"].as_str())
                .unwrap_or("the app's window failed");
            let line = said.lines().next().unwrap_or(said);
            return Err(line.strip_prefix("Error: ").unwrap_or(line).to_string());
        }
        Ok(answer["result"]["value"].clone())
    }

    /// One method of the app's page command API.
    async fn request(&self, method: &str, params: &Value) -> Result<Value, String> {
        let expression = format!(
            "window.__rast.request({}, {}, {}, {})",
            json!(method),
            params,
            START_LIMIT.as_millis(),
            REQUEST_LIMIT.as_millis()
        );
        self.evaluate(
            expression,
            START_LIMIT + REQUEST_LIMIT + SAVE_LIMIT + Duration::from_secs(30),
        )
        .await
    }

    /// Hand the app the person's file at `path`.
    async fn open(&self, state: &Agent, path: &str) -> Result<(), String> {
        let bytes = state.host.file_bytes(&self.owner, path).await?;
        if bytes.len() > MAX_DOCUMENT {
            return Err("the file is larger than 64 MB".into());
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        drop(bytes);
        self.evaluate(
            format!(
                "window.__rast.open({}, \"{data}\", {})",
                json!(name),
                START_LIMIT.as_millis()
            ),
            START_LIMIT + Duration::from_secs(30),
        )
        .await?;
        *lock(&self.path) = Some(path.to_string());
        lock(&self.documents).insert(name, path.to_string());
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Answer one of the page's asking messages.
    async fn answer(&self, call: u64, result: Result<Value, String>) {
        let (ok, value) = match result {
            Ok(value) => (true, value),
            Err(error) => (false, Value::String(error)),
        };
        let _ = self
            .evaluate(
                format!("window.__rast.answer({call}, {ok}, {value})"),
                Duration::from_secs(60),
            )
            .await;
    }
}

/// Close a chat's app windows (the chat is being deleted).
pub fn close(conversation: &str) {
    let gone: Vec<Arc<Window>> = {
        let mut map = windows();
        let keys: Vec<Key> = map
            .keys()
            .filter(|(c, _)| c == conversation)
            .cloned()
            .collect();
        keys.iter().filter_map(|k| map.remove(k)).collect()
    };
    last_paths().retain(|(c, _), _| c != conversation);
    drop(gone);
}

/// Every half minute, close the windows nobody used for [`IDLE`] whose chat
/// is not replying and those that stopped, and rest the idle ones.
fn start_reaper(runs: Arc<Runs>) {
    REAPER.call_once(|| {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let now = crate::db::now_ms();
                let idle = IDLE.as_millis() as i64;
                let gone: Vec<Arc<Window>> = {
                    let mut map = windows();
                    let keys: Vec<Key> = map
                        .iter()
                        .filter(|((conversation, _), w)| {
                            !w.alive()
                                || (now - w.last_used.load(Ordering::Relaxed) > idle
                                    && !w.in_use()
                                    && !runs.running(conversation))
                        })
                        .map(|(k, _)| k.clone())
                        .collect();
                    keys.iter().filter_map(|k| map.remove(k)).collect()
                };
                for window in &gone {
                    if window.alive() {
                        window.wake().await;
                        let _ = window
                            .evaluate("window.__rast.flush()".into(), Duration::from_secs(10))
                            .await;
                    }
                }
                drop(gone);
                let resting: Vec<Arc<Window>> = windows()
                    .values()
                    .filter(|w| {
                        w.alive()
                            && !w.in_use()
                            && now - w.last_used.load(Ordering::Relaxed) > REST.as_millis() as i64
                    })
                    .cloned()
                    .collect();
                for window in resting {
                    window.rest().await;
                }
            }
        });
    });
}

/// Make room for one more window: close the one used longest ago that is
/// not working.
fn make_room() -> Result<(), String> {
    let mut map = windows();
    map.retain(|_, w| w.alive());
    if map.len() < MAX_WINDOWS {
        return Ok(());
    }
    let oldest = map
        .iter()
        .filter(|(_, w)| !w.in_use())
        .min_by_key(|(_, w)| w.last_used.load(Ordering::Relaxed))
        .map(|(k, _)| k.clone());
    match oldest {
        Some(key) => {
            map.remove(&key);
            Ok(())
        }
        None => Err(format!(
            "the server's app windows are all working ({MAX_WINDOWS} at a time); try again in a moment"
        )),
    }
}

/// The chat's window of the app, started (and its last document opened
/// again) when it has none: the window, and a note when it started again.
async fn window_for(
    state: &Agent,
    user: &AuthUser,
    conversation: &str,
    app: &HostApp,
    reopen: bool,
) -> Result<(Arc<Window>, String), String> {
    start_reaper(state.ai.clone());
    let key: Key = (conversation.to_string(), app.id.clone());
    let open = windows().get(&key).filter(|w| w.alive()).cloned();
    if let Some(window) = open {
        return Ok((window, String::new()));
    }
    make_room()?;
    let window = start(state, user, conversation, app).await?;
    windows().insert(key.clone(), window.clone());
    let mut note = String::new();
    let last = last_paths().get(&key).cloned();
    if let (true, Some(path)) = (reopen, last) {
        match window.open(state, &path).await {
            Ok(()) => {
                note = format!(
                    "{}'s window on the server started again (the last one closed): it has {path} open as last saved.\n",
                    app.name
                )
            }
            Err(error) => {
                note = format!(
                    "{}'s window on the server started again (the last one closed); {path} did not open: {error}\n",
                    app.name
                )
            }
        }
    }
    Ok((window, note))
}

/// RAST's address as an origin.
fn origin(rast: SocketAddr) -> String {
    format!("http://{rast}")
}

async fn start(
    state: &Agent,
    user: &AuthUser,
    conversation: &str,
    app: &HostApp,
) -> Result<Arc<Window>, String> {
    let rast = state
        .host
        .local_address()
        .ok_or("this RAST runs no app windows on the server")?;
    let page_path = state
        .host
        .app_page(user, &app.id)
        .await
        .ok_or_else(|| format!("{} is not a web page app this account may open", app.name))?;
    let launch = Launch {
        unit: "rast-app-window",
        description: "RAST app window",
        proxy_port: browser::app_proxy_port(rast).await?,
        memory: "3G",
        lifetime_secs: LIFETIME_SECS,
        // A share, not a cap: the app's WebAssembly compiles on every core
        // at once, and a cap stalls its drawing with it; the window gives
        // way to everything else on a busy server.
        cpu: "CPUWeight=20",
        window: (1440, 900),
        flags: FLAGS,
        max_message: 2 * MAX_DOCUMENT + 32 * 1024 * 1024,
    };
    let cdp = browser::launch(state, user, &launch).await?;
    let events = cdp.events();
    let (target, page) = prepare(&cdp, rast)
        .await
        .map_err(|e| browser::started_wrong(&cdp, &e))?;
    let window = Arc::new(Window {
        cdp,
        page,
        app: app.clone(),
        owner: user.clone(),
        conversation: conversation.to_string(),
        path: Mutex::new(None),
        documents: Mutex::new(HashMap::new()),
        written: Mutex::new(HashSet::new()),
        notes: Mutex::new(Vec::new()),
        dirty: AtomicBool::new(false),
        gone: AtomicBool::new(false),
        frozen: AtomicBool::new(false),
        last_used: AtomicI64::new(0),
        busy: tokio::sync::Mutex::new(()),
        saving: tokio::sync::Mutex::new(()),
    });
    window.touch();
    tokio::spawn(listen(
        Arc::downgrade(&window),
        events,
        state.clone(),
        origin(rast),
        target,
    ));
    window
        .cdp
        .call(
            Some(&window.page),
            "Page.navigate",
            json!({"url": format!("{}{HOST_PATH}", origin(rast))}),
        )
        .await?;
    let mut ready = false;
    for _ in 0..100 {
        if window
            .evaluate(
                "typeof window.__rast === 'object' && document.readyState !== 'loading'".into(),
                Duration::from_secs(5),
            )
            .await
            .is_ok_and(|v| v == true)
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !ready {
        return Err(format!("{}'s window on the server did not start", app.name));
    }
    let local = local_items(state, user, &app.id).await;
    let config = json!({
        "page": format!("{}{page_path}", origin(rast)),
        "appId": app.id,
        "name": app.name,
        "local": local,
    });
    window
        .evaluate(
            format!("window.__rast.start({config})"),
            Duration::from_secs(30),
        )
        .await?;
    Ok(window)
}

/// The account's localStorage of the app (none when RAST does not answer).
async fn local_items(state: &Agent, user: &AuthUser, app_id: &str) -> Value {
    match state
        .host
        .web_storage(
            user,
            app_id,
            HttpMethod::GET,
            "/local",
            Vec::new(),
            "application/json",
        )
        .await
    {
        Ok((status, _, body)) if status.is_success() => serde_json::from_slice::<Value>(&body)
            .ok()
            .map(|answer| answer["items"].clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

/// The window's tab, ready: its page served in place, the binding to AI
/// Agent installed.
async fn prepare(cdp: &Cdp, rast: SocketAddr) -> Result<(String, String), String> {
    let _ = cdp
        .call(
            None,
            "Browser.setDownloadBehavior",
            json!({"behavior": "deny"}),
        )
        .await;
    cdp.call(None, "Target.setDiscoverTargets", json!({"discover": true}))
        .await?;
    let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
    let existing = targets["targetInfos"].as_array().and_then(|list| {
        list.iter()
            .find(|t| t["type"] == "page")
            .and_then(|t| t["targetId"].as_str())
            .map(str::to_string)
    });
    let target = match existing {
        Some(id) => id,
        None => cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await?["targetId"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    };
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
        )
        .await?;
    let page = attached["sessionId"]
        .as_str()
        .ok_or("the window has no tab")?
        .to_string();
    cdp.call(Some(&page), "Page.enable", json!({})).await?;
    cdp.call(Some(&page), "Runtime.enable", json!({})).await?;
    cdp.call(
        Some(&page),
        "Runtime.addBinding",
        json!({"name": "rastAgent"}),
    )
    .await?;
    cdp.call(
        Some(&page),
        "Fetch.enable",
        json!({"patterns": [{"urlPattern": format!("{}{HOST_PATH}*", origin(rast)), "requestStage": "Request"}]}),
    )
    .await?;
    Ok((target, page))
}

/// The window's events: its page asked for, what the page tells AI Agent,
/// and the page stopping.
async fn listen(
    window: Weak<Window>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<Value>,
    state: Agent,
    origin: String,
    target: String,
) {
    // The window's own page's scripts (its top frame's main world): only
    // they speak for the window, never the app's frame.
    let mut own_contexts: HashSet<i64> = HashSet::new();
    while let Some(event) = events.recv().await {
        let Some(win) = window.upgrade() else {
            return;
        };
        let params = &event["params"];
        match event["method"].as_str().unwrap_or("") {
            "Runtime.executionContextCreated" => {
                let context = &params["context"];
                if context["auxData"]["frameId"] == target.as_str()
                    && context["auxData"]["isDefault"] == true
                    && context["origin"] == origin.as_str()
                {
                    if let Some(id) = context["id"].as_i64() {
                        own_contexts.insert(id);
                    }
                }
            }
            "Runtime.executionContextDestroyed" => {
                if let Some(id) = params["executionContextId"].as_i64() {
                    own_contexts.remove(&id);
                }
            }
            "Runtime.executionContextsCleared" => own_contexts.clear(),
            "Fetch.requestPaused" => {
                let request = params["requestId"].clone();
                let url = params["request"]["url"].as_str().unwrap_or("");
                let ours = url == format!("{origin}{HOST_PATH}");
                let cdp = win.cdp.clone();
                let session = event["sessionId"].as_str().map(str::to_string);
                tokio::spawn(async move {
                    let session = session.as_deref();
                    if ours {
                        let body = base64::engine::general_purpose::STANDARD.encode(host_page());
                        let _ = cdp
                            .call(
                                session,
                                "Fetch.fulfillRequest",
                                json!({
                                    "requestId": request,
                                    "responseCode": 200,
                                    "responseHeaders": [
                                        {"name": "Content-Type", "value": "text/html; charset=utf-8"},
                                        {"name": "Cache-Control", "value": "no-store"},
                                        {"name": "Content-Security-Policy", "value": HOST_CSP},
                                    ],
                                    "body": body,
                                }),
                            )
                            .await;
                    } else {
                        let _ = cdp
                            .call(
                                session,
                                "Fetch.failRequest",
                                json!({"requestId": request, "errorReason": "BlockedByClient"}),
                            )
                            .await;
                    }
                });
            }
            "Runtime.bindingCalled"
                if params["name"] == "rastAgent"
                    && params["executionContextId"]
                        .as_i64()
                        .is_some_and(|id| own_contexts.contains(&id)) =>
            {
                let Ok(message) =
                    serde_json::from_str::<Value>(params["payload"].as_str().unwrap_or(""))
                else {
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move { told(&state, &win, message).await });
            }
            "Inspector.targetCrashed" | "Inspector.detached" => {
                win.gone.store(true, Ordering::Relaxed);
                win.note(format!(
                    "{}'s window on the server stopped (it may have run out of memory); the next call starts it again with the file as last saved.",
                    win.app.name
                ));
            }
            "Target.detachedFromTarget" if params["sessionId"] == win.page.as_str() => {
                win.gone.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// One message from the window's page.
async fn told(state: &Agent, window: &Arc<Window>, message: Value) {
    let call = message["call"].as_u64();
    match message["kind"].as_str().unwrap_or("") {
        "dirty" => window
            .dirty
            .store(message["dirty"] == true, Ordering::Relaxed),
        "note" => {
            let text: String = message["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect();
            if !text.is_empty() {
                window.note(text);
            }
        }
        "fetch" => {
            if let Some(call) = call {
                let answer = relay(state, window, &message).await;
                window.answer(call, answer).await;
            }
        }
        "save" => {
            if let Some(call) = call {
                let answer = {
                    let _one = window.saving.lock().await;
                    save(state, window, &message).await
                };
                match &answer {
                    Ok(saved) => window.note(format!(
                        "Saved {} to the person's files.",
                        saved["path"].as_str().unwrap_or("")
                    )),
                    Err(error) => window.note(format!(
                        "Could not save {}: {error}",
                        message["name"].as_str().unwrap_or("the file")
                    )),
                }
                window.answer(call, answer).await;
            }
        }
        _ => {}
    }
}

/// The page's request to the account's storage of its own app
/// (`/api/webstorage/{app}/…`), through the host API.
async fn relay(state: &Agent, window: &Window, message: &Value) -> Result<Value, String> {
    let path = message["path"].as_str().unwrap_or("");
    let base = format!("/api/webstorage/{}", window.app.id);
    let rest = path
        .strip_prefix(&base)
        .filter(|rest| rest.starts_with('/') && !rest.contains(".."))
        .ok_or("the window keeps only its own app's storage")?;
    let method = match message["method"].as_str().unwrap_or("GET") {
        "GET" => HttpMethod::GET,
        "POST" => HttpMethod::POST,
        "DELETE" => HttpMethod::DELETE,
        _ => return Err("the window's storage reads, writes and deletes".into()),
    };
    let body = match message["body"].as_str() {
        Some(data) => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "the page sent unreadable data".to_string())?,
        None => Vec::new(),
    };
    let media = message["type"].as_str().unwrap_or("application/json");
    let media = if media == "application/octet-stream" {
        "application/octet-stream"
    } else {
        "application/json"
    };
    let (status, kind, answer) = state
        .host
        .web_storage(&window.owner, &window.app.id, method, rest, body, media)
        .await?;
    Ok(json!({
        "status": status.as_u16(),
        "type": kind,
        "body": base64::engine::general_purpose::STANDARD.encode(answer),
    }))
}

/// A file name the app gave, as one safe name (empty when nothing is left).
pub fn clean_file_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = last
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(255)
        .collect();
    if name == "." || name == ".." {
        String::new()
    } else {
        name
    }
}

fn parent_of(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((dir, _)) => dir,
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `name (n).ext` for the n-th name of its own.
pub fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Where a save goes (see the module docs).
async fn target_for(state: &Agent, window: &Window, name: &str, save_as: bool) -> String {
    if !save_as {
        if let Some(path) = lock(&window.documents).get(name).cloned() {
            return path;
        }
    }
    let current = lock(&window.path).clone();
    let folder = current.as_deref().map(parent_of).unwrap_or("/").to_string();
    let taken: HashSet<String> = match state.host.files_list(&window.owner, &folder).await {
        Ok(listing) => listing.items.into_iter().map(|e| e.name).collect(),
        Err(_) => HashSet::new(),
    };
    let ours = |path: &str| {
        lock(&window.written).contains(path) || lock(&window.documents).values().any(|p| p == path)
    };
    let first = join(&folder, name);
    if !taken.contains(name) || ours(&first) {
        return first;
    }
    for n in 2..1000 {
        let other = numbered(name, n);
        let path = join(&folder, &other);
        if !taken.contains(&other) || ours(&path) {
            return path;
        }
    }
    first
}

/// Store what the app saved in the person's files.
async fn save(state: &Agent, window: &Window, message: &Value) -> Result<Value, String> {
    let name = clean_file_name(message["name"].as_str().unwrap_or(""));
    let name = if name.is_empty() {
        "Untitled".to_string()
    } else {
        name
    };
    let save_as = message["saveAs"] == true;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(message["bytes"].as_str().unwrap_or(""))
        .map_err(|_| "the app handed over unreadable data".to_string())?;
    if bytes.len() > MAX_DOCUMENT {
        return Err("it is larger than 64 MB".into());
    }
    let target = target_for(state, window, &name, save_as).await;
    state
        .host
        .put_file_bytes(&window.owner, &target, bytes)
        .await?;
    lock(&window.written).insert(target.clone());
    let new_document = lock(&window.path).is_none();
    if save_as || new_document {
        *lock(&window.path) = Some(target.clone());
        lock(&window.documents).insert(name, target.clone());
    }
    if let Some(path) = lock(&window.path).clone() {
        last_paths().insert((window.conversation.clone(), window.app.id.clone()), path);
    }
    Ok(json!({ "path": target }))
}

// ------------------------------------------------------------ the tools

/// `open_app` of an app that takes commands: the chat's window of the app on
/// the server starts, and opens the file (what `use_app` works on).
pub async fn open(
    state: &Agent,
    user: &AuthUser,
    conversation: &str,
    app: &HostApp,
    path: Option<&str>,
) -> Result<String, String> {
    let (window, started) = window_for(state, user, conversation, app, path.is_none()).await?;
    let _busy = window.busy.lock().await;
    window.wake().await;
    window.touch();
    let open = match path {
        Some(path) => {
            window.open(state, path).await?;
            last_paths().insert((conversation.to_string(), app.id.clone()), path.to_string());
            format!("{} has {path} open", app.name)
        }
        None => match lock(&window.path).clone() {
            Some(path) => format!("{} has {path} open", app.name),
            None => format!("{} is open (a new document)", app.name),
        },
    };
    window.touch();
    let notes = window.take_notes();
    Ok(format!(
        "{started}{notes}{open} in its window on the server: work in it with use_app (it keeps working when the person closes RAST; they see its pictures in the chat). Run its save command to write the file back."
    ))
}

/// `use_app` in the chat's window of the app on the server.
pub async fn use_app(
    state: &Agent,
    user: &AuthUser,
    available: &Available,
    conversation: &str,
    input: &Value,
) -> Result<Output, String> {
    let (app, methods, batch) = use_app::check(available, input)?;
    let (window, started) = window_for(state, user, conversation, app, true).await?;
    let busy = window.busy.lock().await;
    window.wake().await;
    window.touch();
    let mut results = Vec::new();
    let mut failed = None;
    for (method, params) in &methods {
        if !window.alive() {
            failed = Some(format!("{}'s window on the server stopped", app.name));
            results.push(json!({"ok": false, "error": failed.clone()}));
            break;
        }
        match window.request(method, params).await {
            Ok(value) => results.push(json!({"ok": true, "result": value})),
            Err(error) => {
                results.push(json!({"ok": false, "error": error}));
                failed = Some(error);
                break;
            }
        }
        window.touch();
    }
    window.touch();
    drop(busy);
    let notes = format!("{started}{}", window.take_notes());
    if !batch {
        if let Some(error) = failed {
            return Err(format!("{notes}{}: {error}", app.name));
        }
    }
    let result = if batch {
        json!({ "batch": results })
    } else {
        results
            .pop()
            .map(|r| r["result"].clone())
            .unwrap_or(Value::Null)
    };
    match use_app::shape(state, user, conversation, app, &methods, batch, result).await {
        Ok(mut output) => {
            output.text = format!("{notes}{}", output.text);
            Ok(output)
        }
        Err(error) => Err(format!("{notes}{error}")),
    }
}

/// The document the chat's window of the app has open (for the chat's
/// cards).
pub fn document(conversation: &str, app_id: &str) -> Option<String> {
    let key = (conversation.to_string(), app_id.to_string());
    let open = windows().get(&key).and_then(|w| lock(&w.path).clone());
    open.or_else(|| last_paths().get(&key).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_app_gives_are_cleaned() {
        assert_eq!(clean_file_name("report.docx"), "report.docx");
        assert_eq!(clean_file_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_file_name("a\\b\\c.pdf"), "c.pdf");
        assert_eq!(clean_file_name("  x\u{0}y.txt "), "xy.txt");
        assert_eq!(clean_file_name(".."), "");
        assert_eq!(clean_file_name("dir/"), "");
    }

    #[test]
    fn names_of_their_own() {
        assert_eq!(numbered("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered("README", 2), "README (2)");
        assert_eq!(numbered(".env", 2), ".env (2)");
        assert_eq!(join("/", "a.docx"), "/a.docx");
        assert_eq!(join("/docs", "a.docx"), "/docs/a.docx");
        assert_eq!(parent_of("/a.docx"), "/");
        assert_eq!(parent_of("/docs/a.docx"), "/docs");
    }

    #[test]
    fn the_windows_page_carries_its_script() {
        let page = host_page();
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("window.__rast = {"));
        assert!(!HOST_SCRIPT.contains("</script"));
    }
}
