//! HTTP helpers and retries: the agent, auth headers, typed error statuses,
//! backoff, the text-only-model and no-tools-model fallbacks, and the retrying
//! chat calls.

use super::stream::{chat_stream, OutputLoop};
use super::{truncate, Message, Streamed, Usage};
use crate::config::{ApiKind, Config};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// An HTTP error status from the provider, kept typed (not just a message)
/// so the retry loop can tell a rate limit from a request that can't succeed.
#[derive(Debug)]
pub struct HttpStatus {
    pub code: u16,
    /// Seconds from a `Retry-After` header, when the server sent one.
    pub retry_after: Option<u64>,
    pub detail: String,
}

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.code, self.detail)
    }
}

impl std::error::Error for HttpStatus {}

/// How long to wait before retrying after `err` on attempt `attempt` (1-based),
/// or None when a retry can't help: any 4xx but timeout/conflict/rate-limit
/// would fail the same way again. Exponential backoff (1s, 2s, 4s…) plus
/// jitter; a server's `Retry-After` wins, capped at a minute.
fn retry_delay(err: &anyhow::Error, attempt: u32) -> Option<Duration> {
    if err.downcast_ref::<OutputLoop>().is_some() {
        return None;
    }
    if let Some(s) = err.downcast_ref::<HttpStatus>() {
        if !matches!(s.code, 408 | 409 | 425 | 429 | 500..=599) {
            return None;
        }
        if let Some(secs) = s.retry_after {
            return Some(Duration::from_secs(secs.min(60)));
        }
    }
    let base = Duration::from_millis(1000u64 << attempt.saturating_sub(1).min(5));
    // No RNG crate on purpose (binary size): the clock's sub-second nanos are
    // plenty to keep several clients from retrying in lockstep.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Some(base + Duration::from_millis(nanos % (base.as_millis() as u64 / 2 + 1)))
}

/// Models that answered a request carrying images with "text only" this run,
/// keyed `provider/model`. A static list can't know every gateway model (Vercel
/// routes to hundreds), so the provider's own 400 is the source of truth.
static NO_IMAGES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn image_key(cfg: &Config) -> String {
    format!("{}/{}", cfg.provider, cfg.model)
}

/// Can the active model see images? False for the providers known to be
/// text-only and for any model that has rejected an image this run.
pub fn takes_images(cfg: &Config) -> bool {
    cfg.supports_images() && !NO_IMAGES.lock().map(|l| l.contains(&image_key(cfg))).unwrap_or(false)
}

fn mark_text_only(cfg: &Config) {
    if let Ok(mut l) = NO_IMAGES.lock() {
        let k = image_key(cfg);
        if !l.contains(&k) {
            l.push(k);
        }
    }
}

/// A 4xx that blames the images in the request (e.g. Vercel's "This API
/// supports text input only. This conversation contains an image.").
fn rejects_images(err: &anyhow::Error) -> bool {
    let Some(s) = err.downcast_ref::<HttpStatus>() else { return false };
    let d = s.detail.to_ascii_lowercase();
    matches!(s.code, 400 | 415 | 422)
        && ["image", "vision", "multimodal", "text input only"].iter().any(|w| d.contains(w))
}

/// On an image rejection with images in play: remember the model is text-only
/// and say so, so the caller resends at once (the next send strips them).
fn retry_without_images(cfg: &Config, messages: &[Message], err: &anyhow::Error) -> Option<String> {
    if !rejects_images(err) || !takes_images(cfg) || messages.iter().all(|m| m.images.is_empty()) {
        return None;
    }
    mark_text_only(cfg);
    Some(format!("{} doesn't take images — resending without them", cfg.model))
}

/// Models that refused a request because it carried a tool schema this run,
/// keyed `provider/model`. Like images, only the provider's own error knows:
/// Gemini serves preview models with function calling switched off, and
/// gateways route to models that never had it.
static NO_TOOLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Can the active model be sent a tool schema? False once it has refused one.
pub fn takes_tools(cfg: &Config) -> bool {
    !NO_TOOLS.lock().map(|l| l.contains(&image_key(cfg))).unwrap_or(false)
}

/// A 4xx saying the model can't call tools at all (Gemini's "Function calling
/// is not enabled for models/…", Ollama's "… does not support tools",
/// OpenRouter's 404 "No endpoints found that support tool use"). Phrases, not
/// the bare word "tool": a bad tool schema is a real error, not this.
fn rejects_tools(err: &anyhow::Error) -> bool {
    let Some(s) = err.downcast_ref::<HttpStatus>() else { return false };
    let d = s.detail.to_ascii_lowercase();
    matches!(s.code, 400 | 404 | 422)
        && [
            "function calling is not enabled",
            "function calling is not supported",
            "does not support function calling",
            "does not support tool",
            "tool calling is not supported",
            "tools are not supported",
            "support tool use",
        ]
        .iter()
        .any(|w| d.contains(w))
}

/// On a tools rejection: remember the model can't call tools and say so, so
/// the caller resends at once as a plain chat.
fn retry_without_tools(cfg: &Config, err: &anyhow::Error) -> Option<String> {
    if !rejects_tools(err) || !takes_tools(cfg) {
        return None;
    }
    if let Ok(mut l) = NO_TOOLS.lock() {
        l.push(image_key(cfg));
    }
    Some(format!(
        "{} can't call tools — continuing as plain chat: it can't run commands or read and edit files (/model to switch)",
        cfg.model
    ))
}

/// What a model without tools is told, so it says it can't act instead of
/// inventing the output of tools its system prompt describes.
const NO_TOOLS_NOTE: &str = "\n\nTools are unavailable with this model: you cannot run commands, read or edit files, or call any tool in this session. Never pretend to; say what you can't do and answer from the conversation alone.";

/// `messages` for a model without tools: the note above appended to the system
/// prompt (not a new message: every protocol already carries that one).
fn without_tools(messages: &[Message]) -> Vec<Message> {
    let mut out = messages.to_vec();
    match out.iter_mut().find(|m| m.role == "system") {
        Some(m) => m.content.push_str(NO_TOOLS_NOTE),
        None => out.insert(0, Message::system(NO_TOOLS_NOTE.trim_start())),
    }
    out
}

/// Models that refused a request because it carried a system prompt this run,
/// keyed `provider/model` (Gemini: "Developer instruction is not enabled for
/// models/…").
static NO_SYSTEM: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn takes_system(cfg: &Config) -> bool {
    !NO_SYSTEM.lock().map(|l| l.contains(&image_key(cfg))).unwrap_or(false)
}

/// A 4xx saying the model takes no system (or "developer") message.
fn rejects_system(err: &anyhow::Error) -> bool {
    let Some(s) = err.downcast_ref::<HttpStatus>() else { return false };
    let d = s.detail.to_ascii_lowercase();
    matches!(s.code, 400 | 422)
        && [
            "developer instruction is not enabled",
            "system instruction is not enabled",
            "developer instruction is not supported",
            "system instruction is not supported",
            "does not support system",
            "does not support developer",
            "system role is not supported",
            "system messages are not supported",
        ]
        .iter()
        .any(|w| d.contains(w))
}

/// On a system-prompt rejection: remember it and say so, so the caller
/// resends at once with the instructions folded into the conversation.
fn retry_without_system(cfg: &Config, messages: &[Message], err: &anyhow::Error) -> Option<String> {
    if !rejects_system(err) || !takes_system(cfg) || messages.iter().all(|m| m.role != "system") {
        return None;
    }
    if let Ok(mut l) = NO_SYSTEM.lock() {
        l.push(image_key(cfg));
    }
    Some(format!("{} takes no system prompt — resending with the instructions in the first message", cfg.model))
}

/// `messages` for a model without a system role: every system message's text
/// moves to the front of the first user message, marked off as instructions.
fn without_system(messages: &[Message]) -> Vec<Message> {
    let (system, mut rest): (Vec<Message>, Vec<Message>) = messages.iter().cloned().partition(|m| m.role == "system");
    let text = system.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n\n");
    if text.is_empty() {
        return rest;
    }
    let head = format!("<instructions>\n{text}\n</instructions>");
    match rest.iter_mut().find(|m| m.role == "user") {
        Some(m) => m.content = format!("{head}\n\n{}", m.content),
        None => rest.insert(0, Message::user(head)),
    }
    rest
}

/// `messages` reshaped for what the active model has refused this run (tools,
/// a system prompt), or None when it takes them as they are.
fn adapted(cfg: &Config, messages: &[Message], with_tools: bool) -> Option<Vec<Message>> {
    let no_tools = with_tools && !takes_tools(cfg);
    let no_system = !takes_system(cfg);
    if !no_tools && !no_system {
        return None;
    }
    let mut out = if no_tools { without_tools(messages) } else { messages.to_vec() };
    if no_system {
        out = without_system(&out);
    }
    Some(out)
}

/// Sleep for `d`, waking early on cancel. Returns false when cancelled.
fn sleep_unless_cancelled(d: Duration, cancel: &AtomicBool) -> bool {
    let deadline = std::time::Instant::now() + d;
    while std::time::Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !cancel.load(Ordering::Relaxed)
}

pub fn agent_http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent("haikucode/0.1")
        .build()
}

/// `req` with the provider's credentials: Anthropic takes `x-api-key` plus a
/// version header, everyone else a Bearer token.
pub(crate) fn authed(req: ureq::Request, cfg: &Config) -> ureq::Request {
    if cfg.chatgpt_sub() {
        // A ChatGPT sign-in: the Codex backend wants the account it bills.
        let account = cfg.oauth_token().map(|t| t.account_id.as_str()).unwrap_or("");
        let req = req
            .set("Authorization", &format!("Bearer {}", cfg.bearer()))
            .set("OpenAI-Beta", "responses=experimental")
            .set("originator", "codex_cli_rs");
        return if account.is_empty() { req } else { req.set("chatgpt-account-id", account) };
    }
    if cfg.auth() == crate::config::AuthMethod::None && cfg.api_key.is_empty() {
        return req; // a keyless server: no empty "Bearer " header
    }
    match cfg.api_kind() {
        ApiKind::Messages => req.set("x-api-key", cfg.bearer()).set("anthropic-version", "2023-06-01"),
        _ => req.set("Authorization", &format!("Bearer {}", cfg.bearer())),
    }
}

/// POST a JSON body; an error status becomes a typed `HttpStatus` (so the
/// retry loop can tell a rate limit from a bad request).
pub(crate) fn post_json(http: &ureq::Agent, cfg: &Config, url: &str, body: &serde_json::Value) -> Result<ureq::Response> {
    match authed(http.post(url), cfg).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, r)) => {
            let retry_after = r.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok());
            let detail = truncate(&r.into_string().unwrap_or_default(), 800);
            Err(HttpStatus { code, retry_after, detail }.into())
        }
        Err(e) => Err(anyhow!("network error: {e}")),
    }
}

/// chat_stream with retry: a dropped SSE chunk can corrupt streamed tool-call
/// JSON, so we re-request a clean response. Transient failures (network,
/// rate limits, 5xx) are retried with backoff; other 4xx fail at once.
#[allow(clippy::too_many_arguments)]
pub fn chat_resilient(
    http: &ureq::Agent,
    cfg: &Config,
    messages: &[Message],
    tools: &serde_json::Value,
    cancel: &AtomicBool,
    mut on_content: impl FnMut(&str),
    mut on_reasoning: impl FnMut(&str),
    mut on_retry: impl FnMut(&str),
) -> Result<Streamed> {
    let tries = 4;
    let mut last: Option<Streamed> = None;
    for attempt in 1..=tries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let reshaped = adapted(cfg, messages, true);
        let tools = takes_tools(cfg).then_some(tools);
        let messages = reshaped.as_deref().unwrap_or(messages);
        match chat_stream(http, cfg, messages, tools, cancel, &mut on_content, &mut on_reasoning) {
            Err(e) => {
                let note = retry_without_images(cfg, messages, &e)
                    .or_else(|| retry_without_tools(cfg, &e))
                    .or_else(|| retry_without_system(cfg, messages, &e));
                if let Some(note) = note {
                    on_retry(&note);
                    continue;
                }
                let Some(wait) = retry_delay(&e, attempt).filter(|_| attempt < tries) else {
                    return Err(e);
                };
                on_retry(&format!(
                    "{e} — retry {attempt}/{} in {:.1}s",
                    tries - 1,
                    wait.as_secs_f32()
                ));
                if !sleep_unless_cancelled(wait, cancel) {
                    break;
                }
                continue;
            }
            Ok((content, calls, usage, native)) => {
                let ok = calls.iter().all(|c| c.args_ok());
                if calls.is_empty() || ok {
                    return Ok((content, calls, usage, native));
                }
                last = Some((content, calls, usage, native));
                if attempt < tries {
                    on_retry(&format!("malformed tool args — retry {attempt}/{}", tries - 1));
                }
            }
        }
    }
    last.ok_or_else(|| anyhow!("no response"))
}

/// A plain (no tools) completion, with one retry on transient errors. Used for
/// internal calls like summarizing the conversation during compaction. Returns
/// the token usage too, so these background calls still count toward cost.
pub fn chat_plain(
    http: &ureq::Agent,
    cfg: &Config,
    messages: &[Message],
    cancel: &AtomicBool,
) -> Result<(String, Option<Usage>)> {
    // One more try than the transient-error retry, for the resend after a
    // model's first system-prompt refusal.
    let tries = 3;
    for attempt in 1..=tries {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let reshaped = adapted(cfg, messages, false);
        let messages = reshaped.as_deref().unwrap_or(messages);
        match chat_stream(http, cfg, messages, None, cancel, |_| {}, |_| {}) {
            Ok((content, _, usage, _)) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(anyhow!("cancelled"));
                }
                return Ok((content, usage));
            }
            Err(e) => {
                if retry_without_images(cfg, messages, &e).is_some() || retry_without_system(cfg, messages, &e).is_some() {
                    continue;
                }
                let Some(wait) = retry_delay(&e, attempt).filter(|_| attempt < tries - 1) else {
                    return Err(e);
                };
                if !sleep_unless_cancelled(wait, cancel) {
                    return Err(anyhow!("cancelled"));
                }
            }
        }
    }
    Err(anyhow!("no response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_only_rejection_marks_the_model_and_strips_images() {
        let cfg = Config { provider: "vercel".into(), model: "reflection/beam-free-test".into(), ..Config::default() };
        let with_img = [Message::user_with_images("look", vec!["data:image/png;base64,AAAA".into()])];
        let err = |code, detail: &str| anyhow::Error::from(HttpStatus { code, retry_after: None, detail: detail.into() });
        let vercel = r#"{"error":{"message":"This API supports text input only. This conversation contains an image."}}"#;
        // Unrelated 400s and image-free requests don't flip the model.
        assert!(retry_without_images(&cfg, &with_img, &err(400, "bad tool schema")).is_none());
        assert!(retry_without_images(&cfg, &[Message::user("hi")], &err(400, vercel)).is_none());
        assert!(retry_without_images(&cfg, &with_img, &err(500, vercel)).is_none());
        assert!(takes_images(&cfg));
        assert!(retry_without_images(&cfg, &with_img, &err(400, vercel)).is_some());
        assert!(!takes_images(&cfg), "remembered for the rest of the run");
        // Only once: a second rejection is a real failure, not another retry.
        assert!(retry_without_images(&cfg, &with_img, &err(400, vercel)).is_none());
        let other = Config { model: "anthropic/claude-sonnet-5.5".into(), ..cfg.clone() };
        assert!(takes_images(&other), "other models keep images");
    }

    #[test]
    fn a_no_tools_rejection_marks_the_model_and_drops_the_schema() {
        let cfg = Config { provider: "gemini".into(), model: "antigravity-preview-test".into(), ..Config::default() };
        let err = |code, detail: &str| anyhow::Error::from(HttpStatus { code, retry_after: None, detail: detail.into() });
        let gemini = r#"[{"error":{"code":400,"message":"Function calling is not enabled for models/antigravity-preview-test","status":"INVALID_ARGUMENT"}}]"#;
        // A broken schema or a server error is not "this model has no tools".
        assert!(retry_without_tools(&cfg, &err(400, "tools[3].function.parameters: unsupported keyword")).is_none());
        assert!(retry_without_tools(&cfg, &err(500, gemini)).is_none());
        assert!(takes_tools(&cfg));
        let note = retry_without_tools(&cfg, &err(400, gemini)).unwrap();
        assert!(note.contains("antigravity-preview-test") && note.contains("/model"));
        assert!(!takes_tools(&cfg), "remembered for the rest of the run");
        // Only once: a second rejection is a real failure, not another retry.
        assert!(retry_without_tools(&cfg, &err(400, gemini)).is_none());
        assert!(takes_tools(&Config { model: "gemini-3-pro".into(), ..cfg.clone() }), "other models keep tools");
        for detail in [
            "registry.ollama.ai/library/gemma3:4b does not support tools",
            "No endpoints found that support tool use.",
            "This model does not support function calling",
        ] {
            assert!(rejects_tools(&err(if detail.starts_with("No") { 404 } else { 400 }, detail)), "{detail}");
        }
        // The model is told, in the system prompt it already gets.
        let sent = without_tools(&[Message::system("You are Haiku Code."), Message::user("do a self check")]);
        assert_eq!(sent.len(), 2);
        assert!(sent[0].content.starts_with("You are Haiku Code.") && sent[0].content.contains("Tools are unavailable"));
        assert_eq!(without_tools(&[Message::user("hi")])[0].role, "system");
    }

    #[test]
    fn a_system_prompt_rejection_folds_the_instructions_into_the_first_message() {
        let cfg = Config { provider: "gemini".into(), model: "antigravity-nosystem-test".into(), ..Config::default() };
        let err = |code, detail: &str| anyhow::Error::from(HttpStatus { code, retry_after: None, detail: detail.into() });
        let gemini = r#"[{"error":{"code":400,"message":"Developer instruction is not enabled for models/antigravity-nosystem-test","status":"INVALID_ARGUMENT"}}]"#;
        let convo = [Message::system("You are Haiku Code."), Message::user("hi"), Message::user("again")];
        assert!(adapted(&cfg, &convo, true).is_none(), "untouched until the model refuses");
        assert!(retry_without_system(&cfg, &convo, &err(400, "bad tool schema")).is_none());
        assert!(retry_without_system(&cfg, &convo[1..], &err(400, gemini)).is_none(), "no system message to drop");
        assert!(retry_without_system(&cfg, &convo, &err(400, gemini)).is_some());
        assert!(retry_without_system(&cfg, &convo, &err(400, gemini)).is_none(), "only once");
        let sent = adapted(&cfg, &convo, true).unwrap();
        assert!(sent.iter().all(|m| m.role != "system"));
        assert_eq!(sent[0].content, "<instructions>\nYou are Haiku Code.\n</instructions>\n\nhi");
        assert_eq!(sent[1].content, "again");
        // Internal no-tools calls (compaction) are reshaped the same way.
        assert_eq!(adapted(&cfg, &convo, false).unwrap().len(), 2);
        assert_eq!(without_system(&[Message::system("s")])[0].role, "user");
    }

    #[test]
    fn retries_only_what_a_retry_can_fix() {
        let status = |code, retry_after| anyhow::Error::from(HttpStatus { code, retry_after, detail: String::new() });
        // A bad request, bad key or missing model fails the same way again.
        for code in [400, 401, 403, 404, 413, 422] {
            assert!(retry_delay(&status(code, None), 1).is_none(), "HTTP {code} must not retry");
        }
        // Rate limits and server trouble back off, growing per attempt.
        let first = retry_delay(&status(429, None), 1).unwrap();
        let third = retry_delay(&status(503, None), 3).unwrap();
        assert!(first >= Duration::from_secs(1) && first < Duration::from_secs(2));
        assert!(third >= Duration::from_secs(4) && third < Duration::from_secs(6));
        // The server's Retry-After wins, capped at a minute.
        assert_eq!(retry_delay(&status(429, Some(7)), 1), Some(Duration::from_secs(7)));
        assert_eq!(retry_delay(&status(503, Some(3600)), 1), Some(Duration::from_secs(60)));
        // Network failures are transient.
        assert!(retry_delay(&anyhow!("network error: connection reset"), 1).is_some());
    }
}
