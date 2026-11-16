//! Streaming completions: SSE parsing, chat/completions deltas, usage and
//! sampling-loop detection.

use super::payload::{cache_key, chat_native, messages_payload, reasoning_fields};
use super::{post_json, takes_images, AccumCall, Message, Streamed, Usage};
use crate::config::{ApiKind, Config};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};

/// The model's stream turned into the same block of text over and over (a
/// sampling loop): it would never end by itself, so the response is cut off.
#[derive(Debug)]
pub struct OutputLoop;

impl std::fmt::Display for OutputLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the model got stuck repeating itself, so haikucode stopped the response. Try again or \
             rephrase; a local reasoning model may need its reasoning switched off."
        )
    }
}

impl std::error::Error for OutputLoop {}

/// Whether `text` ends in one block repeated back to back: 8+ times, the block
/// 40+ bytes with 10+ distinct bytes (so a long `=====` rule or a run of
/// spaces isn't a loop).
fn is_looping(text: &[u8]) -> bool {
    const MIN_BLOCK: usize = 40;
    const MAX_BLOCK: usize = 1200;
    const REPEATS: usize = 8;
    let n = text.len();
    (MIN_BLOCK..=MAX_BLOCK.min(n / REPEATS)).any(|p| {
        let span = &text[n - p * REPEATS..];
        span[..span.len() - p] == span[p..] && {
            let mut seen = [false; 256];
            span[..p].iter().for_each(|&b| seen[b as usize] = true);
            seen.iter().filter(|&&s| s).count() >= 10
        }
    })
}

/// chat/completions usage: OpenAI reports cache hits in
/// `prompt_tokens_details.cached_tokens`, DeepSeek in `prompt_cache_hit_tokens`.
#[derive(Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<serde_json::Value>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
}

impl ChatUsage {
    fn normalize(&self) -> Usage {
        let cached = self.prompt_cache_hit_tokens.unwrap_or_else(|| {
            self.prompt_tokens_details
                .as_ref()
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32
        });
        Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self
                .completion_tokens
                .unwrap_or(self.total_tokens.saturating_sub(self.prompt_tokens)),
            cached_tokens: cached.min(self.prompt_tokens),
            cache_write_tokens: 0,
        }
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}
#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}
#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    /// OpenRouter and Vercel's AI Gateway stream thinking under this name.
    reasoning: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}
#[derive(Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<DeltaFn>,
}
#[derive(Deserialize)]
struct DeltaFn {
    name: Option<String>,
    arguments: Option<String>,
}

/// Sampling-loop detection over the last stretch of streamed text (reasoning
/// and reply), checked every 512 bytes.
#[derive(Default)]
pub(crate) struct LoopGuard {
    recent: Vec<u8>,
    unchecked: usize,
}

impl LoopGuard {
    /// Add streamed text; Err once the stream has turned into a loop.
    pub(crate) fn feed(&mut self, t: &str) -> Result<()> {
        self.recent.extend_from_slice(t.as_bytes());
        if self.recent.len() > 16_384 {
            self.recent.drain(..self.recent.len() - 12_288);
        }
        self.unchecked += t.len();
        if self.unchecked >= 512 {
            self.unchecked = 0;
            if is_looping(&self.recent) {
                return Err(OutputLoop.into());
            }
        }
        Ok(())
    }
}

/// Feed each SSE `data:` payload of `resp` to `f` until it returns true, the
/// stream ends, or the turn is cancelled.
pub(crate) fn for_each_sse(
    resp: ureq::Response,
    cancel: &AtomicBool,
    mut f: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    for line in BufReader::new(resp.into_reader()).lines() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let line = line.map_err(|e| anyhow!("stream read error: {e}"))?;
        let Some(data) = line.trim().strip_prefix("data:") else { continue };
        if f(data.trim())? {
            break;
        }
    }
    Ok(())
}

/// One streaming completion over the configured protocol. Calls
/// `on_content`/`on_reasoning` as deltas arrive. `tools` None omits the tool
/// schema (internal calls like context compaction).
pub(super) fn chat_stream(
    http: &ureq::Agent,
    cfg: &Config,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    cancel: &AtomicBool,
    on_content: impl FnMut(&str),
    on_reasoning: impl FnMut(&str),
) -> Result<Streamed> {
    let stripped: Vec<Message>;
    let messages = if !takes_images(cfg) && messages.iter().any(|m| !m.images.is_empty()) {
        stripped = messages.iter().cloned().map(|mut m| { m.images.clear(); m }).collect();
        &stripped[..]
    } else {
        messages
    };
    match cfg.api_kind() {
        ApiKind::Chat => chat_completions(http, cfg, messages, tools, cancel, on_content, on_reasoning),
        ApiKind::Messages => crate::native::anthropic_stream(http, cfg, messages, tools, cancel, on_content, on_reasoning),
        ApiKind::Responses => crate::native::responses_stream(http, cfg, messages, tools, cancel, on_content, on_reasoning),
    }
}

/// OpenAI chat/completions, streamed.
fn chat_completions(
    http: &ureq::Agent,
    cfg: &Config,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    cancel: &AtomicBool,
    mut on_content: impl FnMut(&str),
    mut on_reasoning: impl FnMut(&str),
) -> Result<Streamed> {
    let url = format!("{}/chat/completions", cfg.endpoint_base().trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages_payload(messages, cfg.supports_images(), &cfg.model),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if let Some(t) = cfg.temperature() {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(t) = tools {
        body["tools"] = t.clone();
        body["tool_choice"] = serde_json::json!("auto");
    }
    for (k, v) in reasoning_fields(cfg) {
        body[k] = v;
    }
    if cfg.provider == "openai" {
        body["prompt_cache_key"] = serde_json::json!(cache_key(messages));
    }
    let resp = post_json(http, cfg, &url, &body)?;

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<AccumCall> = Vec::new();
    let mut usage: Option<Usage> = None;
    let mut guard = LoopGuard::default();
    for_each_sse(resp, cancel, |data| {
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) else { return Ok(false) };
        if let Some(u) = chunk.usage {
            usage = Some(u.normalize());
        }
        let Some(choice) = chunk.choices.into_iter().next() else { return Ok(false) };
        let delta = choice.delta;
        if let Some(r) = delta.reasoning_content.or(delta.reasoning).filter(|r| !r.is_empty()) {
            on_reasoning(&r);
            guard.feed(&r)?;
            reasoning.push_str(&r);
        }
        if let Some(c) = delta.content.filter(|c| !c.is_empty()) {
            on_content(&c);
            content.push_str(&c);
            guard.feed(&c)?;
        }
        for tc in delta.tool_calls.unwrap_or_default() {
            if calls.len() <= tc.index {
                calls.resize(tc.index + 1, AccumCall::default());
            }
            let slot = &mut calls[tc.index];
            if let Some(id) = tc.id.filter(|id| !id.is_empty()) {
                slot.id = id;
            }
            if let Some(f) = tc.function {
                if let Some(n) = f.name.filter(|n| !n.is_empty()) {
                    slot.name = n;
                }
                if let Some(a) = f.arguments {
                    slot.args.push_str(&a);
                }
            }
        }
        Ok(false)
    })?;
    calls.retain(|c| !c.name.is_empty());
    Ok((content, calls, usage, chat_native(cfg, reasoning)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_usage_reads_cache_hits_from_openai_and_deepseek() {
        let u = |v: serde_json::Value| serde_json::from_value::<ChatUsage>(v).unwrap().normalize();
        let openai = u(serde_json::json!({"prompt_tokens": 1000, "completion_tokens": 20, "total_tokens": 1020, "prompt_tokens_details": {"cached_tokens": 768}}));
        assert_eq!(openai, Usage { prompt_tokens: 1000, completion_tokens: 20, cached_tokens: 768, cache_write_tokens: 0 });
        let deepseek = u(serde_json::json!({"prompt_tokens": 500, "total_tokens": 530, "prompt_cache_hit_tokens": 448, "prompt_cache_miss_tokens": 52}));
        assert_eq!(deepseek, Usage { prompt_tokens: 500, completion_tokens: 30, cached_tokens: 448, cache_write_tokens: 0 });
    }

    #[test]
    fn a_paragraph_repeated_back_to_back_is_a_loop() {
        // The paragraph a local Qwen got stuck on (2026-10-06).
        let para = "The 3D Mandelbrot set is usually rendered by evaluating the DE in c-space: \
                    z = c; iterate z = z^2 + c. The classic 3D Mandelbrot shape (the \"Mandelbrot\" \
                    with the antenna) is obtained with c = (0,0,0.7885)? No, that's not right either.\n\n";
        let mut text = String::from("Let me think about the shader first.\n\n");
        for _ in 0..7 {
            text.push_str(para);
        }
        assert!(!is_looping(text.as_bytes()), "7 repeats is still under the bar");
        text.push_str(para);
        assert!(is_looping(text.as_bytes()), "8 repeats is a loop");
        // Mid-block: the tail still repeats, just phase-shifted.
        text.push_str(&para[..90]);
        assert!(is_looping(text.as_bytes()));
    }

    #[test]
    fn ordinary_output_is_not_a_loop() {
        let rule = "=".repeat(2000);
        assert!(!is_looping(rule.as_bytes()), "a long rule has too few distinct bytes");
        assert!(!is_looping(" ".repeat(5000).as_bytes()));
        let prose: String = (0..300).map(|i| format!("Step {i}: update the uniform and redraw. ")).collect();
        assert!(!is_looping(prose.as_bytes()), "similar lines that differ aren't a loop");
        assert!(!is_looping(b"short"));
    }
}
