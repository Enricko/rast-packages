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
//! * Every app answers the window's own control methods ([`by_hand`]:
//!   pictures, clicks, typing, keys, scrolling, dragging), also one without
//!   a command API of its own; one that has it answers the rest.
//! * An app that takes no documents from RAST (it opens files with the
//!   browser's file picker and saves downloads) gets the person's file at
//!   its file picker, and its downloads are stored as Save As stores them
//!   (`app_frame.js`, added to the app's frame before its scripts).
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
use super::host::{Agent, Host, HostApp, WindowSession};
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
/// Added to the app's frame before its own scripts (file picker, downloads).
const FRAME_SCRIPT: &str = include_str!("app_frame.js");
/// The window's size, the space of the control methods' points.
pub const WINDOW_SIZE: (u32, u32) = (1440, 900);
const WIDTH: u32 = WINDOW_SIZE.0;
const HEIGHT: u32 = WINDOW_SIZE.1;
/// The most text one `ui.text` types (key by key).
const MAX_TYPED: usize = 20_000;
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
    /// The account's session of a window of RAST's own window code (its
    /// page is RAST's, `?window=<appId>`); none for a web page app's.
    session: Option<WindowSession>,
    host: Host,
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
    /// What the app offers, once it said ([`Window::offers`]).
    offers: Mutex<Option<Offers>>,
    last_used: AtomicI64,
    busy: tokio::sync::Mutex<()>,
    /// Saves are stored one after another.
    saving: tokio::sync::Mutex<()>,
}

type Key = (String, String);

/// Control methods the window answers itself, for every app.
const WINDOW_ONLY: [&str; 4] = ["ui.scroll", "ui.drag", "ui.move", "app.open"];
/// Control methods the window answers for an app with no command API.
const BY_HAND: [&str; 5] = [
    "ui.screenshot",
    "ui.inspect",
    "ui.click",
    "ui.text",
    "ui.key",
];

/// One key, as DevTools presses it.
#[derive(Debug, PartialEq)]
struct KeyPress {
    key: String,
    code: String,
    key_code: u32,
    text: Option<String>,
}

/// A key by its name: Enter, Tab, Escape, Backspace, Delete, the arrows
/// (Left or ArrowLeft), Home, End, PageUp, PageDown, Space, F1-F12, a letter
/// or a digit.
fn key_named(name: &str) -> Result<KeyPress, String> {
    let named = |key: &str, code: &str, key_code: u32, text: Option<&str>| KeyPress {
        key: key.into(),
        code: code.into(),
        key_code,
        text: text.map(str::to_string),
    };
    let lower = name.trim().to_ascii_lowercase();
    let key = match lower.trim_start_matches("arrow") {
        "enter" | "return" => named("Enter", "Enter", 13, Some("\r")),
        "tab" => named("Tab", "Tab", 9, None),
        "escape" | "esc" => named("Escape", "Escape", 27, None),
        "backspace" => named("Backspace", "Backspace", 8, None),
        "delete" | "del" => named("Delete", "Delete", 46, None),
        "left" => named("ArrowLeft", "ArrowLeft", 37, None),
        "up" => named("ArrowUp", "ArrowUp", 38, None),
        "right" => named("ArrowRight", "ArrowRight", 39, None),
        "down" => named("ArrowDown", "ArrowDown", 40, None),
        "home" => named("Home", "Home", 36, None),
        "end" => named("End", "End", 35, None),
        "pageup" => named("PageUp", "PageUp", 33, None),
        "pagedown" => named("PageDown", "PageDown", 34, None),
        "space" | " " => named(" ", "Space", 32, Some(" ")),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n)) => {
            let n: u32 = f[1..].parse().unwrap_or(1);
            let upper = format!("F{n}");
            named(&upper, &upper, 111 + n, None)
        }
        one if one.chars().count() == 1 && one.chars().all(|c| c.is_ascii_alphanumeric()) => {
            return Ok(key_typed(name.trim().chars().next().unwrap_or('a')));
        }
        _ => {
            return Err(format!(
                "ui.key: {name:?} is not a key here (Enter, Tab, Escape, Backspace, Delete, Left, Up, Right, Down, Home, End, PageUp, PageDown, Space, F1-F12, a letter or a digit)"
            ))
        }
    };
    Ok(key)
}

/// The key that types `ch`.
fn key_typed(ch: char) -> KeyPress {
    let upper = ch.to_ascii_uppercase();
    let (code, key_code) = if ch.is_ascii_alphabetic() {
        (format!("Key{upper}"), upper as u32)
    } else if ch.is_ascii_digit() {
        (format!("Digit{ch}"), ch as u32)
    } else if ch == ' ' {
        ("Space".into(), 32)
    } else {
        (String::new(), 0)
    };
    KeyPress {
        key: ch.to_string(),
        code,
        key_code,
        text: Some(ch.to_string()),
    }
}

/// What an app offers its window: documents from RAST (otherwise it opens
/// files with the browser's file picker) and a command API of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    pub documents: bool,
    pub commands: bool,
}

/// What each app offered the last window of it (for the tool's description).
static KNOWN: LazyLock<Mutex<HashMap<String, Offers>>> = LazyLock::new(Default::default);

/// What `app_id` offered the last window of it on the server, if one ran.
pub fn known_offers(app_id: &str) -> Option<Offers> {
    lock(&KNOWN).get(app_id).copied()
}

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

impl Drop for Window {
    fn drop(&mut self) {
        // The window's session ends with it (RAST ends it after 12 hours
        // anyway).
        if let (Some(session), Ok(runtime)) =
            (self.session.take(), tokio::runtime::Handle::try_current())
        {
            let host = self.host.clone();
            let user = self.owner.id;
            runtime.spawn(async move { host.close_window_session(user, &session.id).await });
        }
    }
}

impl Window {
    /// A window of RAST's own window code (not a web page app's).
    fn is_desktop(&self) -> bool {
        self.session.is_some()
    }

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
        if !self.is_desktop() {
            let _ = self
                .evaluate("window.__rast.flush()".into(), Duration::from_secs(10))
                .await;
        }
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

    /// What the app offers (it says so once it started).
    async fn offers(&self) -> Offers {
        if let Some(offers) = *lock(&self.offers) {
            return offers;
        }
        if self.is_desktop() {
            // It takes a file when its window opens files; it answers
            // requests once its window has mounted (a moment after it
            // opened), or never.
            let documents = self.app.id == "files" || !self.app.opens.is_empty();
            let mut commands = false;
            for _ in 0..20 {
                commands = self
                    .evaluate(
                        "window.__rastWindow.answers()".into(),
                        Duration::from_secs(5),
                    )
                    .await
                    .is_ok_and(|v| v == true);
                if commands {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let offers = Offers {
                documents,
                commands,
            };
            *lock(&self.offers) = Some(offers);
            lock(&KNOWN).insert(self.app.id.clone(), offers);
            return offers;
        }
        let said = self
            .evaluate(
                format!("window.__rast.offers({})", START_LIMIT.as_millis()),
                START_LIMIT + Duration::from_secs(30),
            )
            .await
            .unwrap_or(Value::Null);
        let offers = Offers {
            documents: said["documents"] == true,
            commands: said["commands"] == true,
        };
        if said.is_object() {
            *lock(&self.offers) = Some(offers);
            lock(&KNOWN).insert(self.app.id.clone(), offers);
        }
        offers
    }

    /// One control method: the app's own when it has a command API (and
    /// knows the method), the window's otherwise ([`by_hand`]).
    async fn request(&self, method: &str, params: &Value) -> Result<Value, String> {
        if WINDOW_ONLY.contains(&method) {
            return self.by_hand(method, params).await;
        }
        if !self.offers().await.commands {
            if BY_HAND.contains(&method) {
                return self.by_hand(method, params).await;
            }
            return Err(format!(
                "{} takes no commands: look at it with ui.screenshot and work in it with ui.click, ui.text, ui.key, ui.scroll and ui.drag",
                self.app.name
            ));
        }
        self.app_request(method, params).await
    }

    /// One method of the app's page command API (its window's request
    /// handler, for RAST's own window code).
    async fn app_request(&self, method: &str, params: &Value) -> Result<Value, String> {
        if self.is_desktop() {
            let expression = format!(
                "Promise.resolve(window.__rastWindow.request({}, {})).then((r) => r === undefined ? null : r)",
                json!(method),
                params
            );
            return self
                .evaluate(expression, REQUEST_LIMIT + Duration::from_secs(10))
                .await;
        }
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

    /// Hand the app the person's file at `path`: open it in the app, or,
    /// for an app that takes no documents from RAST, have it ready for the
    /// app's file picker (false).
    async fn open(&self, state: &Agent, path: &str) -> Result<bool, String> {
        if self.is_desktop() {
            self.open_with(&json!({ "path": path })).await?;
            *lock(&self.path) = Some(path.to_string());
            return Ok(true);
        }
        let bytes = state.host.file_bytes(&self.owner, path).await?;
        if bytes.len() > MAX_DOCUMENT {
            return Err("the file is larger than 64 MB".into());
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        drop(bytes);
        let in_app = self.offers().await.documents;
        let expression = if in_app {
            format!(
                "window.__rast.open({}, \"{data}\", {})",
                json!(name),
                START_LIMIT.as_millis()
            )
        } else {
            format!(
                "window.__rast.stage({}, \"{data}\", {})",
                json!(name),
                json!(path)
            )
        };
        self.evaluate(expression, START_LIMIT + Duration::from_secs(30))
            .await?;
        *lock(&self.path) = Some(path.to_string());
        lock(&self.documents).insert(name, path.to_string());
        self.dirty.store(false, Ordering::Relaxed);
        Ok(in_app)
    }

    /// Open the window of RAST's own window code again with these
    /// parameters (`{path}`, Code Editor's `{path, line}`, …).
    async fn open_with(&self, params: &Value) -> Result<(), String> {
        let shown = self
            .evaluate(
                format!("window.__rastWindow.open({params})"),
                Duration::from_secs(30),
            )
            .await?;
        if shown != true {
            return Err(format!("{} did not open for this account", self.app.name));
        }
        // Its window reads the parameters as it opens.
        tokio::time::sleep(Duration::from_millis(800)).await;
        Ok(())
    }

    /// The window's own control methods, for every app: what a person does
    /// with the mouse and keys, and a picture of the window.
    async fn by_hand(&self, method: &str, params: &Value) -> Result<Value, String> {
        let point = |x: &str, y: &str| -> Result<(f64, f64), String> {
            let get = |name: &str, most: u32| {
                params[name]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= most as f64)
                    .ok_or_else(|| {
                        format!("{method}: {name} is a number from 0 to {most} (the window is {WIDTH}×{HEIGHT})")
                    })
            };
            Ok((get(x, WIDTH)?, get(y, HEIGHT)?))
        };
        let answer = match method {
            "ui.screenshot" => {
                let shot = self
                    .cdp
                    .call_for(
                        Some(&self.page),
                        "Page.captureScreenshot",
                        json!({"format": "png"}),
                        Duration::from_secs(60),
                    )
                    .await?;
                return Ok(json!({
                    "png": shot["data"],
                    "width": WIDTH,
                    "height": HEIGHT,
                }));
            }
            "app.open" => {
                if !params.is_object() || params.as_object().is_some_and(|p| p.is_empty()) {
                    return Err("app.open: give what the window opens, e.g. {path}".into());
                }
                if self.is_desktop() {
                    self.open_with(params).await?;
                    if let Some(path) = params["path"].as_str() {
                        *lock(&self.path) = Some(path.to_string());
                    }
                    return Ok(Value::String(format!("{} opened it.", self.app.name)));
                }
                return Err("app.open: open a file in this app with open_app and its path".into());
            }
            "ui.inspect" if self.is_desktop() => {
                let offers = self.offers().await;
                let state = self
                    .evaluate(
                        "window.__rastWindow.state()".into(),
                        Duration::from_secs(10),
                    )
                    .await
                    .unwrap_or(Value::Null);
                let text: String = self
                    .evaluate("window.__rastWindow.text()".into(), Duration::from_secs(10))
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
                    .chars()
                    .take(6000)
                    .collect();
                return Ok(json!({
                    "app": self.app.name,
                    "window": {"width": WIDTH, "height": HEIGHT},
                    "title": state["title"],
                    "open": state["open"],
                    "dialogs": state["dialogs"],
                    "file": lock(&self.path).clone(),
                    "takesCommands": offers.commands,
                    "text": text,
                    "how": "Look at it with ui.screenshot; work in it with ui.click, ui.text, ui.key, ui.scroll and ui.drag. app.open {path, ...} opens a file in it. It saves with its own Save, into the person's files.",
                }));
            }
            "ui.inspect" => {
                let offers = self.offers().await;
                let open = lock(&self.path).clone();
                return Ok(json!({
                    "app": self.app.name,
                    "window": {"width": WIDTH, "height": HEIGHT},
                    "file": open,
                    "takesCommands": offers.commands,
                    "opensRastFiles": offers.documents,
                    "how": if offers.documents {
                        "Look at it with ui.screenshot; work in it with ui.click, ui.text, ui.key, ui.scroll and ui.drag."
                    } else {
                        "Look at it with ui.screenshot; work in it with ui.click, ui.text, ui.key, ui.scroll and ui.drag. It opens files with its own Open command: the file open_app named goes to its file picker. What it saves as a download goes into that file's folder."
                    },
                }));
            }
            "ui.move" => {
                let (x, y) = point("x", "y")?;
                self.mouse("mouseMoved", x, y, "none", 0).await?;
                format!("moved to {x}, {y}")
            }
            "ui.click" => {
                let (x, y) = point("x", "y")?;
                let button = match params["button"].as_str().unwrap_or("left") {
                    b @ ("left" | "right" | "middle") => b,
                    other => {
                        return Err(format!(
                            "ui.click: button {other:?} is left, right or middle"
                        ))
                    }
                };
                let count = params["count"].as_u64().unwrap_or(1).clamp(1, 3);
                self.mouse("mouseMoved", x, y, "none", 0).await?;
                for n in 1..=count {
                    self.mouse("mousePressed", x, y, button, n).await?;
                    self.mouse("mouseReleased", x, y, button, n).await?;
                }
                format!("clicked at {x}, {y}")
            }
            "ui.drag" => {
                let (x, y) = point("x", "y")?;
                let (to_x, to_y) = point("toX", "toY")?;
                self.mouse("mouseMoved", x, y, "none", 0).await?;
                self.mouse("mousePressed", x, y, "left", 1).await?;
                for step in 1..=12 {
                    let f = step as f64 / 12.0;
                    self.mouse(
                        "mouseMoved",
                        x + (to_x - x) * f,
                        y + (to_y - y) * f,
                        "left",
                        0,
                    )
                    .await?;
                    tokio::time::sleep(Duration::from_millis(16)).await;
                }
                self.mouse("mouseReleased", to_x, to_y, "left", 1).await?;
                format!("dragged from {x}, {y} to {to_x}, {to_y}")
            }
            "ui.scroll" => {
                let (x, y) = point("x", "y")?;
                let dx = params["dx"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(-10_000.0, 10_000.0);
                let dy = params["dy"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(-10_000.0, 10_000.0);
                if dx == 0.0 && dy == 0.0 {
                    return Err("ui.scroll: give dy (down when positive) or dx".into());
                }
                self.cdp
                    .call(
                        Some(&self.page),
                        "Input.dispatchMouseEvent",
                        json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy}),
                    )
                    .await?;
                format!("scrolled at {x}, {y}")
            }
            "ui.text" => {
                let text = params["text"].as_str().ok_or("ui.text: give text")?;
                if text.chars().count() > MAX_TYPED {
                    return Err(format!("ui.text: at most {MAX_TYPED} characters at a time"));
                }
                // Key by key: canvas apps read keys, not text input.
                for ch in text.chars() {
                    match ch {
                        '\n' => self.key(&key_named("Enter")?, 0).await?,
                        '\t' => self.key(&key_named("Tab")?, 0).await?,
                        '\r' => {}
                        ch => self.key(&key_typed(ch), 0).await?,
                    }
                }
                format!("typed {} characters", text.chars().count())
            }
            "ui.key" => {
                let name = params["key"].as_str().ok_or("ui.key: give key")?;
                let key = key_named(name)?;
                let on = |flag: &str| params[flag] == true;
                // cmd is the command key of the app's shortcuts: Ctrl here.
                let modifiers = (on("alt") as u32)
                    | ((on("cmd") || on("ctrl")) as u32) << 1
                    | (on("meta") as u32) << 2
                    | (on("shift") as u32) << 3;
                self.key(&key, modifiers).await?;
                format!("pressed {name}")
            }
            other => return Err(format!("the window has no method {other}")),
        };
        // The app answers in its next frames; what it saved meanwhile is
        // stored before the call ends.
        if self.is_desktop() {
            tokio::time::sleep(Duration::from_millis(400)).await;
        } else {
            let _ = self
                .evaluate(
                    "window.__rast.settle()".into(),
                    SAVE_LIMIT + Duration::from_secs(10),
                )
                .await;
        }
        Ok(Value::String(format!("{}: {answer}.", self.app.name)))
    }

    async fn mouse(
        &self,
        kind: &str,
        x: f64,
        y: f64,
        button: &str,
        count: u64,
    ) -> Result<(), String> {
        self.cdp
            .call(
                Some(&self.page),
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": button, "clickCount": count,
                       "buttons": if button == "none" || kind == "mouseReleased" { 0 } else { 1 }}),
            )
            .await
            .map(|_| ())
    }

    async fn key(&self, key: &KeyPress, modifiers: u32) -> Result<(), String> {
        let mut down = json!({
            "type": if key.text.is_some() && modifiers & 0b110 == 0 { "keyDown" } else { "rawKeyDown" },
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "modifiers": modifiers,
        });
        if let (Some(text), 0) = (&key.text, modifiers & 0b110) {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        self.cdp
            .call(Some(&self.page), "Input.dispatchKeyEvent", down)
            .await?;
        self.cdp
            .call(
                Some(&self.page),
                "Input.dispatchKeyEvent",
                json!({"type": "keyUp", "key": key.key, "code": key.code,
                       "windowsVirtualKeyCode": key.key_code, "modifiers": modifiers}),
            )
            .await
            .map(|_| ())
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
                    if window.alive() && !window.is_desktop() {
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
            Ok(true) => {
                note = format!(
                    "{}'s window on the server started again (the last one closed): it has {path} open as last saved.\n",
                    app.name
                )
            }
            Ok(false) => {
                note = format!(
                    "{}'s window on the server started again (the last one closed): open {path} in it again with its Open command (its file picker gets the file as last saved).\n",
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
    let desktop = app.window == "desktop";
    // A web page app's window frames its page; one of RAST's own window
    // code is RAST's page itself, signed in as the account.
    let (proxy_port, page_path, session) = if desktop {
        if !state.host.window_sessions() {
            return Err(format!(
                "{} runs on the server with a newer RAST (host API 3)",
                app.name
            ));
        }
        let session = state.host.open_window_session(user).await?;
        match browser::desktop_proxy_port(rast).await {
            Ok(port) => (port, String::new(), Some(session)),
            Err(error) => {
                state.host.close_window_session(user.id, &session.id).await;
                return Err(error);
            }
        }
    } else {
        let page =
            state.host.app_page(user, &app.id).await.ok_or_else(|| {
                format!("{} is not a web page app this account may open", app.name)
            })?;
        (browser::app_proxy_port(rast).await?, page, None)
    };
    let launch = Launch {
        unit: "rast-app-window",
        description: "RAST app window",
        proxy_port,
        memory: "3G",
        lifetime_secs: LIFETIME_SECS,
        // A share, not a cap: the app's WebAssembly compiles on every core
        // at once, and a cap stalls its drawing with it; the window gives
        // way to everything else on a busy server.
        cpu: "CPUWeight=20",
        window: (WIDTH, HEIGHT),
        flags: FLAGS,
        max_message: 2 * MAX_DOCUMENT + 32 * 1024 * 1024,
    };
    let launched = browser::launch(state, user, &launch).await;
    let cdp = match launched {
        Ok(cdp) => cdp,
        Err(error) => {
            if let Some(session) = &session {
                state.host.close_window_session(user.id, &session.id).await;
            }
            return Err(error);
        }
    };
    let events = cdp.events();
    let prepared = prepare(&cdp, rast).await;
    let (target, page) = match prepared {
        Ok(ready) => ready,
        Err(error) => {
            if let Some(session) = &session {
                state.host.close_window_session(user.id, &session.id).await;
            }
            return Err(browser::started_wrong(&cdp, &error));
        }
    };
    let window = Arc::new(Window {
        cdp,
        page,
        app: app.clone(),
        owner: user.clone(),
        conversation: conversation.to_string(),
        session: session.clone(),
        host: state.host.clone(),
        path: Mutex::new(None),
        documents: Mutex::new(HashMap::new()),
        written: Mutex::new(HashSet::new()),
        notes: Mutex::new(Vec::new()),
        dirty: AtomicBool::new(false),
        gone: AtomicBool::new(false),
        frozen: AtomicBool::new(false),
        offers: Mutex::new(None),
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
    let (address, ready_test) = match &session {
        Some(session) => {
            window
                .cdp
                .call(
                    Some(&window.page),
                    "Network.setCookie",
                    json!({
                        "name": session.cookie,
                        "value": session.token,
                        "url": origin(rast),
                        "path": "/",
                        "httpOnly": true,
                        "sameSite": "Lax",
                    }),
                )
                .await?;
            (
                format!("{}/?window={}", origin(rast), app.id),
                "typeof window.__rastWindow === 'object' && window.__rastWindow.state().open",
            )
        }
        None => (
            format!("{}{HOST_PATH}", origin(rast)),
            "typeof window.__rast === 'object' && document.readyState !== 'loading'",
        ),
    };
    window
        .cdp
        .call(Some(&window.page), "Page.navigate", json!({"url": address}))
        .await?;
    let until = std::time::Instant::now() + START_LIMIT;
    let mut ready = false;
    while std::time::Instant::now() < until {
        if window
            .evaluate(ready_test.into(), Duration::from_secs(5))
            .await
            .is_ok_and(|v| v == true)
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if !ready {
        return Err(format!("{}'s window on the server did not start", app.name));
    }
    if desktop {
        return Ok(window);
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
    // The page is exactly the window's size (a headless window's page is
    // shorter than the window): points and pictures agree.
    cdp.call(
        Some(&page),
        "Emulation.setDeviceMetricsOverride",
        json!({"width": WIDTH, "height": HEIGHT, "deviceScaleFactor": 1, "mobile": false}),
    )
    .await?;
    cdp.call(
        Some(&page),
        "Runtime.addBinding",
        json!({"name": "rastAgent"}),
    )
    .await?;
    // The app's frame, before its scripts: in this process when Chrome keeps
    // it here, in its own target otherwise (see `listen`).
    cdp.call(
        Some(&page),
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": FRAME_SCRIPT}),
    )
    .await?;
    cdp.call(
        Some(&page),
        "Target.setAutoAttach",
        json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
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
            "Target.attachedToTarget" if event["sessionId"] == win.page.as_str() => {
                let Some(child) = params["sessionId"].as_str().map(str::to_string) else {
                    continue;
                };
                let frame = params["targetInfo"]["type"] == "iframe";
                let cdp = win.cdp.clone();
                tokio::spawn(async move {
                    if frame {
                        // The frame waits for us: its first document is
                        // there, its scripts have not run (runImmediately).
                        let _ = cdp.call(Some(&child), "Page.enable", json!({})).await;
                        // It stays on the app's page (a link it follows
                        // would take the app, and its work, away).
                        let _ = cdp
                            .call(
                                Some(&child),
                                "Fetch.enable",
                                json!({"patterns": [{"urlPattern": "*", "resourceType": "Document", "requestStage": "Request"}]}),
                            )
                            .await;
                        let _ = cdp
                            .call(
                                Some(&child),
                                "Page.addScriptToEvaluateOnNewDocument",
                                json!({"source": FRAME_SCRIPT, "runImmediately": true}),
                            )
                            .await;
                        let _ = cdp
                            .call(
                                Some(&child),
                                "Target.setAutoAttach",
                                json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
                            )
                            .await;
                    }
                    let _ = cdp
                        .call(Some(&child), "Runtime.runIfWaitingForDebugger", json!({}))
                        .await;
                });
            }
            "Runtime.executionContextCreated" if event["sessionId"] == win.page.as_str() => {
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
            "Runtime.executionContextDestroyed" if event["sessionId"] == win.page.as_str() => {
                if let Some(id) = params["executionContextId"].as_i64() {
                    own_contexts.remove(&id);
                }
            }
            "Runtime.executionContextsCleared" if event["sessionId"] == win.page.as_str() => {
                own_contexts.clear()
            }
            "Fetch.requestPaused" if event["sessionId"] != win.page.as_str() => {
                // A page the app's frame goes to: only RAST's app files.
                let request = params["requestId"].clone();
                let url = params["request"]["url"].as_str().unwrap_or("").to_string();
                let stays = url.starts_with(&format!("{origin}/api/appweb/"));
                if !stays {
                    let shown: String = url.chars().take(300).collect();
                    win.note(format!(
                        "{} went to open {shown} in its window; the window stays on the app (open links with the browser tool).",
                        win.app.name
                    ));
                }
                let cdp = win.cdp.clone();
                let session = event["sessionId"].as_str().map(str::to_string);
                tokio::spawn(async move {
                    let (method, params) = if stays {
                        ("Fetch.continueRequest", json!({"requestId": request}))
                    } else {
                        (
                            "Fetch.failRequest",
                            json!({"requestId": request, "errorReason": "Aborted"}),
                        )
                    };
                    let _ = cdp.call(session.as_deref(), method, params).await;
                });
            }
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
                if event["sessionId"] == win.page.as_str()
                    && params["name"] == "rastAgent"
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
            "Inspector.targetCrashed" | "Inspector.detached"
                if event["sessionId"] == win.page.as_str() =>
            {
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
    let offers = window.offers().await;
    let open = match path {
        Some(path) => {
            let in_app = window.open(state, path).await?;
            last_paths().insert((conversation.to_string(), app.id.clone()), path.to_string());
            if in_app {
                format!("{} has {path} open", app.name)
            } else {
                format!(
                    "{} opens files with its own Open command: {path} is ready for its file picker. Open it in the app (ui.screenshot, then ui.click its Open button or ui.key O with cmd)",
                    app.name
                )
            }
        }
        None => match lock(&window.path).clone() {
            Some(path) => format!("{} has {path} open", app.name),
            None => format!("{} is open (a new document)", app.name),
        },
    };
    window.touch();
    let notes = window.take_notes();
    let how = match (offers.commands, offers.documents) {
        (true, true) => "Run its save command to write the file back.",
        (true, false) => "What it saves as a download goes into the file's folder (Home for a new one).",
        (false, true) => "It takes no commands: look with ui.screenshot and work with ui.click, ui.text, ui.key, ui.scroll and ui.drag; save with its Save command (Save writes the file back).",
        (false, false) => "It takes no commands: look with ui.screenshot and work with ui.click, ui.text, ui.key, ui.scroll and ui.drag. What it saves as a download goes into the file's folder (Home for a new one).",
    };
    Ok(format!(
        "{started}{notes}{open} in its window on the server: work in it with use_app (it keeps working when the person closes RAST; they see its pictures in the chat). {how}"
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
