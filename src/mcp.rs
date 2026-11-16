//! MCP (Model Context Protocol) client, over two transports:
//!
//! - **stdio** — the server is spawned as a child process and spoken to in
//!   newline-delimited JSON-RPC 2.0; a reader thread per server funnels stdout
//!   lines into a channel so request/response is synchronous with a timeout.
//! - **Streamable HTTP** — every message is POSTed to the server's URL and the
//!   reply is either JSON or an SSE stream carrying it (possibly after
//!   notifications). `Mcp-Session-Id` and `MCP-Protocol-Version` ride along;
//!   a 404 on a known session means it expired, so the client re-initializes.
//!   A 401 means sign-in: `/mcp login <server>` runs the MCP authorization flow
//!   (protected-resource metadata → authorization-server metadata → dynamic
//!   client registration → PKCE in the browser, see auth.rs) and keeps the
//!   token, refreshed on expiry, in haikucode's config dir (0600).
//!
//! Servers come from config.json's `mcp_servers` plus a project `.mcp.json`
//! (Claude Code's `mcpServers` shape, in the working directory or at the repo
//! root). A project file can make haikucode run any command, so each of its
//! servers waits for a one-time `/mcp approve <name>`, pinned to a hash of
//! that entry: a changed entry asks again.
//!
//! Tools are advertised as `mcp__<server>__<tool>`; resources through two
//! built-in tools (`list_mcp_resources`, `read_mcp_resource`); prompts as
//! slash commands (`/mcp__<server>__<prompt> args`). Lists are paginated.
//!
//! Dependency-free (ureq + serde_json, already here) so it still
//! cross-compiles to a static ARMv6 binary.

use crate::config::McpServerConfig;
use serde_json::Value;
use std::time::Duration;

mod client;
mod config;
mod oauth;
mod render;
mod transport;

pub use client::Mcp;
pub use config::entries;

/// The protocol revision haikucode offers; a server may answer with any of
/// `SUPPORTED` (everything here speaks the same tools/resources/prompts).
const PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const INIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound on list pages, against a server whose cursor never runs out.
const MAX_PAGES: usize = 50;
/// Files in haikucode's config dir: project-server decisions and OAuth tokens.
const APPROVALS_FILE: &str = "mcp-approvals.json";
const TOKENS_FILE: &str = "mcp-oauth.json";

/// Built-in tools for MCP resources, advertised while any server has some.
pub const LIST_RESOURCES: &str = "list_mcp_resources";
pub const READ_RESOURCE: &str = "read_mcp_resource";

/// A tool exposed by an MCP server, in the shape the model schema needs.
#[derive(Clone)]
pub struct McpTool {
    /// `mcp__<server>__<tool>` — the name advertised to the model.
    pub full_name: String,
    pub server: String,
    pub tool: String,
    pub description: String,
    pub schema: Value,
}

/// A prompt a server offers, run as the slash command `/<command>`.
#[derive(Clone)]
pub struct McpPrompt {
    /// `mcp__<server>__<prompt>`, sanitized like tool names.
    pub command: String,
    pub server: String,
    pub name: String,
    pub description: String,
    /// (argument name, required) in the server's order.
    pub arguments: Vec<(String, bool)>,
}

/// One server in the `/mcp` picker: its list line, the detail lines shown
/// when it's picked, and the actions it offers as (label, `/mcp` subcommand).
#[derive(Clone, Debug, PartialEq)]
pub struct McpRow {
    pub name: String,
    pub line: String,
    pub details: Vec<String>,
    pub actions: Vec<(String, String)>,
}

/// Status of one server, for startup notices and `/mcp`.
pub struct ServerStatus {
    pub name: String,
    pub ok: bool,
    /// Not running because it waits on the user (approval or sign-in), not
    /// because it broke.
    pub waiting: bool,
    pub detail: String,
}

/// Why a request failed, kept typed so each transport can recover the right way.
#[derive(Debug, PartialEq)]
enum Fail {
    /// The connection itself broke: process gone, network down, bad HTTP status.
    Transport(String),
    /// The server answered with a JSON-RPC error.
    Rpc(String),
    /// HTTP 401: sign-in needed (with the `WWW-Authenticate` challenge).
    Auth(Option<String>),
    /// HTTP 404 on a request that carried a session id: the session is gone.
    Expired,
    Interrupted,
    Timeout(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Transport(m) | Fail::Rpc(m) | Fail::Timeout(m) => f.write_str(m),
            Fail::Auth(_) => f.write_str("the server needs you to sign in"),
            Fail::Expired => f.write_str("the MCP session expired"),
            Fail::Interrupted => f.write_str("interrupted"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Transport {
    Stdio,
    Http,
}

/// Which transport a config asks for: `type` when set, else `url` → HTTP and
/// `command` → stdio.
fn transport_of(cfg: &McpServerConfig) -> Result<Transport, String> {
    match cfg.kind.to_ascii_lowercase().as_str() {
        "" if !cfg.url.is_empty() => Ok(Transport::Http),
        "" if !cfg.command.is_empty() => Ok(Transport::Stdio),
        "" => Err("needs a \"command\" (stdio) or a \"url\" (HTTP)".into()),
        "stdio" => Ok(Transport::Stdio),
        "http" | "streamable-http" | "streamable_http" | "streamablehttp" => Ok(Transport::Http),
        "sse" => Err("the legacy HTTP+SSE transport isn't supported; use the server's Streamable HTTP URL".into()),
        other => Err(format!("unknown transport type {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(super) fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("haikucode-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub(super) fn stdio(command: &str, args: Vec<String>) -> McpServerConfig {
        McpServerConfig { command: command.into(), args, ..Default::default() }
    }

    #[test]
    fn transport_follows_type_then_url_or_command() {
        let mut c = McpServerConfig { url: "https://x/mcp".into(), ..Default::default() };
        assert_eq!(transport_of(&c), Ok(Transport::Http));
        c.kind = "sse".into();
        assert!(transport_of(&c).unwrap_err().contains("Streamable HTTP"));
        assert_eq!(transport_of(&stdio("npx", vec![])), Ok(Transport::Stdio));
        assert!(transport_of(&McpServerConfig::default()).is_err());
    }
}
