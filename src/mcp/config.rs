//! MCP configuration: global + project servers, env expansion, approvals.

use super::APPROVALS_FILE;
use crate::config::McpServerConfig;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Configuration: global + project servers, env expansion, approvals
// ---------------------------------------------------------------------------

/// Where a server's entry came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Global,
    /// A project `.mcp.json` in `dir`; `hash` pins the approved entry.
    Project { dir: PathBuf, hash: String },
}

/// Every configured server: config.json's plus the project's `.mcp.json`
/// (which wins on a shared name, as in Claude Code).
pub fn entries(global: &BTreeMap<String, McpServerConfig>, cwd: &Path) -> Vec<(String, McpServerConfig, Origin)> {
    let mut all: BTreeMap<String, (McpServerConfig, Origin)> =
        global.iter().map(|(k, v)| (k.clone(), (v.clone(), Origin::Global))).collect();
    if let Some((dir, servers)) = project_servers(cwd) {
        for (name, cfg, hash) in servers {
            all.insert(name, (cfg, Origin::Project { dir: dir.clone(), hash }));
        }
    }
    all.into_iter().map(|(name, (cfg, origin))| (name, cfg, origin)).collect()
}

/// (server name, entry, hash of the raw entry) from a `.mcp.json`.
type ProjectServer = (String, McpServerConfig, String);

/// The project's `.mcp.json` servers — from the working directory, else the
/// repo root — and the directory it was found in.
fn project_servers(cwd: &Path) -> Option<(PathBuf, Vec<ProjectServer>)> {
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf);
    let dir = std::iter::once(cwd.to_path_buf()).chain(root).find(|d| d.join(".mcp.json").is_file())?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".mcp.json")).ok()?).ok()?;
    let servers = v.get("mcpServers").or_else(|| v.get("mcp_servers"))?.as_object()?;
    let list = servers
        .iter()
        .filter_map(|(name, raw)| {
            let cfg = McpServerConfig::parse(raw)?;
            Some((name.clone(), cfg, hex(&crate::auth::sha256(raw.to_string().as_bytes()))))
        })
        .collect();
    Some((dir, list))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Expand `${VAR}` and `${VAR:-default}` from the environment. A variable
/// that is unset with no default is an error naming it.
fn expand_env(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]); // unterminated: keep it literally
            return Ok(out);
        };
        let inner = &after[..j];
        let (var, default) = match inner.split_once(":-") {
            Some((v, d)) => (v, Some(d)),
            None => (inner, None),
        };
        // As in a shell: `:-` also covers a variable that is set but empty.
        let value = std::env::var(var).ok().filter(|v| !v.is_empty() || default.is_none());
        match (value, default) {
            (Some(v), _) => out.push_str(&v),
            (None, Some(d)) => out.push_str(d),
            (None, None) => return Err(format!("environment variable {var} is not set")),
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `cfg` with every string field env-expanded.
pub(super) fn expanded(cfg: &McpServerConfig) -> Result<McpServerConfig, String> {
    let map = |m: &BTreeMap<String, String>| -> Result<BTreeMap<String, String>, String> {
        m.iter().map(|(k, v)| Ok((k.clone(), expand_env(v)?))).collect()
    };
    Ok(McpServerConfig {
        command: expand_env(&cfg.command)?,
        args: cfg.args.iter().map(|a| expand_env(a)).collect::<Result<_, _>>()?,
        env: map(&cfg.env)?,
        kind: cfg.kind.clone(),
        url: expand_env(&cfg.url)?,
        headers: map(&cfg.headers)?,
        oauth: cfg.oauth.clone(),
    })
}

pub(super) fn read_store<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Write a store file atomically, readable only by the user (it can hold
/// OAuth tokens).
pub(super) fn write_store<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    crate::config::atomic_write(path, text.as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct Decision {
    allow: bool,
    #[serde(default)]
    hash: String,
}

/// project dir → server name → decision
type Approvals = BTreeMap<String, BTreeMap<String, Decision>>;

/// Some(true): approved for exactly this entry; Some(false): denied (a denial
/// sticks even if the entry changes); None: never decided, or the approved
/// entry has changed since.
pub(super) fn approval(store: &Path, dir: &Path, name: &str, hash: &str) -> Option<bool> {
    let all: Approvals = read_store(&store.join(APPROVALS_FILE));
    let d = all.get(&dir.display().to_string())?.get(name)?;
    if !d.allow {
        return Some(false);
    }
    (d.hash == hash).then_some(true)
}

pub(super) fn record_approval(store: &Path, dir: &Path, name: &str, hash: &str, allow: bool) -> Result<(), String> {
    let path = store.join(APPROVALS_FILE);
    let mut all: Approvals = read_store(&path);
    all.entry(dir.display().to_string())
        .or_default()
        .insert(name.to_string(), Decision { allow, hash: hash.to_string() });
    write_store(&path, &all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion_with_defaults_and_missing_vars() {
        std::env::set_var("HAIKUCODE_TEST_MCP_TOKEN", "s3cret");
        assert_eq!(expand_env("Bearer ${HAIKUCODE_TEST_MCP_TOKEN}").unwrap(), "Bearer s3cret");
        assert_eq!(expand_env("${HAIKUCODE_TEST_MCP_NOPE:-http://localhost}/mcp").unwrap(), "http://localhost/mcp");
        assert!(expand_env("${HAIKUCODE_TEST_MCP_NOPE}").unwrap_err().contains("HAIKUCODE_TEST_MCP_NOPE"));
        assert_eq!(expand_env("no vars, ${unterminated").unwrap(), "no vars, ${unterminated");
    }
}
