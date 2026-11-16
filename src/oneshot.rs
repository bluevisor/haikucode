//! One-shot mode (`haikucode "task"`): output modes, piped stdin, `run_oneshot`.

use crate::agent::{UiEvent, WorkerCmd};
use crate::api::Message;
use crate::config::Config;
use crate::{agent, api, money, ui};
use std::io::{Read, Write};

/// How one-shot mode reports: streamed text (default), one JSON object at the
/// end (`--json`), or one JSON object per event (`--stream-json`) — the shapes
/// scripts and CI expect from Claude Code / Codex.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum OutputMode {
    Text,
    Json,
    StreamJson,
}

/// True when stdin is a pipe (`cmd | haikucode …`). A pipe's writer will close
/// it, so reading to EOF is safe even when the producer is slow — unlike a
/// supervisor's socket or an inherited tty-less device, which we only read
/// when data is already waiting.
pub(super) fn stdin_is_pipe() -> bool {
    #[cfg(unix)]
    {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd 0 and a valid, zeroed stat buffer.
        if unsafe { libc::fstat(0, &mut st) } == 0 {
            return (st.st_mode & libc::S_IFMT) == libc::S_IFIFO;
        }
        return false;
    }
    #[allow(unreachable_code)]
    false
}

/// True when stdin has data (or EOF) to read within `timeout_ms`.
pub(super) fn stdin_ready(timeout_ms: i32) -> bool {
    #[cfg(unix)]
    {
        let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: a valid pollfd array of length 1 and a finite timeout.
        let n = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
        return n > 0 && (fds.revents & (libc::POLLIN | libc::POLLHUP)) != 0;
    }
    #[allow(unreachable_code)]
    true
}

pub(super) fn read_stdin_capped(cap: u64) -> String {
    let mut s = String::new();
    let _ = std::io::stdin().lock().take(cap).read_to_string(&mut s);
    s
}

/// Run one task to completion without the TUI. Returns false when the run
/// reported an error (or `--output` couldn't be written), for the exit status.
pub(super) fn run_oneshot(
    mut cfg: Config,
    messages: Vec<Message>,
    session: Option<std::path::PathBuf>,
    task: String,
    output: Option<String>,
    mode: OutputMode,
    perm: u8,
) -> bool {
    let started = std::time::Instant::now();
    let model = cfg.model.clone();
    // The cost counter follows the provider: a custom server without a price
    // list has no known cost (null in the JSON summary).
    let price_list = cfg.effective_price_list();
    let price_currency = price_list
        .as_ref()
        .map(|p| p.price_currency.clone())
        .unwrap_or_default();
    // The TUI refines the window from /models while the user types; a
    // one-shot sends its prompt at once, so look it up first or the first
    // turn is sized against the table's guess. In memory only: never saved.
    if cfg.model_context_window().is_none() && !cfg.context_window_explicit {
        if let Some(n) = api::context_window(&api::agent_http(), &cfg, &cfg.model) {
            cfg.context_window = n;
            cfg.context_window_explicit = true;
        }
    }
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiEvent>();
    let h = agent::spawn(cfg, messages, perm, session, ui_tx);
    let (task_text, attached) = ui::expand_attachments(&task);
    let (images, img_names) = ui::extract_images(&task);
    let mut all = attached;
    all.extend(img_names);
    let text_mode = mode == OutputMode::Text;
    let emit = |v: serde_json::Value| {
        if mode == OutputMode::StreamJson {
            println!("{v}");
        }
    };
    if !all.is_empty() {
        if text_mode {
            eprintln!("\x1b[2mattached: {}\x1b[0m", all.join(", "));
        }
        emit(serde_json::json!({"type": "attached", "files": all}));
    }
    let _ = h.cmd_tx.send(WorkerCmd::User { text: task_text, images, typed: task });

    let mut out = std::io::stdout();
    let mut at_line_start = true;
    // Track the final assistant message (the last committed non-empty one)
    // so --output can write it to disk after the turn.
    let mut live = String::new();
    let mut final_text = String::new();
    let (mut spent, mut tool_calls, mut errors) = (money::Spent::default(), 0u32, Vec::new());
    while let Ok(ev) = ui_rx.recv() {
        match ev {
            UiEvent::Token(t) => {
                if text_mode {
                    print!("{t}");
                    let _ = out.flush();
                }
                emit(serde_json::json!({"type": "token", "text": t}));
                at_line_start = t.ends_with('\n');
                live.push_str(&t);
            }
            UiEvent::ResetLive => {
                // The worker is retrying the request: what streamed so far was
                // a failed attempt and will be re-sent, so it is not a result
                // candidate. Tell JSON consumers to discard it; in text mode a
                // newline at least separates the repeat from the fragment.
                live.clear();
                if text_mode && !at_line_start {
                    println!();
                    at_line_start = true;
                }
                emit(serde_json::json!({"type": "reset"}));
            }
            UiEvent::AssistantCommit => {
                // The reply (or the text before a tool round) finished — it is
                // the latest candidate for the final result.
                if !live.trim().is_empty() {
                    final_text = std::mem::take(&mut live);
                } else {
                    live.clear();
                }
            }
            UiEvent::ToolStart { name, summary } => {
                tool_calls += 1;
                if text_mode {
                    if !at_line_start {
                        println!();
                    }
                    println!("\x1b[34m◉ {name}\x1b[0m {summary}");
                    at_line_start = true;
                }
                emit(serde_json::json!({"type": "tool_start", "name": name, "summary": summary}));
            }
            UiEvent::ToolResult { ok, preview } => {
                if text_mode {
                    let color = if ok { "\x1b[2m" } else { "\x1b[31m" };
                    for line in preview.lines() {
                        println!("  {color}{line}\x1b[0m");
                    }
                    at_line_start = true;
                }
                emit(serde_json::json!({"type": "tool_result", "ok": ok, "preview": preview}));
            }
            UiEvent::Notice(s) | UiEvent::Progress(s) => {
                if text_mode {
                    if !at_line_start {
                        println!();
                    }
                    println!("\x1b[2m{s}\x1b[0m");
                    at_line_start = true;
                }
                emit(serde_json::json!({"type": "notice", "text": s}));
            }
            UiEvent::Error(s) => {
                if text_mode {
                    if !at_line_start {
                        println!();
                    }
                    at_line_start = true;
                }
                // Errors always go to stderr so a JSON consumer's stdout stays clean.
                eprintln!("\x1b[31m{s}\x1b[0m");
                emit(serde_json::json!({"type": "error", "text": s}));
                errors.push(s);
            }
            UiEvent::Usage(s) | UiEvent::Spend(s) => spent.add(s),
            UiEvent::TurnDone => break,
            UiEvent::Reasoning(t) => {
                if text_mode {
                    // Thinking tokens: print dimmed, no newline.
                    print!("\x1b[2m{t}\x1b[0m");
                    let _ = out.flush();
                    at_line_start = false;
                }
                emit(serde_json::json!({"type": "reasoning", "text": t}));
            }
            UiEvent::Diff(d) => {
                if text_mode {
                    if !at_line_start {
                        println!();
                    }
                    println!("\x1b[2m{d}\x1b[0m");
                    at_line_start = true;
                }
                emit(serde_json::json!({"type": "diff", "text": d}));
            }
            UiEvent::Approval { desc, .. } => {
                // Nobody is at a terminal to ask. YOLO runs shouldn't get
                // here; anything else is declined (the model is told and goes
                // on), never silently approved.
                if perm == agent::PERM_AUTO {
                    let _ = h.appr_tx.send(agent::ApprovalResponse::Yes);
                    eprintln!("\x1b[2m[auto-approved: {desc}]\x1b[0m");
                } else {
                    let _ = h.appr_tx.send(agent::ApprovalResponse::No);
                    let note = format!("declined: {desc} (one-shot runs need --auto or \"permission\": \"yolo\")");
                    eprintln!("\x1b[33m[{note}]\x1b[0m");
                    emit(serde_json::json!({"type": "notice", "text": note}));
                }
            }
            UiEvent::Question { prompt, reply } => {
                // In auto mode, answer with an empty decline.
                eprintln!("\x1b[33m[question skipped in one-shot mode] {prompt}\x1b[0m");
                let _ = reply.send(None);
            }
            UiEvent::PasswordRequest { prompt, reply } => {
                eprintln!("\x1b[33m[sudo password requested but not available in one-shot mode] {prompt}\x1b[0m");
                let _ = reply.send(None);
            }
            _ => {}
        }
    }
    if text_mode && !at_line_start {
        println!();
    }
    let cost = price_list.map(|p| spent.cost(p.price_in, p.price_cached(), p.price_out));
    let result = final_text.trim_end().to_string();
    if mode != OutputMode::Text {
        let summary = serde_json::json!({
            "type": "result",
            "result": result,
            "model": model,
            "usage": {
                "prompt_tokens": spent.prompt,
                "completion_tokens": spent.completion,
                "cached_tokens": spent.cached,
                "cache_write_tokens": spent.written,
            },
            "cost": cost,
            "cost_currency": price_currency,
            "tool_calls": tool_calls,
            "duration_ms": started.elapsed().as_millis() as u64,
            "errors": errors,
            "is_error": !errors.is_empty(),
        });
        println!("{summary}");
    }
    let mut ok = errors.is_empty();
    if let Some(path) = output {
        let mut text = result;
        text.push('\n');
        match std::fs::write(&path, text) {
            Ok(()) => eprintln!("\x1b[2mwrote {path}\x1b[0m"),
            Err(e) => {
                eprintln!("\x1b[31mcould not write {path}: {e}\x1b[0m");
                ok = false;
            }
        }
    }
    let _ = h.cmd_tx.send(WorkerCmd::Quit);
    let _ = h.join.join();
    ok
}
