//! Request bodies: messages on the wire, replayed reasoning, cache keys and
//! per-provider reasoning fields.

use super::Message;
use crate::config::Config;

/// Serialize messages for the API request. When `supports_images` is true, a
/// message with images becomes the OpenAI multimodal form (`content` as an array
/// of text + image_url parts); otherwise images are silently dropped and the
/// message serializes as plain text (DeepSeek V4 Pro and Zhipu reject `image_url`).
///
/// An assistant turn that streamed `reasoning_content` from this same `model`
/// gets it back on the wire: DeepSeek's thinking mode returns 400 on a request
/// with tools unless every earlier assistant turn carries its reasoning.
pub(super) fn messages_payload(messages: &[Message], supports_images: bool, model: &str) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            let plain = |m: &Message| {
                let mut v = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.remove("native"); // haikucode's own bookkeeping, not the wire format
                    if let Some(r) = chat_reasoning(m, model) {
                        o.insert("reasoning_content".into(), serde_json::json!(r));
                    }
                }
                v
            };
            if m.images.is_empty() {
                return plain(m);
            }
            if !supports_images {
                // Drop the payload, not just the multimodal form: serializing
                // `m` as-is would ship the base64 blobs in an `images` field.
                let mut stripped = m.clone();
                stripped.images.clear();
                return plain(&stripped);
            }
            let mut parts: Vec<serde_json::Value> = Vec::new();
            if !m.content.is_empty() {
                parts.push(serde_json::json!({"type":"text","text":m.content}));
            }
            for uri in &m.images {
                parts.push(serde_json::json!({
                    "type":"image_url",
                    "image_url":{"url":uri}
                }));
            }
            serde_json::json!({"role": m.role, "content": parts})
        })
        .collect()
}

/// The chat/completions native form of an assistant turn: just its streamed
/// reasoning. Plain text bound to no signature, so unlike the Messages and
/// Responses forms it survives session resume and compaction.
pub(super) fn chat_native(cfg: &Config, reasoning: String) -> Option<serde_json::Value> {
    (!reasoning.is_empty()).then(|| serde_json::json!({"api": "chat", "model": cfg.model, "reasoning_content": reasoning}))
}

/// A stored assistant turn's reasoning, if it streamed from chat/completions
/// with `model` (another model never sees it).
fn chat_reasoning<'a>(m: &'a Message, model: &str) -> Option<&'a str> {
    let n = m.native.as_ref().filter(|_| m.role == "assistant")?;
    (n["api"] == "chat" && n["model"] == model).then(|| n["reasoning_content"].as_str()).flatten()
}

/// Is this native turn bound to the exact history it was generated over
/// (signed thinking, encrypted reasoning) and so must go when that history is
/// rewritten? Chat reasoning is plain text and stays.
pub fn native_is_history_bound(n: &serde_json::Value) -> bool {
    n["api"] != "chat"
}

/// A stable prompt-cache key for this conversation (OpenAI routes requests
/// sharing a key to the same cache): a hash of the system prompt.
pub(crate) fn cache_key(messages: &[Message]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in messages.first().map(|m| m.content.as_bytes()).unwrap_or_default() {
        h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("haikucode-{h:016x}")
}

/// chat/completions request fields that carry the `reasoning` setting, per
/// provider — only fields that provider accepts (OpenAI 400s on unknown
/// ones). "auto" sends nothing, leaving the model's own default.
pub(crate) fn reasoning_fields(cfg: &Config) -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    let level = cfg.reasoning.as_str();
    if level == "auto" {
        return Vec::new();
    }
    let off = level == "off";
    let top = matches!(level, "xhigh" | "max");
    // Most effort dials stop at high.
    let effort = if off { "none" } else if top { "high" } else { level };
    match cfg.provider.as_str() {
        // DeepSeek V4: thinking on/off plus an effort dial of low/high/max.
        "deepseek" if off => vec![("thinking", json!({"type": "disabled"}))],
        "deepseek" => vec![
            ("thinking", json!({"type": "enabled"})),
            ("reasoning_effort", json!(if top { "max" } else if level == "low" { "low" } else { "high" })),
        ],
        // Zhipu switches thinking on or off; there's no effort dial.
        "zai" => vec![("thinking", json!({"type": if off { "disabled" } else { "enabled" }}))],
        "openai" if top => vec![("reasoning_effort", json!(crate::native::openai_top_effort(&cfg.model)))],
        "openai" | "google" | "groq" => vec![("reasoning_effort", json!(effort))],
        // OpenRouter's scale ends at xhigh and maps it to each model's own.
        "openrouter" if top => vec![("reasoning", json!({"effort": "xhigh"}))],
        // Vercel's AI Gateway takes the same object, up to max, and maps it
        // to each provider's own setting.
        "vercel" => vec![("reasoning", if off { json!({"enabled": false}) } else { json!({"effort": level}) })],
        "openrouter" => vec![("reasoning", if off { json!({"enabled": false}) } else { json!({"effort": level}) })],
        // Its own Messages API carries effort; the compat endpoint isn't mapped.
        "anthropic" => Vec::new(),
        // Self-hosted servers (Splash, vLLM, SGLang, llama.cpp): the chat
        // template's thinking switch, plus effort for those that read it.
        _ => vec![
            ("chat_template_kwargs", json!({"enable_thinking": !off})),
            ("reasoning_effort", json!(effort)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_maps_to_fields_each_provider_accepts() {
        let fields = |provider: &str, level: &str| {
            let c = Config { provider: provider.into(), reasoning: level.into(), ..Config::default() };
            serde_json::to_value(reasoning_fields(&c).into_iter().collect::<std::collections::BTreeMap<_, _>>()).unwrap()
        };
        use serde_json::json as j;
        assert_eq!(fields("openai", "auto"), j!({}), "auto sends nothing anywhere");
        assert_eq!(fields("deepseek", "high"), j!({"thinking": {"type": "enabled"}, "reasoning_effort": "high"}));
        assert_eq!(fields("deepseek", "low"), j!({"thinking": {"type": "enabled"}, "reasoning_effort": "low"}));
        assert_eq!(fields("deepseek", "medium"), j!({"thinking": {"type": "enabled"}, "reasoning_effort": "high"}));
        assert_eq!(fields("deepseek", "off"), j!({"thinking": {"type": "disabled"}}));
        assert_eq!(fields("zai", "off"), j!({"thinking": {"type": "disabled"}}));
        assert_eq!(fields("openai", "low"), j!({"reasoning_effort": "low"}));
        assert_eq!(fields("google", "off"), j!({"reasoning_effort": "none"}));
        assert_eq!(fields("openrouter", "medium"), j!({"reasoning": {"effort": "medium"}}));
        assert_eq!(fields("openrouter", "off"), j!({"reasoning": {"enabled": false}}));
        assert_eq!(fields("vercel", "max"), j!({"reasoning": {"effort": "max"}}));
        assert_eq!(fields("vercel", "off"), j!({"reasoning": {"enabled": false}}));
        assert_eq!(fields("anthropic", "high"), j!({}));
        // xhigh/max: passed on where the dial has them, else capped at high.
        assert_eq!(fields("google", "max"), j!({"reasoning_effort": "high"}));
        assert_eq!(fields("groq", "xhigh"), j!({"reasoning_effort": "high"}));
        assert_eq!(fields("openrouter", "max"), j!({"reasoning": {"effort": "xhigh"}}));
        assert_eq!(fields("deepseek", "max"), j!({"thinking": {"type": "enabled"}, "reasoning_effort": "max"}));
        assert_eq!(fields("deepseek", "xhigh"), j!({"thinking": {"type": "enabled"}, "reasoning_effort": "max"}));
        assert_eq!(fields("splash", "xhigh")["reasoning_effort"], "high");
        assert_eq!(
            fields("splash", "off"),
            j!({"chat_template_kwargs": {"enable_thinking": false}, "reasoning_effort": "none"}),
            "self-hosted servers get the template switch"
        );
    }

    #[test]
    fn chat_reasoning_is_replayed_to_the_same_model_only() {
        let cfg = Config::default();
        let mut turn = Message { role: "assistant".into(), content: String::new(), images: vec![], tool_calls: None, tool_call_id: None, native: None };
        turn.native = chat_native(&cfg, "Need to read the file.".into());
        let v = &messages_payload(&[turn.clone()], true, &cfg.model)[0];
        assert_eq!(v["reasoning_content"], "Need to read the file.");
        assert!(v.get("native").is_none(), "{v}");
        let v = &messages_payload(&[turn.clone()], true, "deepseek-v4-pro")[0];
        assert!(v.get("reasoning_content").is_none(), "another model never sees it: {v}");
        // It rides along in saved sessions, so a resumed run can replay it.
        let back: Message = serde_json::from_str(&serde_json::to_string(&turn).unwrap()).unwrap();
        assert_eq!(chat_reasoning(&back, &cfg.model), Some("Need to read the file."));
        assert!(!native_is_history_bound(back.native.as_ref().unwrap()));
        assert!(chat_native(&cfg, String::new()).is_none(), "no reasoning, no native turn");
    }

    #[test]
    fn native_turns_never_reach_chat_completions() {
        let mut m = Message::user("hi");
        m.native = Some(serde_json::json!({"api": "messages", "parts": []}));
        let v = &messages_payload(&[m], true, "m")[0];
        assert!(v.get("native").is_none(), "{v}");
        assert_eq!(v["content"], "hi");
    }

    #[test]
    fn images_are_dropped_entirely_for_providers_without_vision() {
        let m = Message::user_with_images("look", vec!["data:image/png;base64,AAAA".into()]);
        let v = &messages_payload(std::slice::from_ref(&m), false, "m")[0];
        assert!(v.get("images").is_none(), "{v}");
        assert_eq!(v["content"], "look");
        let v = &messages_payload(&[m], true, "m")[0];
        assert!(v["content"].is_array());
    }
}
