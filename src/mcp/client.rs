//! The MCP client: every configured server, running or not.

use super::config::{approval, expanded, read_store, record_approval, write_store, Origin};
use super::oauth::{discover, register_client, Token};
use super::render::{
    bind_prompt_args, parse_prompts, parse_tools, render_prompt, render_resource,
    render_tool_result, resource_tools,
};
use super::transport::{http_post, Server};
use super::{
    transport_of, Fail, McpPrompt, McpTool, ServerStatus, Transport, CALL_TIMEOUT, LIST_RESOURCES,
    MAX_PAGES, PROTOCOL_VERSION, READ_RESOURCE, TOKENS_FILE,
};
use crate::config::McpServerConfig;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

// ---------------------------------------------------------------------------
// The client: every configured server, running or not
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum State {
    Running,
    Failed(String),
    NeedsApproval,
    Denied,
    NeedsLogin,
}

struct Slot {
    name: String,
    /// As configured (unexpanded: `${VAR}`s are resolved at each connect).
    cfg: McpServerConfig,
    origin: Origin,
    state: State,
    server: Option<Server>,
    tools: Vec<McpTool>,
    prompts: Vec<McpPrompt>,
    resources: bool,
}

/// Owns the servers and the flattened tool list the model sees.
pub struct Mcp {
    slots: Vec<Slot>,
    tools: Vec<McpTool>,
    status: Vec<ServerStatus>,
    /// Where approvals and OAuth tokens are kept (haikucode's config dir).
    store: PathBuf,
}

impl Mcp {
    pub fn disabled() -> Mcp {
        Mcp { slots: Vec::new(), tools: Vec::new(), status: Vec::new(), store: crate::config::config_dir() }
    }

    /// Start `entries` (see `entries`), keeping approvals and tokens under
    /// `store`. Project servers the user hasn't approved (for this exact
    /// entry) stay stopped. Failures are recorded in `status` (and reported via
    /// `/mcp`), never fatal.
    pub fn start(entries: Vec<(String, McpServerConfig, Origin)>, store: PathBuf) -> Mcp {
        let mut mcp = Mcp { slots: Vec::new(), tools: Vec::new(), status: Vec::new(), store };
        for (name, cfg, origin) in entries {
            let state = match &origin {
                Origin::Project { dir, hash } => match approval(&mcp.store, dir, &name, hash) {
                    Some(true) => State::Running,
                    Some(false) => State::Denied,
                    None => State::NeedsApproval,
                },
                Origin::Global => State::Running,
            };
            mcp.slots.push(Slot { name, cfg, origin, state, server: None, tools: Vec::new(), prompts: Vec::new(), resources: false });
        }
        for i in 0..mcp.slots.len() {
            if mcp.slots[i].state == State::Running {
                mcp.open(i);
            }
        }
        mcp.rebuild();
        mcp
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// (Re)connect slot `i` and load its tools and prompts.
    fn open(&mut self, i: usize) {
        if let Some(mut old) = self.slots[i].server.take() {
            old.close();
        }
        let cfg = match expanded(&self.slots[i].cfg) {
            Ok(c) => c,
            Err(e) => {
                self.slots[i].state = State::Failed(e);
                return;
            }
        };
        let token = if transport_of(&cfg) == Ok(Transport::Http) { self.bearer(&cfg.url, false) } else { None };
        let slot = &mut self.slots[i];
        (slot.tools, slot.prompts, slot.resources) = (Vec::new(), Vec::new(), false);
        let mut server = match Server::connect(&cfg, token) {
            Ok(s) => s,
            Err(Fail::Auth(_)) => {
                slot.state = State::NeedsLogin;
                return;
            }
            Err(e) => {
                slot.state = State::Failed(e.to_string());
                return;
            }
        };
        // Older servers list tools without declaring the capability: always
        // ask, but only a server that declared tools fails over the answer.
        match server.list_all("tools/list", "tools") {
            Ok(t) => slot.tools = parse_tools(&slot.name, &t),
            Err(e) if server.has_tools => {
                server.close();
                slot.state = State::Failed(format!("tools/list failed: {e}"));
                return;
            }
            Err(_) => {}
        }
        if server.has_prompts {
            if let Ok(p) = server.list_all("prompts/list", "prompts") {
                slot.prompts = parse_prompts(&slot.name, &p);
            }
        }
        slot.resources = server.has_resources;
        slot.server = Some(server);
        slot.state = State::Running;
    }

    /// Refresh the flattened tool list and statuses after any change.
    fn rebuild(&mut self) {
        let running = || self.slots.iter().filter(|s| s.state == State::Running);
        let mut tools: Vec<McpTool> = running().flat_map(|s| s.tools.iter().cloned()).collect();
        if running().any(|s| s.resources) {
            tools.extend(resource_tools());
        }
        self.tools = tools;
        self.status = self.slots.iter().map(Slot::status).collect();
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    pub fn status(&self) -> &[ServerStatus] {
        &self.status
    }

    /// True if `name` is one of our advertised tool names.
    pub fn handles(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.full_name == name)
    }

    /// Tools that only read (the resource tools): no approval, fine in plan mode.
    pub fn is_read_only(&self, name: &str) -> bool {
        name == LIST_RESOURCES || name == READ_RESOURCE
    }

    /// The prompts of the running servers as slash-palette entries
    /// (`/mcp__server__prompt`, description).
    pub fn prompt_palette(&self) -> Vec<(String, String)> {
        self.slots
            .iter()
            .filter(|s| s.state == State::Running)
            .flat_map(|s| s.prompts.iter())
            .map(|p| {
                let usage: Vec<String> =
                    p.arguments.iter().map(|(n, req)| if *req { format!("<{n}>") } else { format!("[{n}]") }).collect();
                let desc = if p.description.is_empty() { "MCP prompt".to_string() } else { p.description.clone() };
                let desc = if usage.is_empty() { desc } else { format!("{} — {desc}", usage.join(" ")) };
                (format!("/{}", p.command), desc)
            })
            .collect()
    }

    /// A request to server `name`, with the recovery each failure allows: an
    /// expired OAuth token is refreshed, a crashed stdio server is restarted
    /// once (only when the transport itself failed — a tool's own error must
    /// not re-run a side-effecting call), and a rejected token marks the
    /// server as needing sign-in.
    fn request_on(&mut self, name: &str, method: &str, params: Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, Fail> {
        let i = self
            .slots
            .iter()
            .position(|s| s.name == name && s.server.is_some())
            .ok_or_else(|| Fail::Transport(format!("MCP server {name} is not running")))?;
        let http = self.slots[i].server.as_ref().is_some_and(Server::is_http);
        if http {
            // Swap in a fresh token before it lapses mid-call.
            let url = expanded(&self.slots[i].cfg).map(|c| c.url).unwrap_or_default();
            let token = self.bearer(&url, false);
            if let Some(s) = self.slots[i].server.as_mut() {
                s.set_token(token);
            }
        }
        let r = self.slots[i].server.as_mut().map(|s| s.request(method, params.clone(), timeout, cancel));
        match r.unwrap_or_else(|| Err(Fail::Transport("not running".into()))) {
            Err(Fail::Transport(e)) if !http => {
                self.open(i);
                self.rebuild();
                if self.slots[i].state != State::Running {
                    return Err(Fail::Transport(format!("server crashed ({e}) and couldn't be restarted")));
                }
                let s = self.slots[i].server.as_mut().ok_or(Fail::Transport("not running".into()))?;
                s.request(method, params, timeout, cancel)
            }
            Err(Fail::Auth(_)) if http => {
                let url = expanded(&self.slots[i].cfg).map(|c| c.url).unwrap_or_default();
                if let Some(token) = self.bearer(&url, true) {
                    if let Some(s) = self.slots[i].server.as_mut() {
                        s.set_token(Some(token));
                        if let Ok(v) = s.request(method, params, timeout, cancel) {
                            return Ok(v);
                        }
                    }
                }
                if let Some(mut s) = self.slots[i].server.take() {
                    s.close();
                }
                self.slots[i].state = State::NeedsLogin;
                self.rebuild();
                Err(Fail::Transport(format!("{name} needs you to sign in again: /mcp login {name}")))
            }
            r => r,
        }
    }

    /// Call an advertised tool with the given arguments; returns text.
    /// `cancel` aborts the wait (the server may still finish the work).
    pub fn call(&mut self, full_name: &str, args: &Value, cancel: &AtomicBool) -> String {
        match full_name {
            LIST_RESOURCES => return self.list_resources(args, cancel),
            READ_RESOURCE => return self.read_resource(args, cancel),
            _ => {}
        }
        let Some(tool) = self.tools.iter().find(|t| t.full_name == full_name).cloned() else {
            return format!("ERROR: unknown MCP tool {full_name}");
        };
        let params = json!({"name": tool.tool, "arguments": args});
        match self.request_on(&tool.server, "tools/call", params, CALL_TIMEOUT, Some(cancel)) {
            Ok(r) => render_tool_result(&r),
            Err(e) => format!("ERROR: MCP call failed: {e}"),
        }
    }

    fn resource_servers(&self, only: Option<&str>) -> Vec<String> {
        self.slots
            .iter()
            .filter(|s| s.state == State::Running && s.resources && only.is_none_or(|o| o == s.name))
            .map(|s| s.name.clone())
            .collect()
    }

    fn list_resources(&mut self, args: &Value, cancel: &AtomicBool) -> String {
        let only = args.get("server").and_then(Value::as_str).filter(|s| !s.is_empty());
        let names = self.resource_servers(only);
        if names.is_empty() {
            return match only {
                Some(o) => format!("ERROR: no running MCP server named {o} offers resources."),
                None => "No running MCP server offers resources.".into(),
            };
        }
        let mut out = String::new();
        for name in names {
            let listed = self.list_on(&name, "resources/list", "resources", cancel);
            match listed {
                Ok(items) if items.is_empty() => out.push_str(&format!("{name}: (no resources)\n")),
                Ok(items) => {
                    for r in items {
                        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
                        out.push_str(&format!("{name}: {}", s("uri")));
                        if !s("name").is_empty() {
                            out.push_str(&format!(" — {}", s("name")));
                        }
                        if !s("mimeType").is_empty() {
                            out.push_str(&format!(" ({})", s("mimeType")));
                        }
                        if !s("description").is_empty() {
                            out.push_str(&format!(": {}", s("description")));
                        }
                        out.push('\n');
                    }
                }
                Err(e) => out.push_str(&format!("{name}: ERROR {e}\n")),
            }
            if let Ok(templates) = self.list_on(&name, "resources/templates/list", "resourceTemplates", cancel) {
                for t in templates {
                    let s = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");
                    out.push_str(&format!("{name}: template {} — {}\n", s("uriTemplate"), s("name")));
                }
            }
        }
        crate::api::truncate(out.trim_end(), crate::api::MAX_TOOL_OUTPUT)
    }

    /// Every page of a list on server `name`.
    fn list_on(&mut self, name: &str, method: &str, key: &str, cancel: &AtomicBool) -> Result<Vec<Value>, Fail> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request_on(name, method, params, CALL_TIMEOUT, Some(cancel))?;
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

    fn read_resource(&mut self, args: &Value, cancel: &AtomicBool) -> String {
        let Some(uri) = args.get("uri").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return "ERROR: read_mcp_resource needs a 'uri' (see list_mcp_resources).".into();
        };
        let named = args.get("server").and_then(Value::as_str).filter(|s| !s.is_empty());
        let candidates = self.resource_servers(named);
        let server = match (named, candidates.as_slice()) {
            (_, [one]) => one.clone(),
            (Some(n), []) => return format!("ERROR: no running MCP server named {n} offers resources."),
            (None, []) => return "ERROR: no running MCP server offers resources.".into(),
            (None, _) => return format!("ERROR: several servers offer resources ({}); pass 'server'.", candidates.join(", ")),
            (Some(n), _) => n.to_string(),
        };
        match self.request_on(&server, "resources/read", json!({"uri": uri}), CALL_TIMEOUT, Some(cancel)) {
            Ok(r) => render_resource(&r),
            Err(e) => format!("ERROR: reading {uri} failed: {e}"),
        }
    }

    /// Fetch prompt `command` (`mcp__server__prompt`, no slash) with `args`
    /// (positional, or `name=value`) and return its text to send as a turn.
    pub fn get_prompt(&mut self, command: &str, args: &str, cancel: &AtomicBool) -> Result<String, String> {
        let p = self
            .slots
            .iter()
            .filter(|s| s.state == State::Running)
            .flat_map(|s| s.prompts.iter())
            .find(|p| p.command == command)
            .cloned()
            .ok_or_else(|| format!("no MCP prompt /{command} (see /mcp)"))?;
        let values = bind_prompt_args(&p.arguments, args).map_err(|missing| {
            let usage: Vec<String> = p.arguments.iter().map(|(n, _)| format!("{n}=…")).collect();
            format!("/{command} needs {missing}. Usage: /{command} {}", usage.join(" "))
        })?;
        let r = self
            .request_on(&p.server, "prompts/get", json!({"name": p.name, "arguments": values}), CALL_TIMEOUT, Some(cancel))
            .map_err(|e| format!("prompt /{command} failed: {e}"))?;
        render_prompt(&r).ok_or_else(|| format!("prompt /{command} came back empty"))
    }

    /// `/mcp approve|deny <name>` for a project server: record the decision
    /// (pinned to the entry as it is now) and start or stop it.
    pub fn decide(&mut self, name: &str, allow: bool) -> Result<String, String> {
        let i = self.slot(name)?;
        let Origin::Project { dir, hash } = self.slots[i].origin.clone() else {
            return Err(format!("{name} comes from your own config.json; it needs no approval."));
        };
        record_approval(&self.store, &dir, name, &hash, allow)?;
        if allow {
            self.open(i);
        } else {
            if let Some(mut s) = self.slots[i].server.take() {
                s.close();
            }
            self.slots[i].state = State::Denied;
        }
        self.rebuild();
        Ok(match (allow, &self.slots[i].state) {
            (false, _) => format!("{name} denied; it won't start. /mcp approve {name} to allow it."),
            (true, State::Running) => format!("{name} approved and started: {}", self.status[i].detail),
            (true, _) => format!("{name} approved, but: {}", self.status[i].detail),
        })
    }

    /// `/mcp login <name>`: run the MCP authorization flow in the browser and
    /// reconnect with the token. `notify` shows progress (the URL, in case the
    /// browser can't open). Blocks until the redirect arrives (5-minute cap).
    pub fn login(&mut self, name: &str, notify: &dyn Fn(String)) -> Result<String, String> {
        let i = self.slot(name)?;
        if self.slots[i].state == State::NeedsApproval || self.slots[i].state == State::Denied {
            return Err(format!("{name} isn't approved yet: /mcp approve {name}"));
        }
        let cfg = expanded(&self.slots[i].cfg)?;
        if transport_of(&cfg)? != Transport::Http {
            return Err(format!("{name} is a stdio server; only HTTP servers sign in."));
        }
        let agent = crate::api::agent_http();
        // The server's 401 names its resource metadata (and maybe the scope).
        let probe = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": PROTOCOL_VERSION, "capabilities": {},
            "clientInfo": {"name": "haikucode", "version": env!("CARGO_PKG_VERSION")}}});
        let headers: Vec<(String, String)> = cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let challenge = match http_post(&agent, &cfg.url, &headers, &probe.to_string(), Some(0)) {
            Err(Fail::Auth(h)) => h,
            _ => None,
        };
        let plan = discover(&agent, &cfg.url, challenge.as_deref())?;
        let oc = cfg.oauth.clone().unwrap_or_default();
        let (listener, redirect) = crate::auth::bind_redirect(oc.callback_port, "/callback").map_err(|e| e.to_string())?;
        let (client_id, client_secret) = if !oc.client_id.is_empty() {
            (oc.client_id.clone(), oc.client_secret.clone())
        } else if let Some(reg) = &plan.register {
            register_client(&agent, reg, &redirect)?
        } else {
            return Err(format!(
                "{name}'s authorization server has no dynamic client registration; register haikucode with it \
                 (redirect {redirect}, or set oauth.callback_port) and put the client id in the server's \
                 \"oauth\": {{\"client_id\": …}}"
            ));
        };
        let resource = vec![("resource".to_string(), plan.resource.clone())];
        let provider = crate::auth::OAuthProvider {
            key: name.to_string(),
            label: format!("MCP server {name}"),
            auth_url: plan.authorize.clone(),
            token_url: plan.token.clone(),
            client_id: client_id.clone(),
            client_secret: client_secret.clone(),
            scope: if oc.scope.is_empty() { plan.scope.clone() } else { oc.scope.clone() },
            redirect_port: 0,
            redirect_path: "/callback".into(),
            extra: resource.clone(),
            token_extra: resource,
        };
        let login = crate::auth::start_on(provider, listener, redirect).map_err(|e| e.to_string())?;
        notify(format!("sign in to {name} in your browser (opening it now): {}", login.url));
        let t = login.finish().map_err(|e| e.to_string())?;
        let token = Token {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: t.expires_at,
            client_id,
            client_secret,
            token_endpoint: plan.token,
            resource: plan.resource,
        };
        let path = self.store.join(TOKENS_FILE);
        let mut all: BTreeMap<String, Token> = read_store(&path);
        all.insert(cfg.url.clone(), token);
        write_store(&path, &all)?;
        self.open(i);
        self.rebuild();
        match &self.slots[i].state {
            State::Running => Ok(format!("signed in to {name}: {}", self.status[i].detail)),
            _ => Err(format!("signed in, but {name} still won't connect: {}", self.status[i].detail)),
        }
    }

    /// `/mcp logout <name>`: forget the token and disconnect.
    pub fn logout(&mut self, name: &str) -> Result<String, String> {
        let i = self.slot(name)?;
        let url = expanded(&self.slots[i].cfg)?.url;
        let path = self.store.join(TOKENS_FILE);
        let mut all: BTreeMap<String, Token> = read_store(&path);
        if all.remove(&url).is_none() {
            return Err(format!("not signed in to {name}."));
        }
        write_store(&path, &all)?;
        if let Some(mut s) = self.slots[i].server.take() {
            s.close();
        }
        self.slots[i].state = State::NeedsLogin;
        self.rebuild();
        Ok(format!("signed out of {name}."))
    }

    /// The bearer token for `url`: refreshed first when it's about to lapse
    /// (or `force`d, after a 401). None without a stored grant.
    fn bearer(&self, url: &str, force: bool) -> Option<String> {
        let path = self.store.join(TOKENS_FILE);
        let mut all: BTreeMap<String, Token> = read_store(&path);
        let t = all.get(url)?.clone();
        let stale = t.expires_at != 0 && crate::config::now_unix() + 60 >= t.expires_at;
        if !(force || stale) || t.refresh_token.is_empty() {
            return (!force).then_some(t.access_token);
        }
        let p = crate::auth::OAuthProvider {
            key: url.to_string(),
            label: url.to_string(),
            auth_url: String::new(),
            token_url: t.token_endpoint.clone(),
            client_id: t.client_id.clone(),
            client_secret: t.client_secret.clone(),
            scope: String::new(),
            redirect_port: 0,
            redirect_path: String::new(),
            extra: Vec::new(),
            token_extra: if t.resource.is_empty() { Vec::new() } else { vec![("resource".into(), t.resource.clone())] },
        };
        let old = crate::config::OAuthToken {
            access_token: t.access_token.clone(),
            refresh_token: t.refresh_token.clone(),
            expires_at: t.expires_at,
            account_id: String::new(),
        };
        let fresh = crate::auth::refresh(&p, &old).ok()?;
        let updated = Token {
            access_token: fresh.access_token.clone(),
            refresh_token: fresh.refresh_token,
            expires_at: fresh.expires_at,
            ..t
        };
        all.insert(url.to_string(), updated);
        let _ = write_store(&path, &all);
        Some(fresh.access_token)
    }

    fn slot(&self, name: &str) -> Result<usize, String> {
        self.slots
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| format!("no MCP server named {name:?} (see /mcp)"))
    }

    /// Lines for `/mcp`: per server its state, transport, origin, auth and
    /// what it offers.
    pub fn describe(&self) -> Vec<String> {
        let tokens: BTreeMap<String, Token> = read_store(&self.store.join(TOKENS_FILE));
        let mut out = Vec::new();
        for (slot, st) in self.slots.iter().zip(&self.status) {
            let tag = match &slot.state {
                State::Running => "ok",
                State::Failed(_) => "FAILED",
                State::NeedsApproval => "needs approval",
                State::Denied => "denied",
                State::NeedsLogin => "needs sign-in",
            };
            let transport = match transport_of(&slot.cfg) {
                Ok(Transport::Http) => "http",
                Ok(Transport::Stdio) => "stdio",
                Err(_) => "?",
            };
            let origin = match &slot.origin {
                Origin::Global => "config.json".to_string(),
                Origin::Project { dir, .. } => format!("{}/.mcp.json", dir.display()),
            };
            let mut auth = String::new();
            if transport == "http" {
                let url = expanded(&slot.cfg).map(|c| c.url).unwrap_or_default();
                auth = match tokens.get(&url) {
                    Some(t) if t.expires_at == 0 => " · signed in".into(),
                    Some(t) => {
                        let left = t.expires_at.saturating_sub(crate::config::now_unix());
                        if left == 0 { " · token expired".into() } else { format!(" · signed in ({}m left)", left / 60) }
                    }
                    None if slot.cfg.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization")) => " · auth header".into(),
                    None => " · no auth".into(),
                };
            }
            out.push(format!("mcp {} [{tag}] {transport} from {origin}{auth} — {}", slot.name, st.detail));
            if slot.state == State::Running {
                if !slot.tools.is_empty() {
                    let names: Vec<&str> = slot.tools.iter().map(|t| t.full_name.as_str()).collect();
                    out.push(format!("  tools: {}", names.join(", ")));
                }
                if !slot.prompts.is_empty() {
                    let names: Vec<String> = slot.prompts.iter().map(|p| format!("/{}", p.command)).collect();
                    out.push(format!("  prompts: {}", names.join(", ")));
                }
            }
        }
        out
    }

    /// Stop every server (best-effort) on shutdown.
    pub fn shutdown(&mut self) {
        for s in &mut self.slots {
            if let Some(mut server) = s.server.take() {
                server.close();
            }
        }
    }
}

impl Slot {
    fn status(&self) -> ServerStatus {
        let name = &self.name;
        let (ok, waiting, detail) = match &self.state {
            State::Running => {
                let n = self.tools.len();
                let mut d = format!("{n} tool{}", if n == 1 { "" } else { "s" });
                if !self.prompts.is_empty() {
                    let p = self.prompts.len();
                    d.push_str(&format!(", {p} prompt{}", if p == 1 { "" } else { "s" }));
                }
                if self.resources {
                    d.push_str(", resources");
                }
                (true, false, d)
            }
            State::NeedsApproval => {
                let from = match &self.origin {
                    Origin::Project { dir, .. } => format!("{}/.mcp.json", dir.display()),
                    Origin::Global => "config".into(),
                };
                let what = if self.cfg.url.is_empty() {
                    let cmdline: Vec<&str> = std::iter::once(self.cfg.command.as_str()).chain(self.cfg.args.iter().map(String::as_str)).collect();
                    format!("runs `{}`", cmdline.join(" "))
                } else {
                    format!("connects to {}", self.cfg.url)
                };
                (false, true, format!("project server from {from} {what} — not started until you approve it: /mcp approve {name} (or /mcp deny {name})"))
            }
            State::Denied => (false, true, format!("denied — /mcp approve {name} to allow it")),
            State::NeedsLogin => (false, true, format!("needs sign-in: /mcp login {name}")),
            State::Failed(e) => (false, false, e.clone()),
        };
        ServerStatus { name: name.clone(), ok, waiting, detail }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{scratch, stdio};
    use super::*;
    use crate::mcp::entries;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const MINI_SERVER: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
for line in sys.stdin:
    line=line.strip()
    if not line: continue
    m=json.loads(line); i=m.get("id"); meth=m.get("method")
    if meth=="initialize":
        send({"jsonrpc":"2.0","id":i,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mini","version":"0.1"}}})
    elif meth=="tools/list":
        send({"jsonrpc":"2.0","id":i,"result":{"tools":[{"name":"add","description":"Add a and b.","inputSchema":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"}}}}]}})
    elif meth=="tools/call":
        a=m["params"]["arguments"]; send({"jsonrpc":"2.0","id":i,"result":{"content":[{"type":"text","text":"sum=%d"%(a.get("a",0)+a.get("b",0))}]}})
    elif i is not None:
        send({"jsonrpc":"2.0","id":i,"error":{"code":-32601,"message":"nope"}})
"#;

    #[test]
    #[ignore] // needs python3; run with `cargo test -- --ignored`
    fn mcp_handshake_list_and_call() {
        let path = std::env::temp_dir().join(format!("haikucode_mini_mcp-{}.py", std::process::id()));
        std::fs::write(&path, MINI_SERVER).unwrap();
        let cancel = AtomicBool::new(false);
        let cfg = stdio("python3", vec![path.to_string_lossy().into_owned()]);
        let mut mcp = Mcp::start(vec![("mini".into(), cfg, Origin::Global)], scratch("mini"));
        assert_eq!(mcp.tools().len(), 1, "status: {:?}", mcp.status().iter().map(|s| &s.detail).collect::<Vec<_>>());
        assert_eq!(mcp.tools()[0].full_name, "mcp__mini__add");
        assert!(mcp.handles("mcp__mini__add"));
        assert_eq!(mcp.call("mcp__mini__add", &json!({"a": 2, "b": 40}), &cancel), "sum=42");
        assert!(mcp.call("mcp__mini__missing", &json!({}), &cancel).starts_with("ERROR"));
    }

    #[test]
    fn bad_command_is_recorded_not_fatal() {
        let cfg = stdio("definitely-not-a-real-binary-xyz", vec![]);
        let mcp = Mcp::start(vec![("broken".into(), cfg, Origin::Global)], scratch("broken"));
        assert!(mcp.tools().is_empty());
        assert_eq!(mcp.status().len(), 1);
        assert!(!mcp.status()[0].ok && !mcp.status()[0].waiting);
    }

    #[test]
    fn project_servers_need_a_pinned_approval() {
        let store = scratch("approvals");
        let root = scratch("project");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let write = |cmd: &str| {
            std::fs::write(
                root.join(".mcp.json"),
                format!(r#"{{"mcpServers": {{"tool": {{"command": "{cmd}"}}, "remote": {{"type": "http", "url": "http://127.0.0.1:9/mcp"}}}}}}"#),
            )
            .unwrap()
        };
        write("definitely-not-a-real-binary-xyz");
        // Found from a subdirectory via the repo root; overrides a global entry.
        let mut global = BTreeMap::new();
        global.insert("tool".to_string(), stdio("echo", vec![]));
        let found = entries(&global, &sub);
        let tool = found.iter().find(|(n, ..)| n == "tool").unwrap();
        assert!(matches!(&tool.2, Origin::Project { dir, .. } if *dir == root));
        let mut mcp = Mcp::start(found, store.clone());
        assert!(mcp.status().iter().all(|s| !s.ok && s.waiting), "nothing runs before approval");
        assert!(mcp.status()[1].detail.contains("/mcp approve tool"));
        // Approve one: recorded and pinned (it then fails to spawn, which is fine here).
        assert!(mcp.decide("tool", true).is_ok());
        let again = Mcp::start(entries(&global, &sub), store.clone());
        assert!(!again.status()[1].waiting, "approval remembered");
        // Changing the entry asks again; a denial sticks.
        write("something-else");
        let changed = Mcp::start(entries(&global, &sub), store.clone());
        assert!(changed.status()[1].waiting);
        let mut m = Mcp::start(entries(&global, &sub), store.clone());
        m.decide("remote", false).unwrap();
        let denied = Mcp::start(entries(&global, &sub), store);
        assert!(denied.status()[0].detail.starts_with("denied"));
        assert!(m.decide("nope", true).is_err());
    }

    /// A Streamable HTTP server in-process: session ids, a paginated tool list
    /// with one page as SSE, a call answered over SSE after a notification,
    /// and a session that expires once (404) so the client must re-initialize.
    #[test]
    fn streamable_http_session_pagination_sse_and_expiry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        type Seen = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut inits = 0;
            let mut expired_once = false;
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut headers = BTreeMap::new();
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut l = String::new();
                    reader.read_line(&mut l).unwrap();
                    let l = l.trim_end();
                    if l.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                if first.starts_with("DELETE") {
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let msg: Value = serde_json::from_slice(&body).unwrap();
                let method = msg["method"].as_str().unwrap_or("").to_string();
                log.lock().unwrap().push((method.clone(), headers.clone()));
                let id = msg["id"].clone();
                let session = headers.get("mcp-session-id").cloned().unwrap_or_default();
                let reply = |status: &str, extra: &str, ct: &str, body: String| {
                    format!("HTTP/1.1 {status}\r\n{extra}Content-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                let json_ok = |result: Value| reply("200 OK", "", "application/json", json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string());
                let out = match method.as_str() {
                    "initialize" => {
                        inits += 1;
                        let result = json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18",
                            "capabilities": {"tools": {}, "prompts": {}, "resources": {}}, "serverInfo": {"name": "fake", "version": "1"}}});
                        reply("200 OK", &format!("Mcp-Session-Id: s{inits}\r\n"), "application/json", result.to_string())
                    }
                    "notifications/initialized" => reply("202 Accepted", "", "text/plain", String::new()),
                    _ if session.is_empty() => reply("400 Bad Request", "", "text/plain", "no session".into()),
                    "tools/list" if msg["params"]["cursor"].is_null() => {
                        json_ok(json!({"tools": [{"name": "a", "inputSchema": {"type": "object"}}], "nextCursor": "p2"}))
                    }
                    "tools/list" => {
                        let r = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "b", "description": "the b tool"}]}});
                        reply("200 OK", "", "text/event-stream", format!("event: message\ndata: {r}\n\n"))
                    }
                    "prompts/list" => json_ok(json!({"prompts": [{"name": "greet", "arguments": [{"name": "who", "required": true}]}]})),
                    "prompts/get" => json_ok(json!({"messages": [{"role": "user", "content": {"type": "text",
                        "text": format!("Say hi to {}", msg["params"]["arguments"]["who"].as_str().unwrap_or("?"))}}]})),
                    "tools/call" if session == "s1" && !expired_once => {
                        expired_once = true;
                        reply("404 Not Found", "", "text/plain", "session expired".into())
                    }
                    "tools/call" => {
                        let note = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}});
                        let r = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "hi from b"}]}});
                        reply("200 OK", "", "text/event-stream", format!("data: {note}\n\ndata: {r}\n\n"))
                    }
                    _ => json_ok(json!({})),
                };
                let _ = s.write_all(out.as_bytes());
            }
        });

        let cfg = McpServerConfig { url: url.clone(), headers: [("X-Team".to_string(), "haikucode".to_string())].into(), ..Default::default() };
        let mut mcp = Mcp::start(vec![("srv".into(), cfg, Origin::Global)], scratch("http"));
        assert!(mcp.status()[0].ok, "{}", mcp.status()[0].detail);
        let names: Vec<&str> = mcp.tools().iter().map(|t| t.full_name.as_str()).collect();
        assert_eq!(names, ["mcp__srv__a", "mcp__srv__b", LIST_RESOURCES, READ_RESOURCE], "both pages + resource tools");
        assert_eq!(mcp.prompt_palette()[0].0, "/mcp__srv__greet");

        let cancel = AtomicBool::new(false);
        assert_eq!(mcp.call("mcp__srv__b", &json!({}), &cancel), "hi from b", "re-initialized after the 404");
        assert_eq!(mcp.get_prompt("mcp__srv__greet", "Ada", &cancel).unwrap(), "Say hi to Ada");
        assert!(mcp.get_prompt("mcp__srv__greet", "", &cancel).unwrap_err().contains("needs who"));

        let log = seen.lock().unwrap();
        assert_eq!(log.iter().filter(|(m, _)| m == "initialize").count(), 2);
        for (method, h) in log.iter().filter(|(m, _)| m != "initialize") {
            assert_eq!(h.get("mcp-protocol-version").map(String::as_str), Some("2025-06-18"), "{method}");
            assert!(h.contains_key("mcp-session-id"), "{method}");
            assert_eq!(h.get("x-team").map(String::as_str), Some("haikucode"));
        }
        let accept = &log[0].1["accept"];
        assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
    }
}
