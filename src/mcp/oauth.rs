//! MCP authorization: protected-resource / issuer discovery, client registration.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

// ---------------------------------------------------------------------------
// OAuth (MCP authorization spec)
// ---------------------------------------------------------------------------

/// A stored OAuth grant for one server URL.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Token {
    pub(super) access_token: String,
    #[serde(default)]
    pub(super) refresh_token: String,
    #[serde(default)]
    pub(super) expires_at: u64,
    #[serde(default)]
    pub(super) client_id: String,
    #[serde(default)]
    pub(super) client_secret: String,
    #[serde(default)]
    pub(super) token_endpoint: String,
    #[serde(default)]
    pub(super) resource: String,
}

/// A parameter of a `WWW-Authenticate: Bearer k="v", k2=v2` challenge.
fn challenge_param(header: &str, key: &str) -> Option<String> {
    let mut rest = header;
    while let Some(i) = rest.find(key) {
        let before_ok = i == 0 || matches!(rest.as_bytes()[i - 1], b' ' | b',');
        let after = rest[i + key.len()..].trim_start();
        if before_ok {
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                return Some(match v.strip_prefix('"') {
                    Some(q) => q.split('"').next().unwrap_or("").to_string(),
                    None => v.split([',', ' ']).next().unwrap_or("").to_string(),
                });
            }
        }
        rest = &rest[i + key.len()..];
    }
    None
}

/// (origin, path) of an http(s) URL: origin lowercased, path without query,
/// fragment or trailing slash ("" for the root).
fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..end].to_ascii_lowercase();
    let path = rest[end..].split(['?', '#']).next().unwrap_or("").trim_end_matches('/').to_string();
    Some((format!("{scheme}://{host}"), path))
}

/// Where to look for the protected-resource metadata (RFC 9728) of a server
/// when its 401 didn't say: path-specific first, then the root.
fn resource_metadata_urls(server_url: &str) -> Vec<String> {
    let Some((origin, path)) = split_url(server_url) else { return Vec::new() };
    let base = format!("{origin}/.well-known/oauth-protected-resource");
    if path.is_empty() {
        vec![base]
    } else {
        vec![format!("{base}{path}"), base]
    }
}

/// Where to look for an issuer's metadata: RFC 8414 then OpenID Connect
/// discovery, with path insertion for issuers that have a path.
fn auth_metadata_urls(issuer: &str) -> Vec<String> {
    let Some((origin, path)) = split_url(issuer) else { return Vec::new() };
    if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// What an authorization server needs from us, discovered for one MCP server.
#[derive(Debug, PartialEq)]
pub(super) struct AuthPlan {
    pub(super) authorize: String,
    pub(super) token: String,
    pub(super) register: Option<String>,
    /// The canonical URI of the MCP server, sent as `resource` (RFC 8707).
    pub(super) resource: String,
    pub(super) scope: String,
}

/// Build the plan from the discovered documents: `prm` is the protected-
/// resource metadata (None for servers predating it, whose own origin is then
/// the authorization server), `asm` the authorization-server metadata (None:
/// the spec's default endpoints on the issuer).
fn auth_plan(server_url: &str, challenge: Option<&str>, prm: Option<&Value>, asm: Option<&Value>) -> Result<AuthPlan, String> {
    let (origin, path) = split_url(server_url).ok_or_else(|| format!("{server_url} isn't an http(s) URL"))?;
    let resource = prm
        .and_then(|p| p.get("resource").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{origin}{path}"));
    let issuer = prm
        .and_then(|p| p.get("authorization_servers").and_then(Value::as_array))
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or(origin);
    let endpoint = |k: &str, default: &str| {
        asm.and_then(|m| m.get(k).and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("{issuer}{default}"))
    };
    if let Some(methods) = asm.and_then(|m| m.get("code_challenge_methods_supported")).and_then(Value::as_array) {
        if !methods.iter().any(|m| m.as_str() == Some("S256")) {
            return Err("the authorization server doesn't support PKCE (S256), which MCP requires".into());
        }
    }
    let scope = challenge
        .and_then(|c| challenge_param(c, "scope"))
        .or_else(|| {
            prm.and_then(|p| p.get("scopes_supported").and_then(Value::as_array)).map(|a| {
                a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
            })
        })
        .unwrap_or_default();
    Ok(AuthPlan {
        authorize: endpoint("authorization_endpoint", "/authorize"),
        token: endpoint("token_endpoint", "/token"),
        register: match asm {
            Some(m) => m.get("registration_endpoint").and_then(Value::as_str).map(str::to_string),
            None => Some(format!("{issuer}/register")),
        },
        resource,
        scope,
    })
}

/// GET a JSON document; None on any failure (absence is a normal answer here).
fn fetch_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    let resp = agent.get(url).timeout(Duration::from_secs(10)).set("Accept", "application/json").call().ok()?;
    serde_json::from_str(&resp.into_string().ok()?).ok()
}

/// Discover how to sign in to the MCP server at `server_url`.
pub(super) fn discover(agent: &ureq::Agent, server_url: &str, challenge: Option<&str>) -> Result<AuthPlan, String> {
    let prm_urls = challenge
        .and_then(|c| challenge_param(c, "resource_metadata"))
        .into_iter()
        .chain(resource_metadata_urls(server_url));
    let prm = prm_urls.filter_map(|u| fetch_json(agent, &u)).find(|v| v.get("authorization_servers").is_some());
    let issuer = prm
        .as_ref()
        .and_then(|p| p.get("authorization_servers")?.as_array()?.first()?.as_str().map(str::to_string))
        .or_else(|| split_url(server_url).map(|(o, _)| o))
        .unwrap_or_default();
    let asm = auth_metadata_urls(&issuer)
        .into_iter()
        .filter_map(|u| fetch_json(agent, &u))
        .find(|v| v.get("authorization_endpoint").is_some());
    auth_plan(server_url, challenge, prm.as_ref(), asm.as_ref())
}

/// Dynamic client registration (RFC 7591) as a public client.
pub(super) fn register_client(agent: &ureq::Agent, endpoint: &str, redirect_uri: &str) -> Result<(String, String), String> {
    let body = json!({
        "client_name": "haikucode",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let text = match agent.post(endpoint).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(code, r)) => {
            return Err(format!("client registration failed: HTTP {code}: {}", crate::api::truncate(&r.into_string().unwrap_or_default(), 300)))
        }
        Err(e) => return Err(format!("client registration failed: {e}")),
    };
    let v: Value = serde_json::from_str(&text).map_err(|_| "client registration returned non-JSON".to_string())?;
    let id = v.get("client_id").and_then(Value::as_str).filter(|s| !s.is_empty());
    let id = id.ok_or("client registration returned no client_id")?;
    let secret = v.get("client_secret").and_then(Value::as_str).unwrap_or_default();
    Ok((id.to_string(), secret.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_discovery_urls_and_challenge() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://api.example.com/.well-known/oauth-protected-resource/mcp", scope="files:read files:write""#;
        assert_eq!(
            challenge_param(h, "resource_metadata").as_deref(),
            Some("https://api.example.com/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(challenge_param(h, "scope").as_deref(), Some("files:read files:write"));
        assert_eq!(challenge_param("Bearer realm=x", "scope"), None);
        assert_eq!(
            resource_metadata_urls("https://API.example.com/v1/mcp/?q=1"),
            [
                "https://api.example.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://api.example.com/.well-known/oauth-protected-resource"
            ]
        );
        assert_eq!(
            auth_metadata_urls("https://auth.example.com/tenant1"),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration"
            ]
        );
        assert_eq!(auth_metadata_urls("https://auth.example.com").len(), 2);
    }

    #[test]
    fn oauth_plan_from_metadata_documents() {
        let prm = json!({"resource": "https://api.example.com/mcp", "authorization_servers": ["https://auth.example.com/"], "scopes_supported": ["a", "b"]});
        let asm = json!({
            "issuer": "https://auth.example.com",
            "authorization_endpoint": "https://auth.example.com/oauth/authorize",
            "token_endpoint": "https://auth.example.com/oauth/token",
            "registration_endpoint": "https://auth.example.com/oauth/register",
            "code_challenge_methods_supported": ["S256"]
        });
        let plan = auth_plan("https://api.example.com/mcp", None, Some(&prm), Some(&asm)).unwrap();
        assert_eq!(plan.authorize, "https://auth.example.com/oauth/authorize");
        assert_eq!(plan.register.as_deref(), Some("https://auth.example.com/oauth/register"));
        assert_eq!((plan.resource.as_str(), plan.scope.as_str()), ("https://api.example.com/mcp", "a b"));
        // The 401's scope beats the advertised list.
        let ch = r#"Bearer scope="only""#;
        assert_eq!(auth_plan("https://api.example.com/mcp", Some(ch), Some(&prm), Some(&asm)).unwrap().scope, "only");
        // No metadata at all (a 2025-03-26 server): default endpoints on its origin.
        let bare = auth_plan("https://srv.example.com/mcp", None, None, None).unwrap();
        assert_eq!(bare.token, "https://srv.example.com/token");
        assert_eq!(bare.resource, "https://srv.example.com/mcp");
        // No PKCE, no deal.
        let plain = json!({"authorization_endpoint": "x", "token_endpoint": "y", "code_challenge_methods_supported": ["plain"]});
        assert!(auth_plan("https://srv.example.com/mcp", None, None, Some(&plain)).is_err());
    }
}
