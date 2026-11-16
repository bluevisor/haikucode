//! The worker thread's startup and command loop: `spawn` and the `WorkerCmd` dispatch.

use super::compact::estimate_tokens;
use super::session::system_prefix_len;
use super::{repair_orphans, spend, ApprovalResponse, GenerationStats, Handles, Shared, UiEvent, Worker, WorkerCmd, SESSION_START_TAG};
use crate::api::{self, Message};
use crate::config::{Config, ConfigPatch};
use crate::hooks::{Hooks, Outcome};
use crate::mcp::Mcp;
use crate::policy::Policy;
use crate::system_prompt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

pub fn spawn(
    cfg: Config,
    messages: Vec<Message>,
    perm_start: u8,
    session: Option<PathBuf>,
    ui: Sender<UiEvent>,
) -> Handles {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
    let (appr_tx, appr_rx) = mpsc::channel::<ApprovalResponse>();
    let cancel = Arc::new(AtomicBool::new(false));
    let perm = Arc::new(AtomicU8::new(perm_start));
    let shared = Shared { cancel: cancel.clone(), perm: perm.clone() };

    let join = std::thread::spawn(move || {
        // The protected prefix is the leading run of system messages (prompt,
        // memory, project context, git state). Counting by role — rather than
        // taking the startup message count — keeps /reset and /compact correct
        // for resumed sessions, where startup messages span the whole history.
        let mut messages = messages;
        let policy = Policy::load();
        let hooks = Hooks::load();
        if !policy.is_empty() {
            let _ = ui.send(UiEvent::Notice(format!(
                "permission rules: {} allow, {} deny (/permissions)",
                policy.allow.len(),
                policy.deny.len()
            )));
        }
        if !hooks.is_empty() {
            let _ = ui.send(UiEvent::Notice(format!("hooks: {} configured (/hooks)", hooks.hooks.len())));
        }
        if cfg.update_check {
            let ui = ui.clone();
            std::thread::spawn(move || {
                if let Some(msg) = crate::update::check_on_launch(&api::agent_http()) {
                    let _ = ui.send(UiEvent::Notice(msg));
                }
            });
        }
        if hooks.has("SessionStart") {
            if let Outcome::Continue { context } = hooks.run("SessionStart", "", &serde_json::json!({})) {
                // A resumed session carries the previous launch's copy; replace
                // it rather than stacking one per launch.
                messages.retain(|m| !(m.role == "system" && m.content.starts_with(SESSION_START_TAG)));
                if !context.trim().is_empty() {
                    let at = system_prefix_len(&messages);
                    messages.insert(at, Message::system(format!("{SESSION_START_TAG}\n{context}")));
                }
            }
        }
        let system_len = system_prefix_len(&messages);
        // Launch MCP servers (config.json's and the project's .mcp.json) before
        // the loop; this can take a moment per server.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let entries = crate::mcp::entries(&cfg.mcp_servers, &cwd);
        let mcp = if entries.is_empty() {
            Mcp::disabled()
        } else {
            let _ = ui.send(UiEvent::Notice(format!("starting {} MCP server(s)…", entries.len())));
            let mcp = Mcp::start(entries, crate::config::config_dir());
            for s in mcp.status() {
                let msg = format!("mcp {}: {}", s.name, s.detail);
                let _ = ui.send(if s.ok || s.waiting { UiEvent::Notice(msg) } else { UiEvent::Error(msg) });
            }
            let palette = mcp.prompt_palette();
            if !palette.is_empty() {
                let _ = ui.send(UiEvent::McpPrompts(palette));
            }
            mcp
        };
        let tools = api::tools_spec_with(mcp.tools());
        // Estimate prompt tokens from the (possibly resumed) message list so
        // auto-compaction triggers correctly on the first turn rather than
        // starting from zero and never firing.
        let est_prompt = estimate_tokens(&messages);
        let mut w = Worker {
            http: api::agent_http(),
            cfg,
            messages,
            system_len,
            cancel,
            suggest_cancel: Arc::new(AtomicBool::new(true)),
            perm,
            ui,
            appr_rx,
            session,
            last_prompt: est_prompt,
            quiet: false,
            pending_images: Vec::new(),
            mcp,
            tools,
            policy,
            hooks,
            recent_calls: Vec::new(),
            ctx_detected: Arc::new(Mutex::new(None)),
            ctx_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            balance_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            turns: Vec::new(),
            last_save: std::time::Instant::now(),
            generation: GenerationStats::default(),
        };
        let _ = w.ui.send(UiEvent::SessionLabel(w.session_label()));
        w.ensure_oauth_fresh(); // keep a resumed subscription login authenticated
        w.refresh_balance(); // initial account balance for the status line
        w.refresh_context_window(); // refine the ctx window from the provider
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                WorkerCmd::Quit => break,
                WorkerCmd::Reset => {
                    // A follow-up suggestion for the old conversation mustn't land here.
                    w.suggest_cancel.store(true, Ordering::Relaxed);
                    let old_len = w.messages.len();
                    w.messages.truncate(w.system_len);
                    w.turns.clear();
                    w.last_prompt = 0;
                    if w.messages.len() != old_len {
                        w.save_session();
                    }
                    let _ = w.ui.send(UiEvent::Notice("context cleared.".into()));
                    let _ = w.ui.send(UiEvent::Context(0));
                }
                WorkerCmd::New => {
                    w.suggest_cancel.store(true, Ordering::Relaxed);
                    // The current session stays saved and resumable; carry on
                    // in a fresh one (its file appears with the first save).
                    if w.session.is_some() {
                        w.session = Some(crate::sessions::Store::here().new_path());
                    }
                    w.messages.truncate(w.system_len);
                    w.turns.clear();
                    w.last_prompt = 0;
                    let _ = w.ui.send(UiEvent::Notice(
                        "new session — the previous one stays saved (/resume to go back).".into(),
                    ));
                    let _ = w.ui.send(UiEvent::Context(0));
                    let _ = w.ui.send(UiEvent::SessionLabel(w.session_label()));
                }
                WorkerCmd::ListSessions
                | WorkerCmd::Resume(_)
                | WorkerCmd::Rename(_)
                | WorkerCmd::Fork
                | WorkerCmd::Export(_)
                | WorkerCmd::Sessions(_) => {
                    w.session_cmd(cmd);
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::Compact(focus) => {
                    w.cancel.store(false, Ordering::Relaxed);
                    if w.compact(focus.as_deref()) {
                        w.save_session();
                    }
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::Shell(cmd) => {
                    w.cancel.store(false, Ordering::Relaxed);
                    w.run_shell(&cmd);
                    w.save_session();
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::Inject(note) => {
                    w.messages.push(Message::user(note));
                    w.save_session();
                }
                WorkerCmd::Update(force) => {
                    // Off the worker thread: a download mustn't hold up a turn.
                    let (http, ui) = (w.http.clone(), w.ui.clone());
                    std::thread::spawn(move || {
                        let note = |m: String| {
                            let _ = ui.send(UiEvent::Notice(m));
                        };
                        match crate::update::run(&http, force, &note) {
                            Ok(m) => note(m),
                            Err(e) => {
                                let _ = ui.send(UiEvent::Error(format!("update failed: {e}")));
                            }
                        }
                    });
                }
                WorkerCmd::ReloadSettings => {
                    w.policy = Policy::load();
                    w.hooks = Hooks::load();
                    let _ = w.ui.send(UiEvent::Notice(format!(
                        "settings reloaded: {} allow / {} deny rule(s), {} hook(s)",
                        w.policy.allow.len(),
                        w.policy.deny.len(),
                        w.hooks.hooks.len()
                    )));
                }
                WorkerCmd::SetModel(m) => {
                    w.cfg.model = m.clone();
                    w.cfg.sync_active_custom();
                    w.cfg.sync_prices(); // a model can carry its own prices
                    w.cfg.persist_model();
                    w.refresh_context_window(); // new model may have a different window
                    let _ = w.ui.send(UiEvent::ModelChanged(m.clone()));
                    let _ = w.ui.send(UiEvent::Notice(format!("model set to {m}")));
                }
                WorkerCmd::Login(name) => {
                    w.cancel.store(false, Ordering::Relaxed);
                    w.login(&name);
                    let _ = w.ui.send(UiEvent::TurnDone);
                }

                WorkerCmd::ListModels => {
                    w.ensure_oauth_fresh();
                    match api::list_models(&w.http, &w.cfg) {
                        Ok(ids) => {
                            let _ = w.ui.send(UiEvent::ModelList(api::chat_models(ids)));
                        }
                        Err(e) => {
                            let _ = w.ui.send(UiEvent::ModelListFailed(e.to_string()));
                        }
                    }
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::ListTurns => {
                    let items: Vec<String> = w
                        .turns
                        .iter()
                        .enumerate()
                        .rev()
                        .map(|(i, t)| {
                            // One picker row: a multi-line prompt is flattened.
                            let flat = t.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
                            let line: String = flat.chars().take(80).collect();
                            let more = if flat.chars().count() > 80 { "…" } else { "" };
                            format!("{}. {line}{more}", i + 1)
                        })
                        .collect();
                    if items.is_empty() {
                        let _ = w.ui.send(UiEvent::Notice("nothing to rewind in this session yet.".into()));
                    }
                    let _ = w.ui.send(UiEvent::TurnList(items));
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::Rewind(n) => {
                    w.rewind(n);
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::ListMcp => {
                    if w.mcp.is_empty() {
                        let _ = w.ui.send(UiEvent::Notice(
                            "no MCP servers configured (add \"mcp_servers\" to config.json, or a project .mcp.json).".into(),
                        ));
                    } else {
                        for line in w.mcp.describe() {
                            let _ = w.ui.send(UiEvent::Notice(line));
                        }
                    }
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::Mcp(args) => {
                    let mut words = args.split_whitespace();
                    let (sub, name) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
                    let ui = w.ui.clone();
                    let result = match (sub, name) {
                        (_, "") => Err("usage: /mcp [approve|deny|login|logout] <server>".to_string()),
                        ("approve", n) => w.mcp.decide(n, true),
                        ("deny", n) => w.mcp.decide(n, false),
                        ("login", n) => w.mcp.login(n, &|m| {
                            let _ = ui.send(UiEvent::Notice(m));
                        }),
                        ("logout", n) => w.mcp.logout(n),
                        (other, _) => Err(format!("unknown /mcp command {other:?}: approve, deny, login or logout")),
                    };
                    match result {
                        Ok(m) => {
                            let _ = w.ui.send(UiEvent::Notice(m));
                        }
                        Err(e) => {
                            let _ = w.ui.send(UiEvent::Error(e));
                        }
                    }
                    // Servers may have started or stopped: refresh what the model sees.
                    w.tools = api::tools_spec_with(w.mcp.tools());
                    let _ = w.ui.send(UiEvent::McpPrompts(w.mcp.prompt_palette()));
                    let _ = w.ui.send(UiEvent::TurnDone);
                }
                WorkerCmd::McpPrompt { name, args } => {
                    w.cancel.store(false, Ordering::Relaxed);
                    let cancel = w.cancel.clone();
                    match w.mcp.get_prompt(&name, &args, &cancel) {
                        Ok(text) => {
                            let shown = format!("/{name}{}", if args.is_empty() { String::new() } else { format!(" {args}") });
                            // The UI sends it on as an ordinary turn (which ends with its own TurnDone).
                            let _ = w.ui.send(UiEvent::McpPromptReady { shown, text });
                        }
                        Err(e) => {
                            let _ = w.ui.send(UiEvent::Error(e));
                            let _ = w.ui.send(UiEvent::TurnDone);
                        }
                    }
                }
                WorkerCmd::Patch(p) => {
                    w.cfg.apply_patch(&p);
                    Config::persist_patch(&p);
                    // Rebuild the system prompt when auto_commit toggles, so the
                    // model's git-instructions stay in sync with the live config.
                    if matches!(&p, ConfigPatch::AutoCommit(_)) && !w.messages.is_empty() {
                        w.messages[0] = Message::system(system_prompt(w.cfg.auto_commit));
                    }
                    // The balance is shown in this currency, and belongs to the
                    // account the credentials name: re-fetch it.
                    if matches!(
                        &p,
                        ConfigPatch::PriceCurrency(_)
                            | ConfigPatch::BaseUrl(_)
                            | ConfigPatch::ApiKey(_)
                            | ConfigPatch::ProviderKey { .. }
                            | ConfigPatch::ProviderAuth { .. }
                            | ConfigPatch::SignOut(_)
                    ) {
                        w.refresh_balance();
                    }
                    // Another server may serve the same model id with another window.
                    if matches!(&p, ConfigPatch::BaseUrl(_)) {
                        w.refresh_context_window();
                    }
                    // Provider presets also switch the model; reflect it.
                    if let ConfigPatch::Provider { model, provider, .. } = &p {
                        let _ = w.ui.send(UiEvent::ModelChanged(model.clone()));
                        // Cycling the provider row lands on providers with no
                        // key; say so now, not as a setup wizard next launch.
                        let note = if !w.cfg.has_credentials() {
                            format!("provider set to {provider} (not set up yet: /provider {provider} to sign in)")
                        } else {
                            format!("provider set to {provider}")
                        };
                        let _ = w.ui.send(UiEvent::Notice(note));
                        w.refresh_balance();
                        w.refresh_context_window(); // new provider/model window
                    }
                }
                WorkerCmd::User { text, images, typed } => {
                    w.cancel.store(false, Ordering::Relaxed);
                    w.ensure_oauth_fresh(); // a long session outlives the access token
                    // Supersede the previous turn's follow-up suggestion: stop its
                    // call early and mark it stale so a late result is dropped.
                    w.suggest_cancel.store(true, Ordering::Relaxed);
                    w.maybe_auto_compact();
                    // UserPromptSubmit hooks can veto the turn or add context.
                    let mut text = text;
                    let mut vetoed = false;
                    if w.hooks.has("UserPromptSubmit") {
                        match w.hooks.run("UserPromptSubmit", "", &serde_json::json!({ "prompt": text })) {
                            Outcome::Block(reason) => {
                                let _ = w.ui.send(UiEvent::Error(format!("prompt blocked by hook: {reason}")));
                                vetoed = true;
                            }
                            Outcome::Continue { context } if !context.trim().is_empty() => {
                                text.push_str("\n\n[hook context]\n");
                                text.push_str(context.trim());
                            }
                            _ => {}
                        }
                    }
                    if vetoed {
                        let _ = w.ui.send(UiEvent::TurnDone);
                        continue;
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        w.run_turn(text, images, typed);
                        w.run_stop_hooks();
                    }));
                    if result.is_err() {
                        let _ = w.ui.send(UiEvent::Error("internal error in agent turn".into()));
                        // A panic mid tool-round leaves tool_calls without
                        // results; the API would reject every later request.
                        repair_orphans(&mut w.messages);
                    }
                    w.save_session();
                    w.refresh_balance(); // reflect spend after the turn
                    if result.is_ok() && !w.cancel.load(Ordering::Relaxed) {
                        if let Some(rate) = w.generation.rate() {
                            let _ = w.ui.send(UiEvent::Notice(format!("{rate:.1} tok/s")));
                        }
                    }
                    let _ = w.ui.send(UiEvent::TurnDone);
                    // Fire a cheap follow-up suggestion call in the background.
                    // Uses its own cancel flag so a mid-turn Esc in the next turn
                    // doesn't interrupt it.
                    if !w.quiet {
                        let suggest_ui = w.ui.clone();
                        let suggest_http = w.http.clone();
                        let suggest_cfg = w.cfg.clone();
                        // Only send the last few turns (system prefix + ~6 most recent
                        // messages), plus the suggestion prompt as a user message.
                        let mut suggest_msgs: Vec<Message> = w.messages[..w.system_len].to_vec();
                        // Pick a window start no earlier than ~6 messages back, then
                        // skip any leading orphaned `tool` results: an OpenAI-style
                        // `tool` message with no preceding `tool_calls` inside the
                        // window makes the API reject the whole request with a 400
                        // (silently dropped below), so no suggestion would ever show.
                        let n = w.messages.len();
                        let mut start = n.saturating_sub(6).max(w.system_len);
                        while start < n && w.messages[start].role == "tool" {
                            start += 1;
                        }
                        suggest_msgs.extend(w.messages[start..].iter().cloned());
                        suggest_msgs.push(Message::user(
                            "Based on the conversation above, suggest ONE short follow-up \
                             prompt the user might want to ask next. Reply with ONLY the \
                             prompt text, no other words, no quotes, no explanation.",
                        ));
                        // Fresh flag for this turn's suggestion; storing it on the
                        // worker lets the next User command cancel it mid-flight.
                        let cancel_flag = Arc::new(AtomicBool::new(false));
                        w.suggest_cancel = cancel_flag.clone();
                        std::thread::spawn(move || {
                            if let Ok((text, usage)) = api::chat_plain(&suggest_http, &suggest_cfg, &suggest_msgs, &cancel_flag) {
                                // Billed even when the result arrives too late to show.
                                if let Some(u) = usage {
                                    let _ = suggest_ui.send(spend(&u));
                                }
                                // Drop a result that arrived after a new turn began.
                                if cancel_flag.load(Ordering::Relaxed) {
                                    return;
                                }
                                let text = text.trim().trim_matches('"').trim();
                                if !text.is_empty() && text.len() < 200 {
                                    let _ = suggest_ui.send(UiEvent::Suggestion(text.to_string()));
                                }
                            } else {
                                // silently skip on error
                            }
                        });
                    }
                }
            }
        }
    });

    Handles { join, cmd_tx, appr_tx, shared }
}
