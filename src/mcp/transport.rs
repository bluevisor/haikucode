//! MCP transports: stdio (newline JSON-RPC) and Streamable HTTP, and one connected server.

use super::{transport_of, Fail, Transport, INIT_TIMEOUT, MAX_PAGES, PROTOCOL_VERSION, SUPPORTED};
use crate::config::McpServerConfig;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// stdio transport
// ---------------------------------------------------------------------------

struct StdioConn {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
}

impl StdioConn {
    fn spawn(cfg: &McpServerConfig) -> Result<StdioConn, Fail> {
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Fail::Transport(format!("spawn failed: {e}")))?;
        let stdin = child.stdin.take().ok_or(Fail::Transport("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or(Fail::Transport("no stdout".into()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                if tx.send(line.trim_end().to_string()).is_err() {
                    break;
                }
                line.clear();
            }
        });
        Ok(StdioConn { child, stdin, rx })
    }

    fn send(&mut self, msg: &Value) -> Result<(), Fail> {
        let line = msg.to_string();
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| Fail::Transport(format!("write failed: {e}")))
    }

    /// Wait for the response to `id`, skipping notifications and anything
    /// else. Polls `cancel` so Esc can abort a slow call.
    fn wait(&mut self, id: i64, method: &str, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, Fail> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Fail::Interrupted);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Fail::Timeout(format!("timed out waiting for {method}")));
            }
            let line = match self.rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(l) => l,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(Fail::Transport("server closed the connection".into())),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(r) = pick(&msg, id) {
                return r;
            }
        }
    }
}

/// The outcome carried by `msg` if it is the response to our request `id`.
/// A server→client request can reuse the same number (separate id spaces),
/// so anything with a `method` is never a response.
fn pick(msg: &Value, id: i64) -> Option<Result<Value, Fail>> {
    if msg.get("id").and_then(Value::as_i64) != Some(id) || msg.get("method").is_some() {
        return None;
    }
    if let Some(err) = msg.get("error") {
        let m = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
        return Some(Err(Fail::Rpc(m.to_string())));
    }
    Some(Ok(msg.get("result").cloned().unwrap_or(Value::Null)))
}

// ---------------------------------------------------------------------------
// Streamable HTTP transport
// ---------------------------------------------------------------------------

/// The `data` payload of each event in a `text/event-stream`.
struct SseEvents<R> {
    lines: std::io::Lines<R>,
}

impl<R: BufRead> Iterator for SseEvents<R> {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        let mut data: Option<String> = None;
        loop {
            let Some(Ok(raw)) = self.lines.next() else {
                return data; // end of stream: flush what's pending
            };
            let line = raw.strip_suffix('\r').unwrap_or(&raw);
            if line.is_empty() {
                if data.is_some() {
                    return data;
                }
                continue;
            }
            // `event:`, `id:`, `retry:` and `:` comments carry nothing we need.
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                match &mut data {
                    Some(d) => {
                        d.push('\n');
                        d.push_str(rest);
                    }
                    None => data = Some(rest.to_string()),
                }
            }
        }
    }
}

/// The messages in one JSON-RPC payload (a single message or a batch).
fn batch(msg: &Value) -> Vec<&Value> {
    match msg.as_array() {
        Some(a) => a.iter().collect(),
        None => vec![msg],
    }
}

/// The response to request `id` from an HTTP reply body: JSON (one message or
/// a batch), or an SSE stream that may carry notifications and server
/// requests before it.
fn read_response(content_type: &str, mut body: impl BufRead, id: i64) -> Result<Value, Fail> {
    if content_type.starts_with("text/event-stream") {
        for data in (SseEvents { lines: body.lines() }) {
            let Ok(msg) = serde_json::from_str::<Value>(&data) else { continue };
            if let Some(r) = batch(&msg).into_iter().find_map(|m| pick(m, id)) {
                return r;
            }
        }
        return Err(Fail::Transport("the event stream ended before the response".into()));
    }
    let mut text = String::new();
    body.read_to_string(&mut text).map_err(|e| Fail::Transport(format!("read failed: {e}")))?;
    let msg: Value = serde_json::from_str(&text)
        .map_err(|_| Fail::Transport(format!("the reply isn't JSON: {}", crate::api::truncate(text.trim(), 200))))?;
    batch(&msg)
        .into_iter()
        .find_map(|m| pick(m, id))
        .unwrap_or_else(|| Err(Fail::Transport("the reply held no response to the request".into())))
}

/// One POST of a JSON-RPC message. Returns the session id the server set (if
/// any) and, for a request (`id`), its response.
pub(super) fn http_post(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    body: &str,
    id: Option<i64>,
) -> Result<(Option<String>, Value), Fail> {
    let mut req = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.send_string(body) {
        Ok(resp) => {
            let sid = resp.header("mcp-session-id").map(str::to_string);
            let Some(id) = id else { return Ok((sid, Value::Null)) };
            if resp.status() == 202 {
                return Err(Fail::Transport("the server accepted the request but sent no response".into()));
            }
            let ct = resp.content_type().to_ascii_lowercase();
            let v = read_response(&ct, BufReader::new(resp.into_reader()), id)?;
            Ok((sid, v))
        }
        Err(ureq::Error::Status(401, r)) => Err(Fail::Auth(r.header("www-authenticate").map(str::to_string))),
        Err(ureq::Error::Status(404, _)) => Err(Fail::Expired),
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            Err(Fail::Transport(format!("HTTP {code}: {}", crate::api::truncate(detail.trim(), 300))))
        }
        Err(e) => Err(Fail::Transport(e.to_string())),
    }
}

/// Wait for a worker thread's result, giving up on timeout or cancel.
fn wait_on<T>(rx: &Receiver<T>, timeout: Duration, cancel: Option<&AtomicBool>, what: &str) -> Result<T, Fail> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Fail::Interrupted);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Fail::Timeout(format!("timed out waiting for {what}")));
        }
        match rx.recv_timeout(left.min(Duration::from_millis(250))) {
            Ok(v) => return Ok(v),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return Err(Fail::Transport("the connection closed".into())),
        }
    }
}

struct HttpConn {
    agent: ureq::Agent,
    url: String,
    /// Static headers from the config (already env-expanded).
    headers: Vec<(String, String)>,
    /// OAuth access token, sent as a bearer unless a static header sets
    /// `Authorization` itself.
    token: Option<String>,
    session: Option<String>,
    /// The negotiated protocol version, sent on every request after initialize.
    version: Option<String>,
}

impl HttpConn {
    fn new(cfg: &McpServerConfig, token: Option<String>) -> HttpConn {
        HttpConn {
            agent: crate::api::agent_http(),
            url: cfg.url.clone(),
            headers: cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            token,
            session: None,
            version: None,
        }
    }

    fn request_headers(&self) -> Vec<(String, String)> {
        let mut h = self.headers.clone();
        if let Some(t) = &self.token {
            if !h.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")) {
                h.push(("Authorization".into(), format!("Bearer {t}")));
            }
        }
        if let Some(s) = &self.session {
            h.push(("Mcp-Session-Id".into(), s.clone()));
        }
        if let Some(v) = &self.version {
            h.push(("MCP-Protocol-Version".into(), v.clone()));
        }
        h
    }

    /// POST `msg` on a worker thread (so Esc and the timeout work) and return
    /// the response to `id`. Keeps the session id the server hands out.
    fn post(&mut self, msg: &Value, id: Option<i64>, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, Fail> {
        let (agent, url, headers, body) = (self.agent.clone(), self.url.clone(), self.request_headers(), msg.to_string());
        let had_session = self.session.is_some();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(http_post(&agent, &url, &headers, &body, id));
        });
        match wait_on(&rx, timeout, cancel, "the MCP server")? {
            Ok((sid, v)) => {
                if sid.is_some() {
                    self.session = sid;
                }
                Ok(v)
            }
            Err(Fail::Expired) if !had_session => Err(Fail::Transport("HTTP 404: no MCP endpoint at this URL".into())),
            Err(Fail::Expired) => {
                self.session = None;
                Err(Fail::Expired)
            }
            Err(e) => Err(e),
        }
    }

    /// End the session (best-effort, briefly: this runs at exit).
    fn close(&mut self) {
        if let Some(sid) = self.session.take() {
            let mut req = self.agent.delete(&self.url).timeout(Duration::from_secs(2)).set("Mcp-Session-Id", &sid);
            for (k, v) in self.request_headers() {
                req = req.set(&k, &v);
            }
            let _ = req.call();
        }
    }
}

// ---------------------------------------------------------------------------
// One connected server
// ---------------------------------------------------------------------------

enum Conn {
    Stdio(StdioConn),
    Http(HttpConn),
}

pub(super) struct Server {
    conn: Conn,
    next_id: i64,
    pub(super) has_tools: bool,
    pub(super) has_resources: bool,
    pub(super) has_prompts: bool,
}

impl Server {
    pub(super) fn connect(cfg: &McpServerConfig, token: Option<String>) -> Result<Server, Fail> {
        let conn = match transport_of(cfg).map_err(Fail::Transport)? {
            Transport::Stdio => Conn::Stdio(StdioConn::spawn(cfg)?),
            Transport::Http => Conn::Http(HttpConn::new(cfg, token)),
        };
        let mut s = Server { conn, next_id: 1, has_tools: false, has_resources: false, has_prompts: false };
        if let Err(e) = s.initialize() {
            s.close();
            return Err(e);
        }
        Ok(s)
    }

    pub(super) fn is_http(&self) -> bool {
        matches!(self.conn, Conn::Http(_))
    }

    fn send(&mut self, method: &str, params: Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, Fail> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match &mut self.conn {
            Conn::Stdio(c) => {
                c.send(&msg)?;
                c.wait(id, method, timeout, cancel)
            }
            Conn::Http(c) => c.post(&msg, Some(id), timeout, cancel),
        }
    }

    /// A request, re-initializing once if the HTTP session expired under it.
    pub(super) fn request(&mut self, method: &str, params: Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, Fail> {
        match self.send(method, params.clone(), timeout, cancel) {
            Err(Fail::Expired) => {
                self.initialize()?;
                self.send(method, params, timeout, cancel)
            }
            r => r,
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &mut self.conn {
            Conn::Stdio(c) => {
                let _ = c.send(&msg);
            }
            Conn::Http(c) => {
                let _ = c.post(&msg, None, INIT_TIMEOUT, None);
            }
        }
    }

    /// The handshake: offer our protocol version, accept any we support, note
    /// what the server can do.
    fn initialize(&mut self) -> Result<(), Fail> {
        if let Conn::Http(c) = &mut self.conn {
            c.session = None;
            c.version = None;
        }
        let r = self.send(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "haikucode", "version": env!("CARGO_PKG_VERSION")}
            }),
            INIT_TIMEOUT,
            None,
        )?;
        let version = r.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
        if !SUPPORTED.contains(&version) {
            return Err(Fail::Transport(format!("the server speaks MCP {version:?}, which haikucode doesn't support")));
        }
        if let Conn::Http(c) = &mut self.conn {
            c.version = Some(version.to_string());
        }
        let caps = r.get("capabilities");
        let has = |k: &str| caps.and_then(|c| c.get(k)).is_some();
        (self.has_tools, self.has_resources, self.has_prompts) = (has("tools"), has("resources"), has("prompts"));
        self.notify("notifications/initialized", json!({}));
        Ok(())
    }

    /// Every item of a paginated list (`tools/list` → `tools`, …).
    pub(super) fn list_all(&mut self, method: &str, key: &str) -> Result<Vec<Value>, Fail> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request(method, params, INIT_TIMEOUT, None)?;
            if let Some(items) = page.get(key).and_then(Value::as_array) {
                out.extend(items.iter().cloned());
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    pub(super) fn set_token(&mut self, token: Option<String>) {
        if let Conn::Http(c) = &mut self.conn {
            c.token = token;
        }
    }

    pub(super) fn close(&mut self) {
        match &mut self.conn {
            Conn::Stdio(c) => {
                let _ = c.child.kill();
                let _ = c.child.wait();
            }
            Conn::Http(c) => c.close(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_skip_comments_and_join_multiline_data() {
        let stream = ": keepalive\nevent: message\ndata: {\"a\":\ndata: 1}\n\nid: 7\ndata: second\r\n\r\ndata: tail";
        let events: Vec<String> = SseEvents { lines: std::io::Cursor::new(stream).lines() }.collect();
        assert_eq!(events, ["{\"a\":\n1}", "second", "tail"]);
    }

    #[test]
    fn responses_come_from_json_batches_and_event_streams() {
        let body = |s: &str| std::io::Cursor::new(s.to_string());
        let ok = r#"{"jsonrpc":"2.0","id":3,"result":{"x":1}}"#;
        assert_eq!(read_response("application/json", body(ok), 3), Ok(json!({"x": 1})));
        let batch = r#"[{"jsonrpc":"2.0","method":"notifications/message"},{"jsonrpc":"2.0","id":3,"result":7}]"#;
        assert_eq!(read_response("application/json", body(batch), 3), Ok(json!(7)));
        let err = r#"{"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"boom"}}"#;
        assert_eq!(read_response("application/json", body(err), 3), Err(Fail::Rpc("boom".into())));
        // A progress notification and a server request reusing our id come first.
        let sse = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n\
                   data: {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n\n\
                   data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":\"done\"}\n\n";
        assert_eq!(read_response("text/event-stream", body(sse), 3), Ok(json!("done")));
        let cut = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n";
        assert!(matches!(read_response("text/event-stream", body(cut), 3), Err(Fail::Transport(_))));
    }
}
