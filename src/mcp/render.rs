//! MCP results to model shapes: tool/prompt lists, prompt arguments, result text.

use super::{McpPrompt, McpTool, LIST_RESOURCES, READ_RESOURCE};
use serde_json::{json, Value};

/// The built-in resource tools, advertised while any server has resources.
pub(super) fn resource_tools() -> Vec<McpTool> {
    let tool = |name: &str, description: &str, schema: Value| McpTool {
        full_name: name.to_string(),
        server: String::new(),
        tool: String::new(),
        description: description.to_string(),
        schema,
    };
    vec![
        tool(
            LIST_RESOURCES,
            "List the resources (files, records, docs) your MCP servers offer, with their URIs. Optionally only one server's.",
            json!({"type": "object", "properties": {"server": {"type": "string", "description": "Only this server's resources."}}}),
        ),
        tool(
            READ_RESOURCE,
            "Read one MCP resource by URI (from list_mcp_resources).",
            json!({"type": "object", "properties": {
                "server": {"type": "string", "description": "The server offering it (optional with only one)."},
                "uri": {"type": "string"}
            }, "required": ["uri"]}),
        ),
    ]
}

/// OpenAI-compatible APIs require function names matching
/// `^[a-zA-Z0-9_-]{1,64}$`; one bad MCP name would 400 every request. Map
/// anything else to `_` and clamp the length. Dispatch looks tools up by this
/// sanitized name, while the call still uses the server's real tool name.
fn sanitize_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    out.truncate(64);
    out
}

pub(super) fn parse_tools(server: &str, items: &[Value]) -> Vec<McpTool> {
    items
        .iter()
        .filter_map(|t| {
            let tool = t.get("name").and_then(Value::as_str)?;
            Some(McpTool {
                full_name: sanitize_name(&format!("mcp__{server}__{tool}")),
                server: server.to_string(),
                tool: tool.to_string(),
                description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect()
}

pub(super) fn parse_prompts(server: &str, items: &[Value]) -> Vec<McpPrompt> {
    items
        .iter()
        .filter_map(|p| {
            let name = p.get("name").and_then(Value::as_str)?;
            let arguments = p
                .get("arguments")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            let n = x.get("name").and_then(Value::as_str)?;
                            Some((n.to_string(), x.get("required").and_then(Value::as_bool).unwrap_or(false)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(McpPrompt {
                command: sanitize_name(&format!("mcp__{server}__{name}")),
                server: server.to_string(),
                name: name.to_string(),
                description: p.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                arguments,
            })
        })
        .collect()
}

/// Bind a slash command's argument text to a prompt's arguments: `name=value`
/// pairs (a value runs until the next pair), else positional words with the
/// last argument taking the rest of the line. Err names the missing required
/// arguments.
pub(super) fn bind_prompt_args(params: &[(String, bool)], raw: &str) -> Result<serde_json::Map<String, Value>, String> {
    let mut values = serde_json::Map::new();
    let words: Vec<&str> = raw.split_whitespace().collect();
    fn keyed<'a>(w: &'a str, params: &[(String, bool)]) -> Option<(&'a str, &'a str)> {
        w.split_once('=').filter(|(k, _)| params.iter().any(|(n, _)| n == k))
    }
    if words.first().is_some_and(|w| keyed(w, params).is_some()) {
        let mut current: Option<String> = None;
        for w in words {
            if let Some((k, v)) = keyed(w, params) {
                values.insert(k.to_string(), Value::String(v.to_string()));
                current = Some(k.to_string());
            } else if let Some(k) = &current {
                if let Some(Value::String(s)) = values.get_mut(k) {
                    s.push(' ');
                    s.push_str(w);
                }
            }
        }
    } else {
        for (i, (name, _)) in params.iter().enumerate() {
            let value = if i + 1 == params.len() { words.get(i..).map(|w| w.join(" ")) } else { words.get(i).map(|w| w.to_string()) };
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                values.insert(name.clone(), Value::String(v));
            }
        }
    }
    let missing: Vec<&str> =
        params.iter().filter(|(n, req)| *req && !values.contains_key(n)).map(|(n, _)| n.as_str()).collect();
    if missing.is_empty() {
        Ok(values)
    } else {
        Err(missing.join(", "))
    }
}

/// A `prompts/get` result as one block of text to send as the user's turn;
/// None if it holds nothing.
pub(super) fn render_prompt(result: &Value) -> Option<String> {
    let parts: Vec<String> = result
        .get("messages")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            let text = content_text(m.get("content")?);
            if text.is_empty() {
                return None;
            }
            Some(match m.get("role").and_then(Value::as_str) {
                Some("assistant") => format!("(assistant) {text}"),
                _ => text,
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The text of one content block (or a list of them).
fn content_text(c: &Value) -> String {
    if let Some(list) = c.as_array() {
        return list.iter().map(content_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    }
    match c.get("type").and_then(Value::as_str) {
        Some("text") => c.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
        Some("resource") => {
            let r = c.get("resource");
            match r.and_then(|r| r.get("text")).and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => format!("[resource {}]", r.and_then(|r| r.get("uri")).and_then(Value::as_str).unwrap_or("")),
            }
        }
        Some("resource_link") => format!("[resource {}]", c.get("uri").and_then(Value::as_str).unwrap_or("")),
        Some(other) => format!("[{other} content]"),
        None => String::new(),
    }
}

/// A `resources/read` result: text contents as-is, binary as a note.
pub(super) fn render_resource(result: &Value) -> String {
    let Some(contents) = result.get("contents").and_then(Value::as_array) else {
        return crate::api::truncate(&result.to_string(), crate::api::MAX_TOOL_OUTPUT);
    };
    let parts: Vec<String> = contents
        .iter()
        .map(|c| match (c.get("text").and_then(Value::as_str), c.get("blob").and_then(Value::as_str)) {
            (Some(t), _) => t.to_string(),
            (None, Some(b)) => format!(
                "[binary {}, {} bytes]",
                c.get("mimeType").and_then(Value::as_str).unwrap_or("data"),
                b.len() * 3 / 4
            ),
            _ => String::new(),
        })
        .collect();
    crate::api::truncate(&parts.join("\n"), crate::api::MAX_TOOL_OUTPUT)
}

/// MCP tool results are a list of content blocks; flatten them to text.
pub(super) fn render_tool_result(result: &Value) -> String {
    let text = result.get("content").map(content_text).unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        return format!("ERROR: {}", if text.is_empty() { "tool reported an error".into() } else { text });
    }
    if text.is_empty() {
        // No content blocks (e.g. structured content only) — return the raw JSON.
        crate::api::truncate(&result.to_string(), crate::api::MAX_TOOL_OUTPUT)
    } else {
        crate::api::truncate(&text, crate::api::MAX_TOOL_OUTPUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_tool_names() {
        assert_eq!(sanitize_name("mcp__mini__add"), "mcp__mini__add");
        assert_eq!(sanitize_name("mcp__my.server__do/it"), "mcp__my_server__do_it");
        let long = sanitize_name(&format!("mcp__s__{}", "x".repeat(100)));
        assert_eq!(long.len(), 64);
    }

    #[test]
    fn prompt_arguments_bind_by_name_or_position() {
        let params = vec![("lang".to_string(), true), ("topic".to_string(), false)];
        let got = bind_prompt_args(&params, "rust error handling in depth").unwrap();
        assert_eq!(got["lang"], "rust");
        assert_eq!(got["topic"], "error handling in depth");
        let got = bind_prompt_args(&params, "topic=async io lang=go").unwrap();
        assert_eq!((got["lang"].as_str(), got["topic"].as_str()), (Some("go"), Some("async io")));
        assert_eq!(bind_prompt_args(&params, "").unwrap_err(), "lang");
        assert!(bind_prompt_args(&[], "").unwrap().is_empty());
    }

    #[test]
    fn prompts_and_results_render_as_text() {
        let r = json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": "Review this:"}},
            {"role": "user", "content": {"type": "resource", "resource": {"uri": "file:///a", "text": "fn main() {}"}}},
            {"role": "assistant", "content": {"type": "text", "text": "Sure."}}
        ]});
        assert_eq!(render_prompt(&r).unwrap(), "Review this:\n\nfn main() {}\n\n(assistant) Sure.");
        assert!(render_prompt(&json!({"messages": []})).is_none());
        let res = json!({"contents": [{"uri": "a", "text": "hello"}, {"uri": "b", "mimeType": "image/png", "blob": "AAAA"}]});
        assert_eq!(render_resource(&res), "hello\n[binary image/png, 3 bytes]");
        let err = json!({"isError": true, "content": [{"type": "text", "text": "bad input"}]});
        assert_eq!(render_tool_result(&err), "ERROR: bad input");
    }
}
