//! AI Agent end to end: its HTTP API beside a real RAST (accounts, files,
//! commands, events and the audit log answered by RAST in the same program),
//! with a temporary database and storage tree and a local fake model
//! provider; and as its own program, run by RAST's app host.

use std::net::SocketAddr;

use axum::{
    body::Body,
    http::{header, HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use rast_ai_agent::host::Agent;

use rast::{
    api,
    auth::Role,
    config::{Config, FileConfig},
    db,
    state::AppState,
    users,
};

struct TestServer {
    _dir: tempfile::TempDir,
    state: AppState,
    /// AI Agent beside RAST: its own database and replies.
    agent: Agent,
    app: axum::Router,
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with(false, None).await
    }

    /// AI Agent on RAST's own database, as RAST ran it when it was built in
    /// (the package then takes its data over).
    async fn start_old_built_in() -> Self {
        Self::start_with(true, None).await
    }

    /// Replies wait `wait` before trying a failed step again.
    async fn start_retrying_after(wait: std::time::Duration) -> Self {
        Self::start_with(false, Some(wait)).await
    }

    async fn start_with(on_rast_db: bool, retry_wait: Option<std::time::Duration>) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::from_parts(
            None,
            FileConfig {
                data_dir: Some(dir.path().join("data")),
                storage_dir: Some(dir.path().join("storage")),
                public_url: Some("https://rast.test".into()),
                secure_cookies: Some(false),
                setup_token_required: Some(true),
                trusted_proxies: Some(vec!["127.0.0.1/32".into()]),
                packages_check_interval_minutes: Some(0),
                ..Default::default()
            },
            |_| None,
        )
        .expect("config");
        let state = AppState::new(cfg).await.expect("state");
        let agent_db = if on_rast_db {
            // AI Agent as it was built into RAST, on RAST's tables, with the
            // columns agent.db added since (migrations/0003, 0004).
            sqlx::raw_sql(include_str!("../migrations/0003_message_stats.sql"))
                .execute(&state.db)
                .await
                .expect("agent.db's new columns");
            sqlx::raw_sql(include_str!("../migrations/0004_workspaces.sql"))
                .execute(&state.db)
                .await
                .expect("agent.db's workspaces");
            state.db.clone()
        } else {
            db::connect_with(
                &dir.path().join("agent.db"),
                4,
                &rast_ai_agent::app::MIGRATOR,
            )
            .await
            .expect("agent database")
        };
        let mut agent = Agent::built_in(&state, agent_db);
        if let Some(wait) = retry_wait {
            agent.retry_wait = wait;
        }
        // As RAST mounted it when AI Agent was built in: its package, once
        // installed, takes /api/ai/* over (`hand_to_app`).
        let app = api::router_with(
            state.clone(),
            rast_ai_agent::api::ai::routes()
                .with_state(agent.clone())
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    rast::apphost::proxy::hand_to_app,
                )),
        );
        Self {
            _dir: dir,
            state,
            agent,
            app,
        }
    }

    async fn create_user(&self, username: &str, password: &str, role: Role) {
        users::create(
            &self.state.db,
            &self.state.config,
            username,
            password,
            role,
            None,
            None,
        )
        .await
        .expect("create user");
    }

    async fn user_id(&self, username: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(username)
            .fetch_one(&self.state.db)
            .await
            .expect("user id")
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: HeaderMap,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (key, value) in headers.iter() {
            builder = builder.header(key, value);
        }
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&value).expect("json"))
            }
            None => Body::empty(),
        };
        let request = builder.body(body).expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
        };
        (status, headers, value)
    }

    async fn login(&self, username: &str, password: &str) -> Session {
        let (status, headers, body) = self
            .request(
                "POST",
                "/api/auth/login",
                Some(json!({"username": username, "password": password})),
                HeaderMap::new(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "login failed: {body}");
        let cookie = headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("session cookie")
            .split(';')
            .next()
            .expect("cookie pair")
            .to_string();
        let csrf = body["csrfToken"].as_str().expect("csrf token").to_string();
        assert!(body["user"]["id"].is_i64(), "{body}");
        Session { cookie, csrf }
    }

    /// Send a raw request body (uploads/downloads) through the real router.
    async fn request_raw(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
        headers: HeaderMap,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, body.len());
        for (key, value) in headers.iter() {
            builder = builder.header(key, value);
        }
        let request = builder.body(Body::from(body)).expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, bytes)
    }
}

struct Session {
    cookie: String,
    csrf: String,
}

impl Session {
    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            self.cookie.parse().expect("valid cookie header"),
        );
        headers
    }

    fn headers_with_csrf(&self) -> HeaderMap {
        let mut headers = self.headers();
        headers.insert("x-csrf-token", self.csrf.parse().expect("csrf header"));
        headers
    }
}

const ADMIN_PASSWORD: &str = "Admin-Password-123!";
const USER_PASSWORD: &str = "User-Password-123!";
// ---------------------------------------------------------------- AI Agent

type FakeLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;

/// A local OpenAI-compatible provider: lists three models, streams
/// "Ahoy there!" (with reasoning, usage and its request limits) or, for the
/// model `slow`, four words 700 ms apart. Chat requests need the key
/// `sk-test-secret-1234`; the key `sk-limited-0000` has used up its plan
/// (429, back in an hour). The model `down` is always busy (503); `flaky`
/// is busy at its first request, breaks off with "overloaded" after "Half "
/// at its second, and answers from its third.
fn fake_provider() -> (SocketAddr, FakeLog) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let addr = listener.local_addr().expect("address");
    let log: FakeLog = Default::default();
    let requests = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let requests = requests.clone();
            std::thread::spawn(move || serve_fake(stream, &requests));
        }
    });
    (addr, log)
}

fn serve_fake(mut stream: std::net::TcpStream, log: &FakeLog) {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let (mut auth, mut length) = (String::new(), 0usize);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "authorization" => auth = value.trim().to_string(),
                "content-length" => length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let request_line = request_line.trim().to_string();
    // A chat's naming request (src/naming.rs): a title, kept out of the log.
    if body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You name chats"))
    {
        let reply = json!({"choices": [{"delta": {"content": "\"Greeting a pirate.\"\nmore"}}]});
        let usage = json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 2}});
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {reply}\n\ndata: {usage}\n\ndata: [DONE]\n\n"
        );
        return;
    }
    log.lock()
        .unwrap()
        .push((request_line.clone(), auth.clone(), body.clone()));
    if request_line.starts_with("GET /v1/models") {
        let json = r#"{"data":[{"id":"fake-2"},{"id":"fake-1"},{"id":"slow"}]}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    if auth == "Bearer sk-limited-0000" {
        let json = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":3600}}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    if auth != "Bearer sk-test-secret-1234" {
        let json = r#"{"error":{"message":"Invalid API key"}}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    let busy = |stream: &mut std::net::TcpStream| {
        let json = r#"{"error":{"message":"The server is busy","type":"server_error"}}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        );
    };
    if body["model"] == "down" {
        busy(&mut stream);
        return;
    }
    if body["model"] == "flaky" {
        let tries = log
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, _, body)| line.starts_with("POST") && body["model"] == "flaky")
            .count();
        if tries == 1 {
            busy(&mut stream);
            return;
        }
        if tries == 2 {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            );
            let half = json!({"choices": [{"delta": {"content": "Half "}}]});
            let _ = write!(stream, "data: {half}\n\n");
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = stream.write_all(
                b"data: {\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
            );
            return;
        }
    }
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nx-ratelimit-limit-requests: 100\r\nx-ratelimit-remaining-requests: 97\r\nx-ratelimit-reset-requests: 6m0s\r\nConnection: close\r\n\r\n",
    );
    let event = |delta: Value| format!("data: {}\n\n", json!({"choices": [{"delta": delta}]}));
    if body["model"] == "slow" {
        for word in ["Once ", "upon ", "a ", "time"] {
            if stream
                .write_all(event(json!({"content": word})).as_bytes())
                .is_err()
            {
                return;
            }
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        let _ = stream.write_all(b"data: [DONE]\n\n");
        return;
    }
    if body["model"] == "tools" {
        let _ = stream.write_all(fake_tool_reply(&body).as_bytes());
        return;
    }
    let mut out = String::new();
    out.push_str(&event(json!({"role": "assistant", "content": ""})));
    out.push_str(&event(json!({"reasoning_content": "The user greets me."})));
    out.push_str(&event(json!({"content": "Ahoy "})));
    out.push_str(&event(json!({"content": "there!"})));
    out.push_str(&format!(
        "data: {}\n\n",
        json!({"choices": [], "usage": {"prompt_tokens": 12, "completion_tokens": 5}})
    ));
    out.push_str("data: [DONE]\n\n");
    let _ = stream.write_all(out.as_bytes());
}

/// The fake "tools" model: after tool results it says what they returned;
/// otherwise it asks for the calls the person's words name.
fn fake_tool_reply(body: &Value) -> String {
    let event = |delta: Value| format!("data: {}\n\n", json!({"choices": [{"delta": delta}]}));
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    if last["role"] == "tool" {
        let said: Vec<String> = messages
            .iter()
            .rev()
            .take_while(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or("").to_string())
            .collect();
        out.push_str(&event(
            json!({"content": format!("Tool said: {}", said.join(" | "))}),
        ));
    } else {
        let text = last["content"].as_str().unwrap_or("").to_string();
        let mut calls: Vec<(&str, Value)> = Vec::new();
        if text.contains("write") {
            calls.push((
                "write_file",
                json!({"path": "/hello.txt", "content": "Hi from the agent"}),
            ));
        }
        if text.contains("list") {
            calls.push(("list_files", json!({"path": "/"})));
        }
        if text.contains("relative") {
            calls.push((
                "write_file",
                json!({"path": "plan.md", "content": "Step one"}),
            ));
        }
        if text.contains("escape") {
            calls.push(("read_file", json!({"path": "/outside.txt"})));
        }
        if text.contains("fetch") {
            calls.push(("fetch_url", json!({"url": "http://127.0.0.1:9/secret"})));
        }
        if text.contains("shell") {
            calls.push(("run_command", json!({"command": "id"})));
        }
        if text.contains("artifact") {
            calls.push((
                "artifact",
                json!({"id": "Counter", "title": "Counter", "kind": "html", "content": "<h1>Counter</h1><script>let n = 0</script>"}),
            ));
            calls.push((
                "artifact",
                json!({"title": "Notes", "kind": "markdown", "content": "# Notes"}),
            ));
            calls.push((
                "artifact",
                json!({"title": "Dot", "kind": "svg", "content": "<svg xmlns='http://www.w3.org/2000/svg'><circle r='4'/></svg>"}),
            ));
            calls.push((
                "artifact",
                json!({"title": "Bad", "kind": "exe", "content": "x"}),
            ));
        }
        if calls.is_empty() {
            out.push_str(&event(json!({"content": "Nothing to do."})));
        } else {
            out.push_str(&event(json!({"content": "On it."})));
        }
        for (index, (name, input)) in calls.iter().enumerate() {
            let arguments = input.to_string();
            let (first, rest) = arguments.split_at(arguments.len() / 2);
            out.push_str(&event(json!({"tool_calls": [{"index": index, "id": format!("call_{index}_{name}"), "type": "function", "function": {"name": name, "arguments": first}}]})));
            out.push_str(&event(
                json!({"tool_calls": [{"index": index, "function": {"arguments": rest}}]}),
            ));
        }
    }
    out.push_str(&format!(
        "data: {}\n\n",
        json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 3}})
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Poll a conversation until its last reply is no longer being written.
/// Wait for the chat's title from its first answer (the fake provider's
/// "Greeting a pirate", 5 tokens).
async fn ai_named(server: &TestServer, session: &Session, id: &str) -> Value {
    for _ in 0..200 {
        let (status, _, body) = server
            .request(
                "GET",
                &format!("/api/ai/conversations/{id}"),
                None,
                session.headers(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["conversation"]["title"] == "Greeting a pirate" {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the chat was not named");
}

async fn ai_finished(server: &TestServer, session: &Session, id: &str) -> Value {
    for _ in 0..200 {
        let (status, _, body) = server
            .request(
                "GET",
                &format!("/api/ai/conversations/{id}"),
                None,
                session.headers(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let last = body["messages"].as_array().unwrap().last().cloned();
        if last.is_some_and(|m| m["status"] != "streaming") {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the reply did not finish");
}

#[tokio::test]
async fn ai_agent_chat_limits_and_admin_view() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    server.create_user("bob", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let bob = server.login("bob", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    for name in ["alice", "bob"] {
        let id = server.user_id(name).await;
        let (status, _, body) = server
            .request(
                "PUT",
                &format!("/api/users/{id}/access-rules"),
                Some(json!({"folderRules": [], "appRules": [{"appId": "agent", "rule": "allow"}]})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // The app starts uninstalled, for administrators too.
    let (status, _, _) = server
        .request("GET", "/api/ai/options", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request("GET", "/api/ai/options", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], false);

    // Setup: a provider (its key stays secret), a skill, an agent.
    let (addr, log) = fake_provider();
    let base_url = format!("http://{addr}/v1");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/models",
            Some(json!({"kind": "custom", "baseUrl": base_url})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"], json!(["fake-1", "fake-2", "slow"]));
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({"kind": "custom", "name": "Local", "baseUrl": base_url})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only administrators set up");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({
                "kind": "custom",
                "name": "Local",
                "baseUrl": format!("{base_url}/"),
                "apiKey": "sk-test-secret-1234",
                "models": ["fake-1", "slow"],
                // USD per million tokens; a model not offered is dropped.
                "prices": {"fake-1": [1000.0, 2000.0], "fake-2": [1.0, 1.0]},
                // What the model list gave when the models were loaded.
                "listPrices": {"slow": {"input": 1.0, "output": 2.0, "cached": 0.1}},
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["provider"]["baseUrl"], base_url);
    assert_eq!(body["provider"]["hasKey"], true);
    assert_eq!(body["provider"]["keyHint"], "…1234");
    assert_eq!(
        body["provider"]["prices"],
        json!({"fake-1": {"input": 1000.0, "output": 2000.0}})
    );
    assert!(!body.to_string().contains("sk-test-secret"), "{body}");

    // Prices: what an administrator set wins over the provider's list,
    // which wins over the built-in list.
    let price_of = |listing: &Value, model: &str| -> Value {
        listing["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == provider_id.as_str())
            .unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model"] == model)
            .unwrap()
            .clone()
    };
    let (status, _, listing) = server
        .request("GET", "/api/ai/prices", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(price_of(&listing, "fake-1")["source"], "set");
    assert_eq!(price_of(&listing, "fake-1")["fallback"]["source"], "none");
    assert_eq!(price_of(&listing, "slow")["source"], "list");
    assert_eq!(price_of(&listing, "slow")["price"]["cached"], 0.1);
    let built_in = listing["builtIn"]["models"].as_array().unwrap();
    assert!(built_in
        .iter()
        .any(|m| m["name"] == "claude-opus-5-5" && m["input"] == 4.0));
    assert!(!listing["builtIn"]["patterns"]
        .as_array()
        .unwrap()
        .is_empty());
    let (status, _, _) = server
        .request("GET", "/api/ai/prices", None, alice.headers())
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only administrators see prices"
    );
    let price_url = format!("/api/ai/providers/{provider_id}/prices");
    for (bad, why) in [
        (
            json!({"model": "slow", "price": {"input": -1.0, "output": 1.0}}),
            "a negative price",
        ),
        (
            json!({"model": "fake-2", "price": {"input": 1.0, "output": 1.0}}),
            "a model not offered",
        ),
    ] {
        let (status, _, body) = server
            .request("PUT", &price_url, Some(bad), admin.headers_with_csrf())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    let (status, _, listing) = server
        .request(
            "PUT",
            &price_url,
            Some(json!({"model": "slow", "price": {"input": 3.0, "output": 15.0, "cached": 0.3, "cacheWrite": 3.75}})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(price_of(&listing, "slow")["source"], "set");
    assert_eq!(price_of(&listing, "slow")["price"]["cacheWrite"], 3.75);
    assert_eq!(price_of(&listing, "slow")["fallback"]["source"], "list");
    let (status, _, listing) = server
        .request(
            "PUT",
            &price_url,
            Some(json!({"model": "slow", "price": null})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(price_of(&listing, "slow")["source"], "list");
    // Asking the list again: this one gives no prices.
    let (status, _, listing) = server
        .request(
            "POST",
            "/api/ai/prices/refresh",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(listing["results"][0]["priced"], 0, "{listing}");
    assert_eq!(price_of(&listing, "slow")["source"], "none");
    assert_eq!(price_of(&listing, "fake-1")["source"], "set");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/skills",
            Some(json!({"name": "Pirate", "instructions": "Always end with Arr."})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let skill_id = body["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/agents",
            Some(json!({
                "name": "Captain",
                "systemPrompt": "Talk like a ship's captain.",
                "providerId": provider_id,
                "model": "fake-1",
                "skills": [skill_id],
                "temperature": 0.3,
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let agent_id = body["agent"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("sk-test-secret"));
    assert_eq!(body["agents"][0]["skills"], json!([skill_id]));

    // Alice sees the agent but not its personality.
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, alice.headers())
        .await;
    assert_eq!(body["configured"], true);
    assert_eq!(body["agents"][0]["name"], "Captain");
    assert!(body["agents"][0].get("systemPrompt").is_none());
    assert_eq!(body["providers"][0]["models"], json!(["fake-1", "slow"]));

    // A model the provider does not offer is refused.
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "fake-2"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"agentId": agent_id})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conversation = body["conversation"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["conversation"]["model"], "fake-1");
    let path = format!("/api/ai/conversations/{conversation}/messages");
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "Hello?\nAre you there?"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reply"]["status"], "streaming");
    assert_eq!(body["conversation"]["title"], "Hello?");
    let done = ai_finished(&server, &alice, &conversation).await;
    let reply = done["messages"][1].clone();
    assert_eq!(reply["status"], "done", "{reply}");
    assert_eq!(
        reply["content"],
        json!([{"type": "reasoning", "text": "The user greets me."}, {"type": "text", "text": "Ahoy there!"}])
    );
    assert_eq!(
        (
            reply["inputTokens"].as_i64(),
            reply["outputTokens"].as_i64()
        ),
        (Some(12), Some(5))
    );
    // For the bar under the message box: what the model read and wrote at
    // its last step, the cache's share and the time it wrote.
    assert_eq!(reply["contextTokens"], 17, "{reply}");
    assert_eq!(reply["cacheReadTokens"], 0);
    assert!(
        reply["writingMs"].as_i64().is_some_and(|ms| ms >= 0),
        "{reply}"
    );
    // The limits of the providers Alice may use: the key's name is the
    // administrators' to see.
    let (status, _, quota) = server
        .request("GET", "/api/ai/quota", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{quota}");
    let account = &quota["providers"][0]["accounts"][0];
    assert_eq!(account["label"], "", "{quota}");
    assert_eq!(
        account["quota"]["windows"][0]["name"], "Requests",
        "{quota}"
    );
    assert!(account.get("keyHint").is_none(), "{quota}");
    let (_, _, quota) = server
        .request("GET", "/api/ai/quota", None, admin.headers())
        .await;
    assert_eq!(
        quota["providers"][0]["accounts"][0]["label"], "Key …1234",
        "{quota}"
    );
    {
        let requests = log.lock().unwrap();
        let (line, auth, sent) = requests.last().unwrap();
        assert!(line.starts_with("POST /v1/chat/completions"), "{line}");
        assert_eq!(auth, "Bearer sk-test-secret-1234");
        assert_eq!(sent["model"], "fake-1");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["temperature"], 0.3);
        let system = sent["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.starts_with("Talk like a ship's captain."),
            "{system}"
        );
        assert!(system.contains("Always end with Arr."), "{system}");
        assert_eq!(
            sent["messages"][1],
            json!({"role": "user", "content": "Hello?\nAre you there?"})
        );
    }

    // The next request carries the conversation so far.
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "Again"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    ai_finished(&server, &alice, &conversation).await;
    {
        let requests = log.lock().unwrap();
        let sent = &requests.last().unwrap().2;
        let roles: Vec<&str> = sent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(sent["messages"][2]["content"], "Ahoy there!");
    }
    // The first answer named the chat, in a request of its own (5 tokens).
    let named = ai_named(&server, &alice, &conversation).await;
    assert_eq!(named["conversation"]["title"], "Greeting a pirate");
    let (_, _, body) = server
        .request("GET", "/api/ai/usage", None, alice.headers())
        .await;
    assert_eq!(body["allowance"]["usedToday"], 39);
    assert_eq!(body["allowance"]["usedWeek"], 39);
    // Each reply's cost is kept as it was priced (micro-dollars).
    let (input, output, cost): (i64, i64, i64) = sqlx::query_as(
        "SELECT SUM(input_tokens), SUM(output_tokens), SUM(cost) FROM ai_usage WHERE user_id = ?",
    )
    .bind(alice_id)
    .fetch_one(&server.agent.db)
    .await
    .unwrap();
    assert_eq!(cost, input * 1000 + output * 2000);
    assert!(cost > 0);
    assert_eq!(body["allowance"]["costToday"], json!(cost as f64 / 1e6));
    // Each reply keeps its estimate (here the same as what is counted), and
    // the account sees its own use with the estimates added up.
    let estimated: i64 = sqlx::query_scalar(
        "SELECT SUM(estimated_cost) FROM ai_messages m JOIN ai_conversations c
             ON c.id = m.conversation_id WHERE c.user_id = ?",
    )
    .bind(alice_id)
    .fetch_one(&server.agent.db)
    .await
    .unwrap();
    assert_eq!(estimated, cost);
    // Administrators see everyone's use together (only Alice used it).
    let (status, _, _) = server
        .request("GET", "/api/ai/admin/usage?days=7", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, history) = server
        .request("GET", "/api/ai/admin/usage?days=7", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["totals"]["today"]["tokens"], 39);
    assert_eq!(
        history["totals"]["month"]["estimate"],
        json!(cost as f64 / 1e6)
    );
    assert_eq!(history["days"].as_array().unwrap().len(), 7);
    assert_eq!(history["days"][6]["estimate"], json!(cost as f64 / 1e6));
    assert!(history.get("user").is_none(), "{history}");
    let (_, _, chat) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            alice.headers(),
        )
        .await;
    let replies: f64 = chat["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["estimatedCost"].as_f64().unwrap())
        .sum();
    assert!((replies - cost as f64 / 1e6).abs() < 1e-9, "{chat}");

    // A slow reply can be stopped; only one runs per conversation.
    let (status, _, body) = server
        .request(
            "PATCH",
            &format!("/api/ai/conversations/{conversation}"),
            Some(json!({"model": "slow", "title": "Pirates"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["agentId"], agent_id);
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "A story"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "Another"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, _, live) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(live["conversation"]["running"], true);
    assert!(live["seq"].as_u64().unwrap() >= 1, "{live}");
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{conversation}/stop"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stopped"], true);
    let done = ai_finished(&server, &alice, &conversation).await;
    let last = done["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["status"], "stopped", "{last}");
    assert!(
        ai_text(&last).starts_with("Once"),
        "what was written is kept: {last}"
    );

    // Bob cannot see Alice's conversation; an administrator can read it
    // (audited) but not write in it.
    let (status, _, _) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            bob.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, body) = server
        .request("GET", "/api/ai/conversations", None, bob.headers())
        .await;
    assert_eq!(body["items"], json!([]));
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/conversations", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["owner"], "alice");
    assert_eq!(body["items"][0]["title"], "Pirates");
    assert_eq!(body["items"][0]["messageCount"], 6);
    let (status, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            admin.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["readOnly"], true);
    assert_eq!(body["conversation"]["owner"], "alice");
    let viewed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE action = 'ai.conversation.view'")
            .fetch_one(&server.state.db)
            .await
            .unwrap();
    assert_eq!(viewed, 1);
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "hi"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Limits: spending, then weekly tokens, then the daily allowance used
    // up, then no providers.
    let (_, _, usage) = server
        .request("GET", "/api/ai/usage", None, alice.headers())
        .await;
    let spent = usage["allowance"]["costToday"].as_f64().unwrap();
    let used_week = usage["allowance"]["usedWeek"].as_i64().unwrap();
    for (limit, word) in [
        (json!({"dailyCost": spent}), "spending"),
        (
            json!({"weeklyCost": spent, "dailyCost": spent * 10.0}),
            "spending",
        ),
        (json!({"weeklyTokens": used_week}), "week"),
    ] {
        let (status, _, body) = server
            .request(
                "PUT",
                &format!("/api/ai/limits/user:{alice_id}"),
                Some(limit.clone()),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, _, body) = server
            .request(
                "POST",
                &path,
                Some(json!({"text": "more"})),
                alice.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{limit} {body}");
        assert!(body.to_string().contains(word), "{limit} {body}");
    }
    let (_, _, usage) = server
        .request("GET", "/api/ai/usage", None, alice.headers())
        .await;
    assert_eq!(usage["limits"]["weeklyTokens"], used_week);
    assert_eq!(usage["allowance"]["weeklyTokens"], used_week);
    let (status, _, _) = server
        .request(
            "PUT",
            &format!("/api/ai/limits/user:{alice_id}"),
            Some(json!({"dailyCost": -1})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Administrators see each account's use by day and model.
    let (status, _, stats) = server
        .request(
            "GET",
            &format!("/api/ai/admin/usage/{alice_id}?days=7"),
            None,
            admin.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["user"]["username"], "alice");
    let days = stats["days"].as_array().unwrap();
    assert_eq!(days.len(), 7);
    assert_eq!(days[6]["tokens"], usage["allowance"]["usedToday"]);
    let fake = stats["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "fake-1")
        .unwrap();
    assert_eq!(fake["provider"], "Local");
    assert_eq!(fake["cost"], json!(cost as f64 / 1e6));
    assert_eq!(stats["allowance"]["weeklyTokens"], used_week);
    let (_, _, overview) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    let row = overview["usage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["userId"] == alice_id)
        .unwrap();
    assert_eq!(row["week"], used_week);
    assert_eq!(row["costToday"], json!(cost as f64 / 1e6));
    assert_eq!(row["estimateMonth"], json!(cost as f64 / 1e6));
    let (status, _, _) = server
        .request(
            "GET",
            &format!("/api/ai/admin/usage/{alice_id}"),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/limits/user:{alice_id}"),
            Some(json!({"dailyTokens": 20})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "more"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    let (status, _, _) = server
        .request(
            "PUT",
            &format!("/api/ai/limits/user:{alice_id}"),
            Some(json!({"providers": []})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, alice.headers())
        .await;
    assert_eq!(body["providers"], json!([]));
    assert_eq!(body["agents"], json!([]));
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "more"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = server
        .request(
            "PUT",
            "/api/ai/limits/user:abc",
            Some(json!({})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/limits/user:{alice_id}"),
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Deleting the conversation keeps what it used counted.
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = server
        .request("GET", "/api/ai/usage", None, alice.headers())
        .await;
    assert!(
        body["allowance"]["usedToday"].as_i64().unwrap() > 34,
        "{body}"
    );

    // Uninstalling closes the API again.
    let (status, _, _) = server
        .request(
            "DELETE",
            "/api/packages/apps/agent?confirm=agent",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = server
        .request("GET", "/api/ai/options", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn ai_provider_accounts_take_over_at_their_limits() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (addr, log) = fake_provider();
    let base_url = format!("http://{addr}/v1");
    // The provider's own key has used up its plan; a second account has not.
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({
                "kind": "custom",
                "name": "Pool",
                "baseUrl": base_url,
                "apiKey": "sk-limited-0000",
                "models": ["fake-1"],
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["provider"]["accounts"].as_array().unwrap().len(), 1);
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/providers/{provider_id}/accounts"),
            Some(json!({"apiKey": "sk-test-secret-1234"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let accounts = body["provider"]["accounts"].as_array().unwrap().clone();
    assert_eq!(accounts.len(), 2, "{body}");
    assert_eq!(accounts[0]["id"], provider_id.as_str());
    assert_eq!(accounts[0]["label"], "Key …0000");
    assert_eq!(accounts[1]["label"], "Key …1234");
    assert_eq!(body["provider"]["rotation"]["mode"], "fill");
    let spare = accounts[1]["id"].as_str().unwrap().to_string();
    // Only the provider is listed; its accounts are not providers of their own.
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    assert_eq!(body["providers"].as_array().unwrap().len(), 1, "{body}");

    let (_, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "fake-1"})),
            admin.headers_with_csrf(),
        )
        .await;
    let conversation = body["conversation"]["id"].as_str().unwrap().to_string();
    let reply = ai_say(&server, &admin, &conversation, "Hello").await;
    assert_eq!(reply["status"], "done", "{reply}");
    assert!(reply.to_string().contains("Ahoy there!"), "{reply}");
    // The account that answered also named the chat.
    ai_named(&server, &admin, &conversation).await;
    let keys = |log: &FakeLog| -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|(line, _, _)| line.starts_with("POST"))
            .map(|(_, auth, _)| auth.clone())
            .collect()
    };
    assert_eq!(
        keys(&log),
        ["Bearer sk-limited-0000", "Bearer sk-test-secret-1234"]
    );

    // The used-up account rests until its reset; the other kept the limits
    // its answer reported, and what it answered is counted for it.
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/quota", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let now = body["now"].as_i64().unwrap();
    let quota = body["providers"][0]["accounts"].as_array().unwrap().clone();
    assert_eq!(
        quota[0]["resting"]["reason"], "Usage limit reached",
        "{body}"
    );
    let until = quota[0]["resting"]["until"].as_i64().unwrap();
    assert!(
        until > now + 3_500_000 && until <= now + 3_600_000,
        "{body}"
    );
    assert_eq!(quota[1]["resting"], Value::Null);
    assert_eq!(
        quota[1]["quota"]["windows"][0]["name"], "Requests",
        "{body}"
    );
    assert_eq!(quota[1]["quota"]["windows"][0]["used"], 3);
    assert_eq!(quota[1]["quota"]["windows"][0]["limit"], 100);
    assert_eq!(quota[1]["counted"]["hours5"]["calls"], 2, "{body}");
    assert_eq!(quota[1]["counted"]["hours5"]["tokens"], 22);
    assert_eq!(quota[0]["counted"]["week"]["calls"], 0);
    // Per model, and only Codex plans are asked on demand.
    assert_eq!(quota[1]["models"][0]["model"], "fake-1", "{body}");
    assert_eq!(quota[1]["models"][0]["hours5"]["calls"], 2);
    assert_eq!(quota[1]["canCheck"], false);
    assert_eq!(body["rotation"]["mode"], "fill");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/admin/quota/refresh",
            Some(json!({})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["providers"][0]["accounts"][1]["checkError"],
        Value::Null
    );

    // A limit RAST keeps: reaching it rests the account until enough of its
    // use leaves the window.
    let second = quota[1]["id"].as_str().unwrap().to_string();
    let limits = format!("/api/ai/providers/{provider_id}/accounts/{second}/limits");
    let (status, _, body) = server
        .request(
            "PUT",
            &limits,
            Some(json!({"hours5": 0, "week": null})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "PUT",
            &limits,
            Some(json!({"hours5": 10, "week": 5000})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["limits"], json!({"hours5": 10, "week": 5000}));
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/quota", None, admin.headers())
        .await;
    let kept = &body["providers"][0]["accounts"][1];
    assert_eq!(kept["limits"]["hours5"], 10, "{body}");
    assert_eq!(
        kept["resting"]["reason"], "Reached the 5-hour limit set in RAST",
        "{body}"
    );
    let leaves = kept["counted"]["hours5"]["leavesAt"].as_i64().unwrap();
    assert_eq!(kept["resting"]["until"].as_i64(), Some(leaves), "{body}");
    assert!(leaves > now + 4 * 3_600_000, "{body}");
    assert!(kept["counted"]["week"]["leavesAt"].as_i64().unwrap() > leaves);
    // Cleared, and back now.
    let (status, _, body) = server
        .request(
            "PUT",
            &limits,
            Some(json!({"hours5": null, "week": null})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["limits"], json!({}));
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}/accounts/{second}"),
            Some(json!({"wake": true})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["accounts"][1]["resting"], Value::Null);

    // What is new: shown once per version and account.
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, admin.headers())
        .await;
    assert_eq!(body["version"], rast_ai_agent::app::VERSION, "{body}");
    assert_eq!(body["whatsNewSeen"], Value::Null);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/whats-new",
            Some(json!({"version": "1.3.0 <b>"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/whats-new",
            Some(json!({"version": rast_ai_agent::app::VERSION})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, admin.headers())
        .await;
    assert_eq!(body["whatsNewSeen"], rast_ai_agent::app::VERSION);

    // Round robin for every provider at once, and for the ones added later.
    let (status, _, body) = server
        .request(
            "PUT",
            "/api/ai/admin/rotation",
            Some(json!({"mode": "round", "sticky": 101})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "PUT",
            "/api/ai/admin/rotation",
            Some(json!({"mode": "round", "sticky": 2})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({"kind": "custom", "name": "Later", "baseUrl": base_url, "apiKey": "sk-test-secret-1234", "models": ["fake-1"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["provider"]["rotation"],
        json!({"mode": "round", "sticky": 2})
    );
    let later = body["provider"]["id"].as_str().unwrap().to_string();
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    assert_eq!(body["rotation"], json!({"mode": "round", "sticky": 2}));
    assert!(body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["rotation"]["mode"] == "round"));
    // One provider on its own; then back to fill first for all.
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{later}/rotation"),
            Some(json!({"mode": "fill", "sticky": 1})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["rotation"]["mode"], "fill");
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}/rotation"),
            Some(json!({"mode": "fill", "sticky": 1})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request(
            "DELETE",
            &format!("/api/ai/providers/{later}"),
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // While it rests, replies go straight to the other account.
    let reply = ai_say(&server, &admin, &conversation, "Again").await;
    assert_eq!(reply["status"], "done", "{reply}");
    assert_eq!(keys(&log).len(), 3);
    assert_eq!(keys(&log)[2], "Bearer sk-test-secret-1234");

    // Round robin, one reply each; "Use now" ends the rest.
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}"),
            Some(json!({
                "name": "Pool",
                "baseUrl": base_url,
                "models": ["fake-1"],
                "rotation": {"mode": "round", "sticky": 0},
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}"),
            Some(json!({
                "name": "Pool",
                "baseUrl": base_url,
                "models": ["fake-1"],
                "rotation": {"mode": "round", "sticky": 1},
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["rotation"]["mode"], "round");
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}/accounts/{provider_id}"),
            Some(json!({"wake": true})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["accounts"][0]["resting"], Value::Null);
    // Set aside: the other account answers every time.
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}/accounts/{provider_id}"),
            Some(json!({"paused": true})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["accounts"][0]["paused"], true);
    for text in ["One", "Two"] {
        let reply = ai_say(&server, &admin, &conversation, text).await;
        assert_eq!(reply["status"], "done", "{reply}");
    }
    assert_eq!(keys(&log).len(), 5);
    assert!(keys(&log)[3..]
        .iter()
        .all(|k| k == "Bearer sk-test-secret-1234"));

    // Moving the spare up puts its key first (the provider stays the provider).
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/ai/providers/{provider_id}/accounts/{spare}"),
            Some(json!({"move": "up"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let accounts = body["provider"]["accounts"].as_array().unwrap().clone();
    assert_eq!(accounts[0]["id"], provider_id.as_str());
    assert_eq!(accounts[0]["label"], "Key …1234", "{body}");
    assert_eq!(accounts[0]["paused"], false);
    assert_eq!(accounts[1]["label"], "Key …0000");
    assert_eq!(accounts[1]["paused"], true);
    // What each account answered moved with it.
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/quota", None, admin.headers())
        .await;
    assert_eq!(
        body["providers"][0]["accounts"][0]["counted"]["hours5"]["calls"], 5,
        "{body}"
    );

    // Removing the first account: the other's key takes its place.
    let (status, _, body) = server
        .request(
            "DELETE",
            &format!("/api/ai/providers/{provider_id}/accounts/{provider_id}"),
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let accounts = body["provider"]["accounts"].as_array().unwrap().clone();
    assert_eq!(accounts.len(), 1, "{body}");
    assert_eq!(accounts[0]["id"], provider_id.as_str());
    assert_eq!(accounts[0]["label"], "Key …0000");
    // The only account stays; the provider is deleted instead.
    let (status, _, body) = server
        .request(
            "DELETE",
            &format!("/api/ai/providers/{provider_id}/accounts/{provider_id}"),
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // Every account set aside: the reply says so.
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{conversation}/messages"),
            Some(json!({ "text": "Anyone?" })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("set aside"), "{body}");
}

async fn ai_say(server: &TestServer, session: &Session, id: &str, text: &str) -> Value {
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{id}/messages"),
            Some(json!({ "text": text })),
            session.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = ai_finished(server, session, id).await;
    body["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
}

fn ai_tools(message: &Value) -> Vec<Value> {
    message["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "tool")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn ai_agent_files_go_with_messages() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    server.create_user("bob", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let bob = server.login("bob", USER_PASSWORD).await;
    for name in ["alice", "bob"] {
        let id = server.user_id(name).await;
        let (status, _, body) = server
            .request(
                "PUT",
                &format!("/api/users/{id}/access-rules"),
                Some(json!({"folderRules": [], "appRules": [{"appId": "agent", "rule": "allow"}]})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (addr, log) = fake_provider();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({
                "kind": "custom",
                "name": "Local",
                "baseUrl": format!("http://{addr}/v1"),
                "apiKey": "sk-test-secret-1234",
                "models": ["fake-1"],
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (_, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "fake-1"})),
            alice.headers_with_csrf(),
        )
        .await;
    let conversation = body["conversation"]["id"].as_str().unwrap().to_string();
    let upload = |name: &str, bytes: &[u8], mime: &str, who: &Session| {
        let path = format!("/api/ai/files?name={name}");
        let (bytes, mime, headers) = (bytes.to_vec(), mime.to_string(), who.headers_with_csrf());
        let server = &server;
        async move {
            let (status, body) = server
                .request_raw("POST", &path, bytes, &mime, headers)
                .await;
            (
                status,
                serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
            )
        }
    };

    // Images, PDFs and text are taken; other files and big ones are not.
    let png = b"\x89PNG\r\n\x1a\nfake image";
    let (status, image) = upload("cat.png", png, "image/png", &alice).await;
    assert_eq!(status, StatusCode::CREATED, "{image}");
    assert_eq!(image["kind"], "image");
    assert_eq!(image["mime"], "image/png");
    let (status, notes) = upload("notes.txt", b"buy milk", "text/plain", &alice).await;
    assert_eq!(status, StatusCode::CREATED, "{notes}");
    assert_eq!(notes["kind"], "text");
    let (status, pdf) = upload("doc.pdf", b"%PDF-1.7\nx", "application/pdf", &alice).await;
    assert_eq!(status, StatusCode::CREATED, "{pdf}");
    let (status, body) = upload("a.bin", b"\x00\x01\x02", "application/octet-stream", &alice).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    let big = vec![b'a'; 10 * 1024 * 1024 + 1];
    let (status, _) = upload("big.txt", &big, "text/plain", &alice).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // A file not sent can be taken out; someone else's cannot be sent.
    let pdf_id = pdf["id"].as_str().unwrap();
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/files/{pdf_id}"),
            None,
            bob.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/files/{pdf_id}"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, bobs) = upload("bob.txt", b"secret", "text/plain", &bob).await;
    let path = format!("/api/ai/conversations/{conversation}/messages");
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "hi", "files": [bobs["id"]]})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Sent: the message lists them, the model gets the image and the text.
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(json!({"text": "", "files": [image["id"], notes["id"]]})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["title"], "cat.png");
    let done = ai_finished(&server, &alice, &conversation).await;
    let sent_blocks = done["messages"][0]["content"].as_array().unwrap().clone();
    assert_eq!(sent_blocks.len(), 3, "{sent_blocks:?}");
    assert_eq!(sent_blocks[0]["type"], "file");
    assert_eq!(sent_blocks[0]["name"], "cat.png");
    assert_eq!(sent_blocks[1]["kind"], "text");
    {
        let requests = log.lock().unwrap();
        let sent = &requests.last().unwrap().2;
        let content = &sent["messages"][1]["content"];
        assert_eq!(
            content[0],
            json!({"type": "text", "text": "<file name=\"notes.txt\">\nbuy milk\n</file>"}),
            "{content}"
        );
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGg"), "{url}");
    }
    // A sent file stays with the chat; the next request sends it again.
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/files/{}", image["id"].as_str().unwrap()),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    ai_say(&server, &alice, &conversation, "And now?").await;
    {
        let requests = log.lock().unwrap();
        let sent = &requests.last().unwrap().2;
        assert!(sent["messages"][1]["content"].is_array(), "{sent}");
    }

    // Its owner and administrators see it; images show, text downloads.
    let image_url = format!("/api/ai/files/{}", image["id"].as_str().unwrap());
    let (status, bytes) = server
        .request_raw("GET", &image_url, Vec::new(), "", alice.headers())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, png);
    let mut again = alice.headers();
    again.insert(
        header::IF_NONE_MATCH,
        format!("\"{}\"", image["id"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let (status, _) = server
        .request_raw("GET", &image_url, Vec::new(), "", again.clone())
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    let (status, _) = server
        .request_raw("GET", &image_url, Vec::new(), "", admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = server
        .request_raw("GET", &image_url, Vec::new(), "", bob.headers())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Pinned chats come first; pinning leaves the chat's time as it is.
    let (_, _, newer) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "fake-1"})),
            alice.headers_with_csrf(),
        )
        .await;
    let newer = newer["conversation"]["id"].as_str().unwrap().to_string();
    let ids = |body: &Value| -> Vec<String> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, _, list) = server
        .request("GET", "/api/ai/conversations", None, alice.headers())
        .await;
    assert_eq!(ids(&list), [newer.clone(), conversation.clone()]);
    let before = list["items"][1]["updatedAt"].clone();
    let chat_url = format!("/api/ai/conversations/{conversation}");
    let (status, _, body) = server
        .request(
            "PATCH",
            &chat_url,
            Some(json!({"pinned": true})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["conversation"]["pinnedAt"].is_i64(), "{body}");
    assert_eq!(body["conversation"]["updatedAt"], before);
    let (_, _, list) = server
        .request("GET", "/api/ai/conversations", None, alice.headers())
        .await;
    assert_eq!(ids(&list), [conversation.clone(), newer.clone()]);
    let (_, _, body) = server
        .request(
            "PATCH",
            &chat_url,
            Some(json!({"pinned": false})),
            alice.headers_with_csrf(),
        )
        .await;
    assert!(body["conversation"]["pinnedAt"].is_null(), "{body}");
    let (status, _, _) = server
        .request(
            "PATCH",
            &chat_url,
            Some(json!({"pinned": true})),
            bob.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "only its owner pins it");

    // Deleting the chat removes its files.
    let alice_id = server.user_id("alice").await;
    let on_disk = server
        .state
        .config
        .data_dir
        .join("ai-files")
        .join(alice_id.to_string())
        .join(image["id"].as_str().unwrap());
    assert!(on_disk.is_file());
    let (status, _, _) = server
        .request(
            "DELETE",
            &format!("/api/ai/conversations/{conversation}"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!on_disk.exists());
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_files WHERE user_id = ?")
        .bind(alice_id)
        .fetch_one(&server.agent.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// A workspace keeps a chat's file tools in its folder: relative paths are
/// inside it, a path outside is refused, the model is told what is there,
/// and only its owner uses it.
#[tokio::test]
async fn ai_workspaces_keep_a_chat_in_its_folder() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    server.create_user("bob", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let bob = server.login("bob", USER_PASSWORD).await;
    for name in ["alice", "bob"] {
        let id = server.user_id(name).await;
        let (status, _, body) = server
            .request(
                "PUT",
                &format!("/api/users/{id}/access-rules"),
                Some(json!({"folderRules": [], "appRules": [
                    {"appId": "agent", "rule": "allow"},
                    {"appId": "files", "rule": "allow"}
                ]})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (addr, log) = fake_provider();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({
                "kind": "custom",
                "name": "Local",
                "baseUrl": format!("http://{addr}/v1"),
                "apiKey": "sk-test-secret-1234",
                "models": ["tools"],
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/files/mkdir",
            Some(json!({"path": "/site"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A folder of Alice's files; a missing one and the server are refused.
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/workspaces",
            Some(json!({"path": "/site/"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workspace"]["name"], "site");
    assert_eq!(body["workspace"]["place"], "files");
    assert_eq!(body["workspace"]["path"], "/site");
    let workspace_id = body["workspace"]["id"].as_str().unwrap().to_string();
    for (folder, expected) in [
        (json!({"path": "/nowhere"}), StatusCode::BAD_REQUEST),
        (json!({"path": "/site/../etc"}), StatusCode::BAD_REQUEST),
        (
            json!({"place": "server", "path": "/tmp"}),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, _, body) = server
            .request(
                "POST",
                "/api/ai/workspaces",
                Some(folder),
                alice.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, expected, "{body}");
    }
    let (_, _, body) = server
        .request("GET", "/api/ai/workspaces", None, alice.headers())
        .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
    let (_, _, body) = server
        .request("GET", "/api/ai/workspaces", None, bob.headers())
        .await;
    assert_eq!(body["items"], json!([]));
    // Only its owner starts a chat in it.
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "tools", "workspaceId": workspace_id})),
            bob.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({
                "providerId": provider_id,
                "model": "tools",
                "workspaceId": workspace_id,
                "allowedTools": ["write_file", "read_file"],
            })),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["workspaceId"], workspace_id.as_str());
    let id = body["conversation"]["id"].as_str().unwrap().to_string();

    // A relative path lands inside the folder.
    let reply = ai_say(&server, &alice, &id, "a relative note").await;
    let calls = ai_tools(&reply);
    assert_eq!(calls[0]["status"], "done", "{reply}");
    assert_eq!(calls[0]["output"], "Created /site/plan.md (8 B).");
    let (status, _, body) = server
        .request(
            "GET",
            "/api/files/text?path=/site/plan.md",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"], "Step one");
    // The model is told about the workspace and what is at its top.
    let reply = ai_say(&server, &alice, &id, "an escape").await;
    let calls = ai_tools(&reply);
    assert_eq!(calls[0]["status"], "error", "{reply}");
    assert!(
        calls[0]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("/outside.txt is outside this chat's workspace \"site\" (/site)"),
        "{reply}"
    );
    let sent = log.lock().unwrap().last().unwrap().2.clone();
    let system = sent["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(system.contains("# Workspace"), "{system}");
    assert!(
        system.contains("works in the workspace \"site\": /site in their RAST files"),
        "{system}"
    );
    assert!(system.contains("At its top:\nplan.md"), "{system}");

    // Leaving it: the chat stays, outside any workspace.
    let (status, _, body) = server
        .request(
            "DELETE",
            &format!("/api/ai/workspaces/{workspace_id}"),
            None,
            bob.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _, body) = server
        .request(
            "DELETE",
            &format!("/api/ai/workspaces/{workspace_id}"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{id}"),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(body["conversation"]["workspaceId"], Value::Null, "{body}");
    let (status, _, _) = server
        .request(
            "GET",
            "/api/files/text?path=/site/plan.md",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the folder is untouched");
}

#[tokio::test]
async fn ai_agent_tools_ask_before_acting() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/users/{alice_id}/access-rules"),
            Some(json!({"folderRules": [], "appRules": [
                {"appId": "agent", "rule": "allow"},
                {"appId": "files", "rule": "allow"}
            ]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (addr, log) = fake_provider();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({
                "kind": "custom",
                "name": "Local",
                "baseUrl": format!("http://{addr}/v1"),
                "apiKey": "sk-test-secret-1234",
                "models": ["tools"],
            })),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "tools"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["conversation"]["id"].as_str().unwrap().to_string();
    let decide = |call: &str, decision: &str, always: bool| {
        (
            format!("/api/ai/conversations/{id}/tools/{call}"),
            json!({"decision": decision, "always": always}),
        )
    };

    // The model asks to write a file: nothing happens until Alice answers.
    let reply = ai_say(&server, &alice, &id, "please write a note").await;
    assert_eq!(reply["status"], "waiting", "{reply}");
    assert_eq!(ai_text(&reply), "On it.");
    let calls = ai_tools(&reply);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["name"], "write_file");
    assert_eq!(calls[0]["status"], "pending");
    assert_eq!(calls[0]["input"]["path"], "/hello.txt");
    let sent = log.lock().unwrap().last().unwrap().2.clone();
    let offered: Vec<&str> = sent["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert_eq!(
        offered,
        vec![
            "list_files",
            "read_file",
            "search_files",
            "write_file",
            "create_folder",
            "list_server_files",
            "read_server_file",
            "fetch_url",
            "artifact"
        ],
        "server reads (read-only), no shell or browser without Terminal, no web search without a service"
    );
    let (_, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{id}"),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(body["conversation"]["waiting"], true);
    let (status, _, _) = server
        .request(
            "GET",
            "/api/files/stat?path=/hello.txt",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "not written before it is allowed"
    );
    // Only the owner answers.
    let (path, answer) = decide("call_0_write_file", "allow", false);
    let (status, _, _) = server
        .request(
            "POST",
            &path,
            Some(answer.clone()),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(json!({"decision": "maybe"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            &path,
            Some(answer.clone()),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = ai_finished(&server, &alice, &id).await;
    let reply = body["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(reply["status"], "done", "{reply}");
    let calls = ai_tools(&reply);
    assert_eq!(calls[0]["status"], "done");
    assert_eq!(calls[0]["output"], "Created /hello.txt (17 B).");
    assert_eq!(
        ai_text(&reply),
        "On it.Tool said: Created /hello.txt (17 B)."
    );
    assert!(
        reply["inputTokens"].as_i64().unwrap() >= 20,
        "both steps counted: {reply}"
    );
    assert_eq!(body["conversation"]["waiting"], false);
    let (status, _, body) = server
        .request(
            "GET",
            "/api/files/text?path=/hello.txt",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"], "Hi from the agent");
    let sent = log.lock().unwrap().last().unwrap().2.clone();
    let turns = sent["messages"].as_array().unwrap();
    let assistant = &turns[turns.len() - 2];
    assert_eq!(assistant["tool_calls"][0]["id"], "call_0_write_file");
    assert_eq!(turns.last().unwrap()["tool_call_id"], "call_0_write_file");
    // A second answer to the same call is refused.
    let (status, _, _) = server
        .request("POST", &path, Some(answer), alice.headers_with_csrf())
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Denied: the model hears so.
    let reply = ai_say(&server, &alice, &id, "list my files").await;
    assert_eq!(reply["status"], "waiting");
    let (path, answer) = decide("call_0_list_files", "deny", false);
    let (status, _, body) = server
        .request("POST", &path, Some(answer), alice.headers_with_csrf())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = ai_finished(&server, &alice, &id).await;
    let reply = body["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(ai_tools(&reply)[0]["status"], "denied");
    assert!(
        ai_text(&reply).contains("Error: The person did not allow this call."),
        "{reply}"
    );

    // Allowed for the chat: the next list runs without asking.
    let reply = ai_say(&server, &alice, &id, "list my files").await;
    let (path, answer) = decide("call_0_list_files", "allow", true);
    assert_eq!(reply["status"], "waiting");
    let (status, _, body) = server
        .request("POST", &path, Some(answer), alice.headers_with_csrf())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["allowedTools"], json!(["list_files"]));
    ai_finished(&server, &alice, &id).await;
    let reply = ai_say(&server, &alice, &id, "list my files again").await;
    assert_eq!(reply["status"], "done", "{reply}");
    let calls = ai_tools(&reply);
    assert_eq!(calls[0]["status"], "done");
    assert!(
        calls[0]["output"].as_str().unwrap().contains("hello.txt"),
        "{reply}"
    );
    // Commands may run without asking too ("Allow everything"); an unknown
    // tool may not; Alice may go back to asking for all.
    let (status, _, body) = server
        .request(
            "PATCH",
            &format!("/api/ai/conversations/{id}"),
            Some(json!({"allowedTools": ["run_command"]})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["allowedTools"], json!(["run_command"]));
    let (status, _, _) = server
        .request(
            "PATCH",
            &format!("/api/ai/conversations/{id}"),
            Some(json!({"allowedTools": ["format_disk"]})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = server
        .request(
            "PATCH",
            &format!("/api/ai/conversations/{id}"),
            Some(json!({"allowedTools": []})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conversation"]["allowedTools"], json!([]));

    // Addresses on this server or private networks are not opened.
    let reply = ai_say(&server, &alice, &id, "fetch that page").await;
    let (path, answer) = decide("call_0_fetch_url", "allow", false);
    assert_eq!(reply["status"], "waiting");
    server
        .request("POST", &path, Some(answer), alice.headers_with_csrf())
        .await;
    let body = ai_finished(&server, &alice, &id).await;
    let reply = body["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    let calls = ai_tools(&reply);
    assert_eq!(calls[0]["status"], "error");
    assert!(
        calls[0]["error"]
            .as_str()
            .unwrap()
            .contains("private network"),
        "{reply}"
    );
    // A tool the account may not use fails without asking.
    let reply = ai_say(&server, &alice, &id, "use the shell").await;
    assert_eq!(reply["status"], "done", "{reply}");
    assert_eq!(ai_tools(&reply)[0]["status"], "error");

    // Stop while waiting: the call is skipped.
    let reply = ai_say(&server, &alice, &id, "write it again").await;
    assert_eq!(reply["status"], "waiting");
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{id}/stop"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], true);
    let (_, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{id}"),
            None,
            alice.headers(),
        )
        .await;
    let reply = body["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(reply["status"], "stopped");
    assert_eq!(ai_tools(&reply)[0]["status"], "skipped");

    // Writing instead of answering ends the request too; the model hears it.
    let reply = ai_say(&server, &alice, &id, "write once more").await;
    assert_eq!(reply["status"], "waiting");
    let reply = ai_say(&server, &alice, &id, "never mind").await;
    assert_eq!(ai_text(&reply), "Nothing to do.");
    let (_, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{id}"),
            None,
            alice.headers(),
        )
        .await;
    let messages = body["messages"].as_array().unwrap();
    let skipped = &messages[messages.len() - 3];
    assert_eq!(skipped["status"], "stopped");
    assert_eq!(ai_tools(skipped)[0]["status"], "skipped");
    let sent = log.lock().unwrap().last().unwrap().2.clone();
    let turns = sent["messages"].as_array().unwrap();
    assert!(turns.iter().any(|t| t["role"] == "tool"
        && t["content"]
            .as_str()
            .unwrap_or("")
            .contains("wrote a new message instead")));
    assert_eq!(turns.last().unwrap()["content"], "never mind");

    // An agent's tool list limits what the model is offered.
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/agents",
            Some(json!({"name": "Reader", "providerId": provider_id, "model": "tools", "tools": ["read_file", "list_files"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["agent"]["tools"], json!(["read_file", "list_files"]));
    let agent_id = body["agent"]["id"].as_str().unwrap().to_string();
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/agents",
            Some(json!({"name": "Bad", "tools": ["format_disk"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"agentId": agent_id})),
            alice.headers_with_csrf(),
        )
        .await;
    let reader = body["conversation"]["id"].as_str().unwrap().to_string();
    ai_say(&server, &alice, &reader, "hello").await;
    let sent = log.lock().unwrap().last().unwrap().2.clone();
    let offered: Vec<&str> = sent["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert_eq!(offered, vec!["list_files", "read_file"]);
    assert!(sent["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("# Tools"));

    // Web search settings: keys stay secret.
    let (status, _, body) = server
        .request(
            "PUT",
            "/api/ai/search",
            Some(json!({"kind": "brave", "apiKey": "brave-secret-9876"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["search"]["keyHint"], "…9876");
    assert_eq!(body["search"]["configured"], true);
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    assert!(!body.to_string().contains("brave-secret"));
    assert_eq!(body["search"]["kind"], "brave");
    let (status, _, _) = server
        .request(
            "PUT",
            "/api/ai/search",
            Some(json!({"kind": "searxng", "url": "nope"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = server
        .request(
            "PUT",
            "/api/ai/search",
            Some(json!({"kind": ""})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Artifacts show without asking; pages and pictures open sandboxed, for
    // whoever may read the chat.
    let (_, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "tools"})),
            alice.headers_with_csrf(),
        )
        .await;
    let shown = body["conversation"]["id"].as_str().unwrap().to_string();
    let reply = ai_say(&server, &alice, &shown, "show me artifacts").await;
    assert_eq!(reply["status"], "done", "{reply}");
    let calls = ai_tools(&reply);
    let statuses: Vec<&str> = calls
        .iter()
        .map(|c| c["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["done", "done", "done", "error"], "{reply}");
    assert!(ai_text(&reply).contains("in the artifact panel"), "{reply}");
    let artifact = |call: &str| format!("/api/ai/conversations/{shown}/artifacts/{call}");
    let (status, headers, page) = server
        .request("GET", &artifact("call_0_artifact"), None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let page = page.as_str().unwrap().to_string();
    assert!(
        page.starts_with("<!doctype html>") && page.contains("<h1>Counter</h1><script>"),
        "{page}"
    );
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(
        csp.starts_with("sandbox allow-scripts") && !csp.contains("allow-same-origin"),
        "{csp}"
    );
    assert!(
        csp.contains("frame-ancestors 'self'") && csp.contains("default-src 'none'"),
        "{csp}"
    );
    assert_eq!(headers[header::X_FRAME_OPTIONS], "SAMEORIGIN");
    let (status, _, page) = server
        .request("GET", &artifact("call_2_artifact"), None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.as_str().unwrap().contains("<circle r='4'/>"));
    for call in ["call_1_artifact", "call_3_artifact", "call_9_artifact"] {
        let (status, _, _) = server
            .request("GET", &artifact(call), None, alice.headers())
            .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{call}: documents show in the app; failed calls show nowhere"
        );
    }
    let (status, _, _) = server
        .request("GET", &artifact("call_0_artifact"), None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "administrators read every chat");
    server.create_user("bob", USER_PASSWORD, Role::User).await;
    let bob_id = server.user_id("bob").await;
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/users/{bob_id}/access-rules"),
            Some(json!({"folderRules": [], "appRules": [{"appId": "agent", "rule": "allow"}]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bob = server.login("bob", USER_PASSWORD).await;
    let (status, _, _) = server
        .request("GET", &artifact("call_0_artifact"), None, bob.headers())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "other accounts see nothing");
    let (status, _, _) = server
        .request("GET", &artifact("call_0_artifact"), None, HeaderMap::new())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

fn ai_text(message: &Value) -> String {
    message["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// Signing in with a plan (ChatGPT's Codex): only administrators start it,
/// a pasted address answers only the sign-in started by the same account,
/// and the provider it adds is its owner's alone.
#[tokio::test]
async fn ai_plan_sign_in_is_its_owners_alone() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server
        .create_user("admin2", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let admin2 = server.login("admin2", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let admin_id = server.user_id("admin").await;
    let alice_id = server.user_id("alice").await;
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/users/{alice_id}/access-rules"),
            Some(json!({"folderRules": [], "appRules": [{"appId": "agent", "rule": "allow"}]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Not added with a key.
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({"kind": "codex", "name": "Mine", "apiKey": "sk-x"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/models",
            Some(json!({"kind": "codex"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Starting: administrators only; OpenAI's page with PKCE.
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/providers/sign-in",
            Some(json!({"kind": "codex"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/sign-in",
            Some(json!({"kind": "openai"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/sign-in",
            Some(json!({"kind": "codex"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://auth.openai.com/oauth/authorize?"),
        "{url}"
    );
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    let state = body["state"].as_str().unwrap().to_string();
    assert!(url.contains(&format!("state={state}")));

    // Another account cannot answer it (and that does not use it up);
    // addresses without a code are refused before anything is sent.
    let callback = format!("http://localhost:1455/auth/callback?code=c1&state={state}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/sign-in/finish",
            Some(json!({"callback": callback})),
            admin2.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    for bad in [
        "https://chatgpt.com/".to_string(),
        format!("http://localhost:1455/auth/callback?error=access_denied&state={state}"),
    ] {
        let (status, _, body) = server
            .request(
                "POST",
                "/api/ai/providers/sign-in/finish",
                Some(json!({"callback": bad})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    // A signed-in plan (as finishing stores it).
    let credentials = rast_ai_agent::signin::Credentials {
        access_token: "at-secret-token".into(),
        refresh_token: "rt-secret-token".into(),
        expires_at: db::now_ms() + 3_600_000,
        refreshed_at: db::now_ms(),
        account_id: "acc-1".into(),
        email: "admin@example.com".into(),
        plan: "plus".into(),
        project_id: String::new(),
    };
    let sealed = rast_ai_agent::seal_credentials(&server.state.secrets, &credentials).unwrap();
    sqlx::query(
        "INSERT INTO ai_providers (id, kind, name, base_url, api_key, models, prices, list_prices,
             enabled, created_at, updated_at, owner_id, sign_in)
         VALUES ('plan1', 'codex', 'My ChatGPT', '', '', '[\"gpt-5.6-terra\"]', '{}', '{}', 1, 1, 1, ?, ?)",
    )
    .bind(admin_id)
    .bind(&sealed)
    .execute(&server.agent.db)
    .await
    .unwrap();
    // Default limits allow every provider; the owner check still applies.
    let offered = |body: &Value| -> bool {
        body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "plan1")
    };
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, admin.headers())
        .await;
    assert!(offered(&body), "{body}");
    for other in [&admin2, &alice] {
        let (status, _, body) = server
            .request("GET", "/api/ai/options", None, other.headers())
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!offered(&body), "{body}");
    }
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": "plan1", "model": "gpt-5.6-terra"})),
            admin2.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Administrators see who signed in, never the tokens; it keeps its own
    // address and takes no key.
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin2.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let plan = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "plan1")
        .unwrap()
        .clone();
    assert_eq!(plan["signsIn"], true);
    assert_eq!(plan["ownerId"], admin_id);
    assert_eq!(plan["signedIn"]["email"], "admin@example.com");
    assert_eq!(plan["signedIn"]["plan"], "plus");
    assert!(!body.to_string().contains("secret-token"), "{body}");
    assert!(body["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["id"] == "codex" && k["group"] == "plan"));
    let (status, _, body) = server
        .request(
            "PUT",
            "/api/ai/providers/plan1",
            Some(json!({"name": "Mine", "apiKey": "sk-x", "models": ["gpt-5.6-terra"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "PUT",
            "/api/ai/providers/plan1",
            Some(json!({"name": "Mine", "models": ["gpt-5.6-terra", "gpt-6.2-sol"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["signedIn"]["email"], "admin@example.com");

    // Its replies are paid for by the plan.
    let (status, _, body) = server
        .request("GET", "/api/ai/prices", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let models = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "plan1")
        .unwrap()["models"]
        .clone();
    assert_eq!(models[0]["source"], "plan", "{models}");
}

/// The plan kinds and what the settings page says about each: kind id, flow,
/// the kind's label and the site in the admin overview (the build contract's
/// table; the overview labels come from the kind list).
const AI_PLAN_KINDS: [(&str, &str, &str, &str); 9] = [
    ("codex", "browser", "ChatGPT Codex", "ChatGPT"),
    ("claude", "browser", "Claude", "Claude"),
    ("grok", "browser", "Grok (X Premium)", "xAI"),
    ("copilot", "device", "GitHub Copilot", "GitHub"),
    ("antigravity", "browser", "Google Antigravity", "Google"),
    ("gemini-cli", "browser", "Gemini CLI", "Google"),
    ("qwen-plan", "device", "Qwen", "Qwen"),
    ("iflow", "browser", "iFlow", "iFlow"),
    ("kimi-plan", "device", "Kimi", "Kimi"),
];

/// The browser-flow plans and the page each opens, with the kind's label and
/// site and whether its flow takes PKCE (iFlow's own flow has none: it ties
/// the pasted answer to the sign-in with its `state` alone). Starting one
/// only builds that page (no request to the provider), so tests may start
/// them; the device flows (Copilot, Qwen, Kimi) are not started because that
/// asks the provider for a code over the network.
/// The plan sign-ins whose OAuth client comes from the server's environment
/// (Antigravity, Gemini CLI, iFlow); the tests configure fake ones so no real
/// credential is needed.
fn set_plan_oauth_clients() {
    static SET: std::sync::Once = std::sync::Once::new();
    SET.call_once(|| {
        for (name, id) in [
            ("ANTIGRAVITY", "test-antigravity-client"),
            ("GEMINI_CLI", "test-gemini-client"),
            ("IFLOW", "test-iflow-id"),
        ] {
            std::env::set_var(format!("RAST_{name}_CLIENT_ID"), id);
            std::env::set_var(format!("RAST_{name}_CLIENT_SECRET"), "test-secret");
        }
    });
}

const AI_BROWSER_PLANS: [(&str, &str, &str, &str, bool); 6] = [
    (
        "codex",
        "https://auth.openai.com/",
        "ChatGPT",
        "ChatGPT",
        true,
    ),
    ("claude", "https://claude.ai/", "Claude", "Claude", true),
    (
        "grok",
        "https://auth.x.ai/",
        "Grok (X Premium)",
        "xAI",
        true,
    ),
    (
        "antigravity",
        "https://accounts.google.com/",
        "Google Antigravity",
        "Google",
        true,
    ),
    (
        "gemini-cli",
        "https://accounts.google.com/",
        "Gemini CLI",
        "Google",
        true,
    ),
    ("iflow", "https://iflow.cn/", "iFlow", "iFlow", false),
];

fn ai_kind<'a>(kinds: &'a [Value], id: &str) -> &'a Value {
    kinds
        .iter()
        .find(|kind| kind["id"] == id)
        .unwrap_or_else(|| panic!("kind {id} is not in the admin overview"))
}

/// A signed-in plan row, as finishing a sign-in stores it.
async fn seed_plan_provider(server: &TestServer, id: &str, kind: &str, owner_id: i64) {
    let credentials = rast_ai_agent::signin::Credentials {
        access_token: "at-secret-token".into(),
        refresh_token: "rt-secret-token".into(),
        expires_at: db::now_ms() + 3_600_000,
        refreshed_at: db::now_ms(),
        account_id: "acc-1".into(),
        email: "admin@example.com".into(),
        plan: "plus".into(),
        project_id: String::new(),
    };
    let sealed = rast_ai_agent::seal_credentials(&server.state.secrets, &credentials).unwrap();
    sqlx::query(
        "INSERT INTO ai_providers (id, kind, name, base_url, api_key, models, prices, list_prices,
             enabled, created_at, updated_at, owner_id, sign_in)
         VALUES (?, ?, 'My plan', '', '', '[\"plan-model\"]', '{}', '{}', 1, 1, 1, ?, ?)",
    )
    .bind(id)
    .bind(kind)
    .bind(owner_id)
    .bind(&sealed)
    .execute(&server.agent.db)
    .await
    .unwrap();
}

/// Whether this machine has a Claude Code login the server would import (the
/// same places `signin::import_claude_code` looks). A test must never spend a
/// real one: it would use that person's tokens and call Anthropic.
fn claude_code_login_on_host() -> Option<std::path::PathBuf> {
    let path = if let Some(overridden) =
        std::env::var_os("RAST_CLAUDE_CREDENTIALS").filter(|path| !path.is_empty())
    {
        std::path::PathBuf::from(overridden)
    } else if let Some(config_dir) =
        std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty())
    {
        std::path::PathBuf::from(config_dir).join(".credentials.json")
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME").filter(|home| !home.is_empty())?)
            .join(".claude")
            .join(".credentials.json")
    };
    path.is_file().then_some(path)
}

/// Browser flows come back to a page on the person's own machine: the paste
/// is the only way that address reaches a server.
fn assert_loopback_redirect(redirect: &str) {
    assert!(
        redirect.starts_with("http://localhost:") || redirect.starts_with("http://127.0.0.1:"),
        "a pasted flow comes back to a loopback address: {redirect}"
    );
}

/// Install the AI Agent app (it ships with RAST but starts uninstalled, and
/// every `/api/ai/*` call needs it) and optionally grant an account access.
/// The last message of the conversation once `ready` says yes.
async fn ai_until(
    server: &TestServer,
    session: &Session,
    id: &str,
    what: &str,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..300 {
        let (status, _, body) = server
            .request(
                "GET",
                &format!("/api/ai/conversations/{id}"),
                None,
                session.headers(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        last = body["messages"]
            .as_array()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default();
        if ready(&last) {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the reply never got to: {what}; it is {last}");
}

/// A reply whose provider fails in passing (busy, broken off) waits and
/// tries again, up to 10 times, showing the countdown; Retry now cuts the
/// wait short, Stop ends it, and what a failed try wrote is taken back.
#[tokio::test]
async fn ai_replies_try_again_after_passing_failures() {
    let server = TestServer::start_retrying_after(std::time::Duration::from_secs(60)).await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    let (addr, log) = fake_provider();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(
                json!({"kind": "custom", "name": "Local", "baseUrl": format!("http://{addr}/v1"),
                        "apiKey": "sk-test-secret-1234", "models": ["flaky", "down"]}),
            ),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let chat = |model: &'static str| {
        let (server, alice, provider_id) = (&server, &alice, provider_id.clone());
        async move {
            let (status, _, body) = server
                .request(
                    "POST",
                    "/api/ai/conversations",
                    Some(json!({"providerId": provider_id, "model": model})),
                    alice.headers_with_csrf(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let id = body["conversation"]["id"].as_str().unwrap().to_string();
            let (status, _, body) = server
                .request(
                    "POST",
                    &format!("/api/ai/conversations/{id}/messages"),
                    Some(json!({"text": "hello"})),
                    alice.headers_with_csrf(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            id
        }
    };
    let retry = |id: String| {
        let (server, alice) = (&server, &alice);
        async move {
            let (status, _, body) = server
                .request(
                    "POST",
                    &format!("/api/ai/conversations/{id}/retry"),
                    None,
                    alice.headers_with_csrf(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["retrying"].as_bool().unwrap()
        }
    };

    // Busy: the reply waits for try 1 of 10, with the time left.
    let flaky = chat("flaky").await;
    let waiting = ai_until(&server, &alice, &flaky, "try 1", |m| {
        m["retry"]["attempt"] == 1
    })
    .await;
    assert_eq!(waiting["status"], "streaming", "{waiting}");
    assert_eq!(waiting["retry"]["of"], 10, "{waiting}");
    let left = waiting["retry"]["inMs"].as_i64().unwrap();
    assert!(left > 50_000 && left <= 60_000, "{waiting}");
    assert!(
        waiting["retry"]["error"].as_str().unwrap().contains("503"),
        "{waiting}"
    );
    // Retry now: the second request writes "Half " and breaks off.
    assert!(retry(flaky.clone()).await);
    let again = ai_until(&server, &alice, &flaky, "try 2", |m| {
        m["retry"]["attempt"] == 2
    })
    .await;
    assert!(
        again["retry"]["error"]
            .as_str()
            .unwrap()
            .contains("Overloaded"),
        "{again}"
    );
    assert!(
        !again["content"].to_string().contains("Half"),
        "what the broken try wrote is taken back: {again}"
    );
    assert!(retry(flaky.clone()).await);
    let done = ai_finished(&server, &alice, &flaky).await;
    let reply = done["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(reply["status"], "done", "{reply}");
    assert!(reply.get("retry").is_none(), "{reply}");
    let text = reply["content"].to_string();
    assert!(
        text.contains("Ahoy there!") && !text.contains("Half"),
        "{reply}"
    );
    assert!(!retry(flaky.clone()).await, "nothing waits any more");
    let tries = |model: &str| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|(line, _, body)| line.starts_with("POST") && body["model"] == model)
            .count()
    };
    assert_eq!(tries("flaky"), 3);

    // Stop ends the wait.
    let down = chat("down").await;
    ai_until(&server, &alice, &down, "try 1", |m| {
        m["retry"]["attempt"] == 1
    })
    .await;
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{down}/stop"),
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stopped = ai_until(&server, &alice, &down, "stopped", |m| {
        m["status"] != "streaming"
    })
    .await;
    assert_eq!(stopped["status"], "stopped", "{stopped}");
    assert_eq!(tries("down"), 1);

    // Busy every time: 10 tries again, then the reply says so.
    let quick = TestServer::start_retrying_after(std::time::Duration::from_millis(20)).await;
    quick
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    let admin = quick.login("admin", ADMIN_PASSWORD).await;
    install_ai_agent_app(&quick, &admin, None).await;
    let (addr, log) = fake_provider();
    let (status, _, body) = quick
        .request(
            "POST",
            "/api/ai/providers",
            Some(
                json!({"kind": "custom", "name": "Local", "baseUrl": format!("http://{addr}/v1"),
                        "apiKey": "sk-test-secret-1234", "models": ["down"]}),
            ),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = quick
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "down"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["conversation"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = quick
        .request(
            "POST",
            &format!("/api/ai/conversations/{id}/messages"),
            Some(json!({"text": "hello"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ended = ai_finished(&quick, &admin, &id).await;
    let reply = ended["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(reply["status"], "error", "{reply}");
    let error = reply["error"].as_str().unwrap();
    assert!(error.contains("tried again 10 times"), "{error}");
    let requests = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(line, _, _)| line.starts_with("POST"))
        .count();
    assert_eq!(requests, 11, "the first request and 10 tries again");
}

/// The viewer beside the chat: media on the web comes through the server
/// from public places only; a PDF sent in the chat opens inline, sandboxed.
#[tokio::test]
async fn ai_viewer_opens_media_from_public_places_and_chat_pdfs_inline() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;

    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view?url=https%3A%2F%2Fexample.com%2Fa.png",
            None,
            HeaderMap::new(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = server
        .request("GET", "/api/ai/view", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for (url, says) in [
        ("http://127.0.0.1:9/a.png", "private network"),
        ("http://localhost/a.pdf", "private network"),
        ("http://10.1.2.3/clip.mp4", "private network"),
        ("ftp://example.com/a.png", ""),
    ] {
        let path = format!(
            "/api/ai/view?url={}",
            url.replace(':', "%3A").replace('/', "%2F")
        );
        let (status, _, body) = server.request("GET", &path, None, alice.headers()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains(says),
            "{url}: {body}"
        );
    }

    let (status, body) = server
        .request_raw(
            "POST",
            "/api/ai/files?name=scan.pdf",
            b"%PDF-1.7\nx".to_vec(),
            "application/pdf",
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let file: Value = serde_json::from_slice(&body).unwrap();
    let (status, headers, _) = server
        .request(
            "GET",
            &format!("/api/ai/files/{}", file["id"].as_str().unwrap()),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let header = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(header(header::CONTENT_TYPE), "application/pdf");
    assert!(header(header::CONTENT_DISPOSITION).starts_with("inline;"));
    assert!(header(header::CONTENT_SECURITY_POLICY).contains("sandbox"));
    assert_eq!(header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
}

#[tokio::test]
async fn ai_panel_shows_html_files_walled_off_and_checks_documents_and_pages() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    server.create_user("bob", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let bob = server.login("bob", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;

    // An HTML file in Alice's files shows as a page under the artifacts'
    // policy: scripts in an opaque origin, no RAST API.
    let (status, _) = server
        .request_raw(
            "POST",
            "/api/files/upload?dir=%2F&name=demo.html",
            b"<!doctype html><h1>Hi</h1><script>1</script>".to_vec(),
            "application/octet-stream",
            alice.headers_with_csrf(),
        )
        .await;
    assert!(status.is_success(), "{status}");
    let (status, headers, _) = server
        .request(
            "GET",
            "/api/ai/view/html?path=%2Fdemo.html",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let header = |headers: &HeaderMap, name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(
        header(&headers, header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    let policy = header(&headers, header::CONTENT_SECURITY_POLICY);
    assert!(policy.starts_with("sandbox allow-scripts"), "{policy}");
    assert!(!policy.contains("allow-same-origin"), "{policy}");
    assert!(policy.contains("frame-ancestors 'self'"), "{policy}");
    // Only .html, for those with AI Agent.
    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view/html?path=%2Fnotes.txt",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view/html?path=%2Fdemo.html",
            None,
            bob.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "Bob has no AI Agent");
    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view/html?path=%2Fdemo.html",
            None,
            HeaderMap::new(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Documents: only the kinds LibreOffice opens, with their contents.
    for (name, bytes, says) in [
        ("notes.txt", b"hello".to_vec(), "Word, spreadsheet, slides"),
        (
            "plan.docx",
            b"<html>not a document</html>".to_vec(),
            "no .docx document inside",
        ),
    ] {
        let (status, body) = server
            .request_raw(
                "POST",
                &format!("/api/ai/view/convert?name={name}"),
                bytes,
                "application/octet-stream",
                alice.headers_with_csrf(),
            )
            .await;
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains(says),
            "{name}: {body}"
        );
    }
    let (status, _) = server
        .request_raw(
            "POST",
            "/api/ai/view/convert?name=plan.docx",
            b"PK\x03\x04".to_vec(),
            "application/octet-stream",
            alice.headers(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a change needs the CSRF token"
    );
    // Converted pages are named by their hash, and kept per account.
    for id in ["../../etc/passwd", "abc", &"A".repeat(64), &"a".repeat(64)] {
        let (status, _, _) = server
            .request(
                "GET",
                &format!("/api/ai/view/converted/{}", id.replace('/', "%2F")),
                None,
                alice.headers(),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }

    // A page's details: public addresses only.
    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view/page?url=https%3A%2F%2Fexample.com%2F",
            None,
            HeaderMap::new(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for url in [
        "http://127.0.0.1:9/",
        "http://localhost/",
        "http://192.168.1.1/admin",
    ] {
        let path = format!(
            "/api/ai/view/page?url={}",
            url.replace(':', "%3A").replace('/', "%2F")
        );
        let (status, _, body) = server.request("GET", &path, None, alice.headers()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("private network"),
            "{url}: {body}"
        );
    }
    // Text and documents from the web: `as` names the kind.
    let (status, _, _) = server
        .request(
            "GET",
            "/api/ai/view?url=https%3A%2F%2Fexample.com%2Fa.md&as=music",
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn install_ai_agent_app(server: &TestServer, admin: &Session, user_id: Option<i64>) {
    if let Some(user_id) = user_id {
        let (status, _, body) = server
            .request(
                "PUT",
                &format!("/api/users/{user_id}/access-rules"),
                Some(json!({"folderRules": [], "appRules": [{"appId": "agent", "rule": "allow"}]})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "grant the AI Agent app: {body}");
    }
    let (status, _, body) = server
        .request(
            "POST",
            "/api/packages/apps/agent/install",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "install the AI Agent app: {body}");
}

/// The admin overview tells the settings page how each plan signs in and
/// what to show about it, and keyed kinds (including `qwen` and `moonshot`,
/// whose plan counterparts are `qwen-plan` and `kimi-plan`) sign in not at
/// all.
#[tokio::test]
async fn ai_plan_kinds_say_how_they_sign_in() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    install_ai_agent_app(&server, &admin, None).await;
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let kinds = body["kinds"].as_array().expect("kinds");
    assert_eq!(
        kinds.iter().filter(|kind| kind["signsIn"] == true).count(),
        AI_PLAN_KINDS.len(),
        "the signing-in kinds are the nine plans: {kinds:?}"
    );
    for (id, flow, label, site) in AI_PLAN_KINDS {
        let kind = ai_kind(kinds, id);
        assert_eq!(kind["group"], "plan", "{kind}");
        assert_eq!(kind["signsIn"], true, "{kind}");
        assert_eq!(kind["signInFlow"], flow, "{kind}");
        assert_eq!(kind["label"], label, "{kind}");
        assert_eq!(kind["site"], site, "{kind}");
        assert!(
            kind["risk"].as_str().is_some_and(|risk| !risk.is_empty()),
            "a plan says what it risks: {kind}"
        );
        if flow == "browser" {
            assert!(
                kind["callbackHint"]
                    .as_str()
                    .is_some_and(|hint| !hint.is_empty()),
                "a browser flow says what to paste back: {kind}"
            );
        } else {
            assert_eq!(
                kind["callbackHint"], "",
                "a device flow pastes nothing: {kind}"
            );
        }
    }
    for id in ["openai", "qwen", "moonshot", "custom", "ollama"] {
        let kind = ai_kind(kinds, id);
        assert_eq!(kind["signsIn"], false, "{kind}");
        assert_eq!(kind["signInFlow"], "", "{kind}");
        assert_eq!(kind["site"], "", "{kind}");
        assert_eq!(kind["callbackHint"], "", "{kind}");
        assert_eq!(kind["risk"], "", "{kind}");
    }
}

/// Starting a browser-flow plan builds the provider's page offline: PKCE
/// S256, the state echoed, and the loopback address the person pastes back.
#[tokio::test]
async fn ai_plan_sign_in_builds_every_browser_page() {
    set_plan_oauth_clients();
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let admin_id = server.user_id("admin").await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    let start = "/api/ai/providers/sign-in";

    // Administrators only.
    let (status, _, _) = server
        .request(
            "POST",
            start,
            Some(json!({"kind": "claude"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Keyed and unknown kinds do not sign in.
    for kind in ["openai", "qwen", "moonshot", "custom", "", "nonsense"] {
        let (status, _, body) = server
            .request(
                "POST",
                start,
                Some(json!({"kind": kind})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "kind {kind:?}: {body}");
    }

    for (kind, host, label, site, pkce) in AI_BROWSER_PLANS {
        let (status, _, body) = server
            .request(
                "POST",
                start,
                Some(json!({"kind": kind})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "kind {kind}: {body}");
        assert_eq!(body["flow"], "browser", "kind {kind}: {body}");
        assert_eq!(body["label"], label, "kind {kind}: {body}");
        assert_eq!(body["site"], site, "kind {kind}: {body}");
        assert!(
            body["callbackHint"]
                .as_str()
                .is_some_and(|hint| !hint.is_empty()),
            "kind {kind}: {body}"
        );
        assert!(
            body["risk"].as_str().is_some_and(|risk| !risk.is_empty()),
            "kind {kind}: {body}"
        );
        let url = body["url"]
            .as_str()
            .unwrap_or_else(|| panic!("kind {kind}: no url: {body}"));
        assert!(
            url.starts_with(host),
            "kind {kind} signs in on {host}: {url}"
        );
        if pkce {
            assert!(url.contains("response_type=code"), "kind {kind}: {url}");
            assert!(url.contains("code_challenge="), "kind {kind}: {url}");
            assert!(
                url.contains("code_challenge_method=S256"),
                "kind {kind}: {url}"
            );
        }
        let state = body["state"]
            .as_str()
            .unwrap_or_else(|| panic!("kind {kind}: no state: {body}"));
        assert!(
            url.contains(&format!("state={state}")),
            "kind {kind}: {url}"
        );
        assert_loopback_redirect(
            body["redirectUri"]
                .as_str()
                .unwrap_or_else(|| panic!("kind {kind}: no redirectUri: {body}")),
        );
    }

    // Signing in again to a provider: it must exist and be of that kind.
    let (status, _, body) = server
        .request(
            "POST",
            start,
            Some(json!({"kind": "codex", "providerId": "no-such-provider"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    seed_plan_provider(&server, "claude-row", "claude", admin_id).await;
    let (status, _, body) = server
        .request(
            "POST",
            start,
            Some(json!({"kind": "codex", "providerId": "claude-row"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            start,
            Some(json!({"kind": "claude", "providerId": "claude-row"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "signing in again to a Claude provider: {body}"
    );
    assert_eq!(body["flow"], "browser", "{body}");

    // The device kinds are routed (they get past the kind check to the
    // provider lookup) but are not started here: their `start` asks the
    // provider for a code, which would be a network request.
    for plan in AI_PLAN_KINDS {
        if plan.1 != "device" {
            continue;
        }
        let (status, _, body) = server
            .request(
                "POST",
                start,
                Some(json!({"kind": plan.0, "providerId": "no-such-provider"})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "kind {}: {body}", plan.0);
    }
}

/// Answering a sign-in: only administrators, only the account that started
/// it, only once, and only with an address that carries its code.
#[tokio::test]
async fn ai_plan_sign_in_is_answered_once_by_its_owner() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server
        .create_user("admin2", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let admin2 = server.login("admin2", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let admin_id = server.user_id("admin").await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    let (finish, poll) = (
        "/api/ai/providers/sign-in/finish",
        "/api/ai/providers/sign-in/poll",
    );

    // A Claude sign-in, started but not answered (offline: it is the page).
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/sign-in",
            Some(json!({"kind": "claude"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let state = body["state"].as_str().unwrap().to_string();
    let redirect = body["redirectUri"].as_str().unwrap().to_string();
    let callback = format!("{redirect}?code=test-code&state={state}");

    // Only administrators answer or poll a sign-in.
    let (status, _, body) = server
        .request(
            "POST",
            finish,
            Some(json!({"callback": callback})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            poll,
            Some(json!({"state": state})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // A browser sign-in is not polled, and an unknown state names nothing.
    let (status, _, body) = server
        .request(
            "POST",
            poll,
            Some(json!({"state": state})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            poll,
            Some(json!({"state": "not-a-state"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Another administrator cannot answer it, and trying does not use it up.
    let (status, _, body) = server
        .request(
            "POST",
            finish,
            Some(json!({"callback": callback})),
            admin2.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Addresses without a code, refusals and states that name no sign-in are
    // refused before the provider is asked anything.
    for bad in [
        "https://claude.ai/".to_string(),
        format!("{redirect}?error=access_denied&state={state}"),
        format!("{redirect}?code=test-code&state=not-a-state"),
        format!("{redirect}?state={state}"),
        "a-code-without-a-state#".to_string(),
    ] {
        let (status, _, body) = server
            .request(
                "POST",
                finish,
                Some(json!({"callback": bad})),
                admin.headers_with_csrf(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _, body) = server
        .request(
            "POST",
            finish,
            Some(json!({"callback": "x".repeat(8193)})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a giant paste: {body}");

    // The answer is used once. Finishing for real would trade the code over
    // the network, which tests must not do, so the used-up state is reached
    // directly: pasting that address again names nothing.
    let used = rast_ai_agent::signin::take_pending(&state, admin_id).expect("the started sign-in");
    assert_eq!(used.plan, rast_ai_agent::signin::Plan::Claude);
    let (status, _, body) = server
        .request(
            "POST",
            finish,
            Some(json!({"callback": callback})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Signing out forgets a plan's tokens and owner, keeping the provider and
/// its models; only administrators do it, and only for plans.
#[tokio::test]
async fn ai_plan_sign_out_forgets_the_tokens() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server
        .create_user("admin2", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let admin2 = server.login("admin2", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let admin_id = server.user_id("admin").await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    seed_plan_provider(&server, "plan1", "codex", admin_id).await;
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({"kind": "openai", "name": "Keyed", "apiKey": "sk-test-1234", "models": ["gpt-4o"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keyed_id = body["provider"]["id"].as_str().unwrap().to_string();

    // Only administrators, only existing providers, only plans.
    let (status, _, _) = server
        .request(
            "POST",
            "/api/ai/providers/plan1/sign-out",
            None,
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/no-such-provider/sign-out",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/providers/{keyed_id}/sign-out"),
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request", "{body}");

    // Before: it is a signed-in provider its owner may chat with.
    let (_, _, body) = server
        .request("GET", "/api/ai/options", None, admin.headers())
        .await;
    assert!(
        body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "plan1"),
        "{body}"
    );
    let (_, _, body) = server
        .request("GET", "/api/ai/admin/overview", None, admin.headers())
        .await;
    let signed_in = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "plan1")
        .unwrap()["signedIn"]
        .clone();
    assert_eq!(signed_in["email"], "admin@example.com", "{signed_in}");
    assert_eq!(signed_in["plan"], "plus", "{signed_in}");

    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers/plan1/sign-out",
            None,
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider = &body["provider"];
    assert_eq!(provider["id"], "plan1", "{body}");
    assert_eq!(provider["signsIn"], true, "{body}");
    assert_eq!(provider["signedIn"], Value::Null, "{body}");
    assert_eq!(provider["ownerId"], Value::Null, "{body}");
    assert_eq!(provider["models"], json!(["plan-model"]), "{body}");
    assert!(!body.to_string().contains("secret-token"), "{body}");

    // The tokens are gone and so is the owner; in the database too.
    let sign_in: String = sqlx::query_scalar("SELECT sign_in FROM ai_providers WHERE id = 'plan1'")
        .fetch_one(&server.agent.db)
        .await
        .unwrap();
    assert_eq!(sign_in, "");
    let owner: Option<i64> =
        sqlx::query_scalar("SELECT owner_id FROM ai_providers WHERE id = 'plan1'")
            .fetch_one(&server.agent.db)
            .await
            .unwrap();
    assert_eq!(owner, None);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'ai.provider.sign_out' AND object_id = 'plan1'",
    )
    .fetch_one(&server.state.db)
    .await
    .unwrap();
    assert_eq!(audited, 1, "signing out is in the activity log");

    // Nobody may chat with it until someone signs in again (its new owner).
    for session in [&admin, &admin2, &alice] {
        let (status, _, body) = server
            .request("GET", "/api/ai/options", None, session.headers())
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            !body["providers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == "plan1"),
            "{body}"
        );
    }
}

/// Importing the server's own Claude Code login: administrators only, a
/// provider to sign in again must be a Claude one, and a missing login says
/// so. This machine may have a real Claude Code login: importing it would
/// spend that person's tokens and call Anthropic, so the test only checks
/// the paths that fail before that file is read.
#[tokio::test]
async fn ai_claude_code_import_is_administrators_only_and_validated() {
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    let import = "/api/ai/providers/import/claude-code";

    // Administrators only.
    let (status, _, _) = server
        .request("POST", import, Some(json!({})), alice.headers_with_csrf())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Unknown provider: 404; a provider of another kind: 400.
    let (status, _, body) = server
        .request(
            "POST",
            import,
            Some(json!({"providerId": "no-such-provider"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(json!({"kind": "openai", "name": "Keyed", "apiKey": "sk-test-1234", "models": ["gpt-4o"]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keyed_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request(
            "POST",
            import,
            Some(json!({"providerId": keyed_id})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // A name that is too long is refused before the login file is read.
    let (status, _, body) = server
        .request(
            "POST",
            import,
            Some(json!({"name": "x".repeat(rast_ai_agent::MAX_NAME + 1)})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    match claude_code_login_on_host() {
        // No login on this machine: the import says so and adds nothing.
        None => {
            let (status, _, body) = server
                .request("POST", import, Some(json!({})), admin.headers_with_csrf())
                .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "no Claude Code login on this machine: {body}"
            );
            assert_eq!(body["error"]["code"], "bad_request", "{body}");
            let providers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_providers")
                .fetch_one(&server.agent.db)
                .await
                .unwrap();
            assert_eq!(providers, 1, "only the keyed provider was added");
        }
        // A real login is on this machine: do not spend it.
        Some(path) => eprintln!("not importing {path:?}: a real Claude Code login exists"),
    }
}

/// A Package Center app for the tests: a Python server on its socket that
/// says what it was asked and asks the host API back.
const DEMO_APP_SERVER: &str = r#"#!/usr/bin/python3
import http.client, http.server, json, os, socket, socketserver, sys

class Host(http.client.HTTPConnection):
    def __init__(self):
        super().__init__("rast-host")
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(os.environ["RAST_HOST_SOCKET"])

def host(method, path, body=None, token=None):
    conn = Host()
    headers = {"Authorization": "Bearer " + (token or os.environ["RAST_APP_TOKEN"])}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    conn.request(method, path, body=data, headers=headers)
    response = conn.getresponse()
    text = response.read().decode()
    try:
        return response.status, (json.loads(text) if text else None)
    except ValueError:
        return response.status, text

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *args):
        pass
    def answer(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Set-Cookie", "app=1")
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        try:
            self.get()
        except Exception:
            import traceback
            self.answer({"crash": traceback.format_exc()}, 500)
    def get(self):
        if self.path == "/api/demo/exit":
            self.answer({"bye": True})
            self.wfile.flush()
            os._exit(3)
        user = json.loads(self.headers.get("X-Rast-User", "null"))
        info = host("GET", "/v1/info")
        users = host("GET", "/v1/users")
        access = host("GET", "/v1/users/%d/apps/files" % user["id"])
        sent = host("POST", "/v1/events", {"kind": "demo.ping", "scope": {"to": "user", "userId": user["id"]}, "data": {"n": 1}})
        foreign = host("POST", "/v1/events", {"kind": "files.changed", "scope": {"to": "all"}})
        audit = host("POST", "/v1/audit", {"action": "demo.visit", "userId": user["id"], "username": user["username"], "detail": "hello"})
        wrong = host("GET", "/v1/info", token="nope")
        self.answer({
            "path": self.path,
            "user": user,
            "token": self.headers.get("X-Rast-Token") == os.environ["RAST_APP_TOKEN"],
            "cookie": self.headers.get("Cookie"),
            "madeUp": self.headers.get("X-Rast-Admin"),
            "ip": self.headers.get("X-Rast-Client-Ip"),
            "info": info,
            "users": users,
            "access": access,
            "sent": sent,
            "foreign": foreign[0],
            "audit": audit[0],
            "wrongToken": wrong[0],
            "pid": os.getpid(),
        })
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        self.answer({"echo": json.loads(self.rfile.read(length))}, 201)

class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

path = os.environ["RAST_APP_SOCKET"]
server = Server(path, Handler)
os.chmod(path, 0o600)
server.serve_forever()
"#;

#[tokio::test]
async fn package_center_apps_run_behind_rast() {
    use std::os::unix::fs::PermissionsExt;
    let server = TestServer::start().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;

    // Installed as Package Center would: files in place, a version recorded
    // and made the default.
    let root = server
        .state
        .config
        .packages_dir()
        .join("runtimes/demo-app/1.0.0");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::write(root.join("bin/server"), DEMO_APP_SERVER).unwrap();
    std::fs::set_permissions(
        root.join("bin/server"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(root.join("ui/app.js"), "export default { mount() {} };\n").unwrap();
    std::fs::write(root.join("ui/app.css"), ".app-demo {}\n").unwrap();
    let manifest: rast::packages::manifest::Manifest = serde_json::from_value(json!({
        "schemaVersion": 1, "kind": "rast-package", "id": "demo-app", "type": "app",
        "name": "Demo", "description": "A test app",
        "publisher": {"id": "rast", "name": "RAST"},
        "version": "1.0.0", "revision": 1,
        "app": {
            "id": "demo",
            "window": {"entry": "ui/app.js", "styles": ["ui/app.css"], "sdk": 1},
            "server": {"executable": "bin/server", "hostApi": 1, "apiPrefix": "/api/demo", "eventPrefix": "demo."}
        },
        "artifacts": [{
            "target": "linux-x86_64", "url": "https://example.com/demo.tar.gz", "sha256": "a".repeat(64),
            "size": 1, "format": "tar.gz", "provenance": {"kind": "built", "source": "https://example.com"}
        }]
    }))
    .unwrap();
    rast::packages::store::record_install(
        &server.state.db,
        rast::packages::store::NewVersion {
            manifest: &manifest,
            source: "official",
            source_repo: None,
            target: "linux-x86_64",
            artifact_url: "https://example.com/demo.tar.gz",
            release_tag: None,
            install_path: &root.display().to_string(),
            size_bytes: 1,
            check_output: "",
            installed_by: "admin",
            artifact_sha256: &"a".repeat(64),
            installed_at: None,
        },
    )
    .await
    .unwrap();
    rast::packages::store::set_default(&server.state.db, "demo-app", Some("1.0.0"))
        .await
        .unwrap();
    server.state.apps.sync(&server.state).await;

    // Its API answers through RAST, saying who asks.
    let (status, headers, body) = server
        .request("GET", "/api/demo/hello?x=1", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path"], "/api/demo/hello?x=1");
    assert_eq!(body["user"]["username"], "admin");
    assert_eq!(body["user"]["admin"], true);
    assert_eq!(body["token"], true);
    assert_eq!(
        body["cookie"],
        Value::Null,
        "the session cookie stays with RAST"
    );
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "an app sets no cookies"
    );
    assert_eq!(body["info"][1]["appId"], "demo");
    assert_eq!(body["info"][1]["hostApi"], 1);
    let names: Vec<&str> = body["users"][1]["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["admin", "alice"]);
    assert_eq!(body["access"][1]["allowed"], true);
    assert_eq!(body["sent"][0], 200);
    assert_eq!(body["foreign"], 400, "events of its own kinds only");
    assert_eq!(body["audit"], 200);
    assert_eq!(body["wrongToken"], 401);
    let first_pid = body["pid"].as_i64().unwrap();
    let (_, _, logs) = server
        .request("GET", "/api/logs?action=demo.visit", None, admin.headers())
        .await;
    assert!(logs.to_string().contains("demo.visit"), "{logs}");

    // Bodies go through; changes need RAST's CSRF token like any other.
    let (status, _, body) = server
        .request(
            "POST",
            "/api/demo/echo",
            Some(json!({"hello": "app"})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["echo"]["hello"], "app");
    let (status, _, _) = server
        .request("POST", "/api/demo/echo", Some(json!({})), admin.headers())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "CSRF");
    let (status, _, _) = server
        .request("GET", "/api/demo/hello", None, HeaderMap::new())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An app RAST has no access rules for is for administrators.
    let (status, _, _) = server
        .request("GET", "/api/demo/hello", None, alice.headers())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, listed) = server
        .request("GET", "/api/appfiles", None, alice.headers())
        .await;
    assert_eq!(listed["apps"], json!([]));

    // Its window, for the desktop.
    let (_, _, listed) = server
        .request("GET", "/api/appfiles", None, admin.headers())
        .await;
    let window = &listed["apps"][0];
    assert_eq!(window["appId"], "demo");
    assert_eq!(window["entry"], "/api/appfiles/demo/1.0.0/1/ui/app.js");
    assert_eq!(window["styles"][0], "/api/appfiles/demo/1.0.0/1/ui/app.css");
    assert_eq!(window["server"]["state"], "running");
    let (status, headers, body) = server
        .request(
            "GET",
            "/api/appfiles/demo/1.0.0/1/ui/app.js",
            None,
            admin.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_str().unwrap().contains("export default"));
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    assert!(headers[header::CACHE_CONTROL]
        .to_str()
        .unwrap()
        .contains("immutable"));
    for path in [
        "/api/appfiles/demo/1.0.0/1/bin/server",
        "/api/appfiles/demo/1.0.0/1/ui/../bin/server",
        "/api/appfiles/demo/0.9.0/1/ui/app.js",
        "/api/appfiles/demo/1.0.0/1/ui/missing.js",
    ] {
        let (status, _, _) = server.request("GET", path, None, admin.headers()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }

    // A server that stops is started again.
    let (status, _, _) = server
        .request("GET", "/api/demo/exit", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut again = Value::Null;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let (status, _, body) = server
            .request("GET", "/api/demo/hello", None, admin.headers())
            .await;
        if status == StatusCode::OK {
            again = body;
            break;
        }
    }
    assert!(
        again["pid"].as_i64().is_some_and(|pid| pid != first_pid),
        "{again}"
    );
    assert_eq!(server.state.apps.get("demo").unwrap().status().restarts, 1);

    // Removed: stopped, its paths are RAST's again.
    rast::packages::store::set_default(&server.state.db, "demo-app", None)
        .await
        .unwrap();
    server.state.apps.sync(&server.state).await;
    let (status, _, _) = server
        .request("GET", "/api/demo/hello", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let pid = again["pid"].as_i64().unwrap();
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "the server process ended"
    );
}

/// AI Agent from its Package Center app package: the real `rast-ai-agent`
/// program behind RAST. It takes RAST's AI Agent data once (chats, the
/// provider with its key sealed again with the app's own key, chat files),
/// then answers `/api/ai/*` with its own database, asking RAST's host API
/// for accounts and File Station.
#[tokio::test]
// App packages install on Linux servers (their checks run only there).
#[cfg_attr(not(target_os = "linux"), ignore = "app packages run on Linux servers")]
async fn ai_agent_runs_from_its_package() {
    use std::os::unix::fs::PermissionsExt;
    let server = TestServer::start_old_built_in().await;
    server
        .create_user("admin", ADMIN_PASSWORD, Role::Admin)
        .await;
    server.create_user("alice", USER_PASSWORD, Role::User).await;
    let admin = server.login("admin", ADMIN_PASSWORD).await;
    let alice = server.login("alice", USER_PASSWORD).await;
    let alice_id = server.user_id("alice").await;
    install_ai_agent_app(&server, &admin, Some(alice_id)).await;
    let (status, _, body) = server
        .request(
            "PUT",
            &format!("/api/users/{alice_id}/access-rules"),
            Some(json!({"folderRules": [], "appRules": [
                {"appId": "agent", "rule": "allow"}, {"appId": "files", "rule": "allow"}
            ]})),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Built in: a provider with a key, a chat with a file.
    let (addr, log) = fake_provider();
    let base_url = format!("http://{addr}/v1");
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/providers",
            Some(
                json!({"kind": "custom", "name": "Local", "baseUrl": base_url,
                        "apiKey": "sk-test-secret-1234", "models": ["fake-1", "tools"]}),
            ),
            admin.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "fake-1"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let chat = body["conversation"]["id"].as_str().unwrap().to_string();
    let (status, body) = server
        .request_raw(
            "POST",
            "/api/ai/files?name=notes.txt",
            b"buy milk".to_vec(),
            "text/plain",
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let file: Value = serde_json::from_slice(&body).unwrap();
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{chat}/messages"),
            Some(json!({"text": "hello", "files": [file["id"]]})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = ai_finished(&server, &alice, &chat).await;
    assert_eq!(before["messages"][1]["status"], "done", "{before}");

    // The package: AI Agent's server program, installed and the default.
    let root = server
        .state
        .config
        .packages_dir()
        .join("runtimes/ai-agent/1.0.0");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_rast-ai-agent"),
        root.join("bin/rast-ai-agent"),
    )
    .unwrap();
    std::fs::set_permissions(
        root.join("bin/rast-ai-agent"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(root.join("ui/app.js"), "export default { mount() {} };\n").unwrap();
    let manifest: rast::packages::manifest::Manifest = serde_json::from_value(json!({
        "schemaVersion": 1, "kind": "rast-package", "id": "ai-agent", "type": "app",
        "name": "AI Agent", "description": "Chat with AI models",
        "publisher": {"id": "rast", "name": "RAST"},
        "version": "1.0.0", "revision": 1,
        "app": {
            "id": "agent",
            "window": {"entry": "ui/app.js", "sdk": 1},
            "server": {"executable": "bin/rast-ai-agent", "hostApi": 1, "apiPrefix": "/api/ai", "eventPrefix": "ai."}
        },
        "artifacts": [{
            "target": "linux-x86_64", "url": "https://example.com/ai-agent.tar.gz", "sha256": "a".repeat(64),
            "size": 1, "format": "tar.gz", "provenance": {"kind": "built", "source": "https://example.com"}
        }]
    }))
    .unwrap();
    rast::packages::store::record_install(
        &server.state.db,
        rast::packages::store::NewVersion {
            manifest: &manifest,
            source: "official",
            source_repo: None,
            target: "linux-x86_64",
            artifact_url: "https://example.com/ai-agent.tar.gz",
            release_tag: None,
            install_path: &root.display().to_string(),
            size_bytes: 1,
            check_output: "",
            installed_by: "admin",
            artifact_sha256: &"a".repeat(64),
            installed_at: None,
        },
    )
    .await
    .unwrap();
    rast::packages::store::set_default(&server.state.db, "ai-agent", Some("1.0.0"))
        .await
        .unwrap();
    server.state.apps.sync(&server.state).await;

    // App Center shows the package on the AI Agent app's tile.
    let (status, _, body) = server
        .request("GET", "/api/packages", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "ai-agent")
        .expect("the package is listed");
    assert_eq!(listed["appId"], "agent", "{listed}");
    assert_eq!(listed["packageType"], "app", "{listed}");

    // The chat came along, answered by the app now.
    let (status, _, body) = server
        .request(
            "GET",
            &format!("/api/ai/conversations/{chat}"),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"], before["messages"]);
    let app = server.state.apps.get("agent").expect("the app runs");
    assert_eq!(app.status().state, "running");
    let data = app.data_dir();
    assert!(data.join("agent.db").is_file());
    assert!(data.join("secret.key").is_file());
    assert!(
        data.join(format!(
            "ai-files/{alice_id}/{}",
            file["id"].as_str().unwrap()
        ))
        .is_file(),
        "chat files are copied"
    );
    let handed: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'apps.agent.handedOverAt'")
            .fetch_optional(&server.state.db)
            .await
            .unwrap();
    assert!(handed.is_some(), "RAST knows its data was taken");

    // Its file can still be read; a new reply reaches the provider with the
    // key the app sealed again.
    let (status, _, _) = server
        .request(
            "GET",
            &format!("/api/ai/files/{}", file["id"].as_str().unwrap()),
            None,
            alice.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let requests_before = log.lock().unwrap().len();
    let reply = ai_say(&server, &alice, &chat, "and again").await;
    assert_eq!(reply["status"], "done", "{reply}");
    assert!(ai_text(&reply).contains("Ahoy"), "{reply}");
    {
        let requests = log.lock().unwrap();
        assert!(requests.len() > requests_before);
        assert_eq!(requests.last().unwrap().1, "Bearer sk-test-secret-1234");
    }

    // Accounts come from RAST: the admin list names the chat's owner.
    let (status, _, body) = server
        .request("GET", "/api/ai/admin/conversations", None, admin.headers())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["owner"], "alice", "{body}");
    let (status, _, body) = server
        .request(
            "GET",
            "/api/ai/admin/conversations?q=ali",
            None,
            admin.headers(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");

    // File tools act through File Station as the account.
    std::fs::write(server.state.storage.home_dir("alice").join("plan.txt"), "x").unwrap();
    let (status, _, body) = server
        .request(
            "POST",
            "/api/ai/conversations",
            Some(json!({"providerId": provider_id, "model": "tools"})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tools_chat = body["conversation"]["id"].as_str().unwrap().to_string();
    let reply = ai_say(&server, &alice, &tools_chat, "list please").await;
    assert_eq!(reply["status"], "waiting", "{reply}");
    let (status, _, body) = server
        .request(
            "POST",
            &format!("/api/ai/conversations/{tools_chat}/tools/call_0_list_files"),
            Some(json!({"decision": "allow", "always": false})),
            alice.headers_with_csrf(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done = ai_finished(&server, &alice, &tools_chat).await;
    let reply = done["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(reply.to_string().contains("plan.txt"), "{reply}");

    // Nobody else reaches the app's server: RAST's sign-in comes first.
    let (status, _, _) = server
        .request("GET", "/api/ai/options", None, Default::default())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    server.state.apps.stop_all().await;
}
