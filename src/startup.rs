//! Session start and the TUI's launch: resumed/forked/new conversations, the
//! startup context (system prompt, memory, instruction files, git), the
//! banner's status lines, the panic hook, `run_tui` and the input history.

use crate::agent::{UiEvent, WorkerCmd};
use crate::api::Message;
use crate::config::Config;
use crate::system_prompt;
use crate::{agent, api, askpass, commands, config, money, sessions, sysinfo, tools, ui};
use std::io::{Read, Write};

/// The conversation to start with and the session file it saves to: a
/// resumed session's own file, a copy of it with `--fork-session`, or a new
/// session. Returns startup notes for the UI too.
pub(super) fn start_session(
    store: &sessions::Store,
    resumed: Option<(sessions::Meta, std::path::PathBuf, Vec<Message>)>,
    fork: bool,
    cfg: &Config,
) -> (Vec<Message>, std::path::PathBuf, Vec<String>) {
    let Some((meta, path, mut msgs)) = resumed else {
        let (messages, notes) = build_context(cfg);
        return (messages, store.new_path(), notes);
    };
    // A session saved mid tool-round (crash, kill) has tool_calls with no
    // results; answer them so the first request on resume isn't a 400.
    agent::repair_orphans(&mut msgs);
    let mut notes = vec![format!("resumed session {}: {} ({} messages)", meta.id, meta.label(), meta.messages)];
    if !fork {
        return (msgs, path, notes);
    }
    match sessions::fork(store, &path, &msgs) {
        Ok(copy) => {
            notes.push(format!(
                "forked into {}; {} stays as it was",
                copy.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
                meta.id
            ));
            (msgs, copy, notes)
        }
        Err(e) => {
            eprintln!("could not fork session {}: {e}", meta.id);
            std::process::exit(1);
        }
    }
}

/// A fresh conversation's start: system prompt, memory, instruction files and
/// git state. Returns the messages and human-readable startup notes.
fn build_context(cfg: &Config) -> (Vec<Message>, Vec<String>) {
    let mut notes = Vec::new();
    let mut messages = vec![Message::system(system_prompt(cfg.auto_commit))];
    if let Ok(Some(mem)) = tools::load_memory() {
        messages.push(Message::system(format!(
            "Persistent memory (things you were told to remember):\n{mem}"
        )));
    }
    if let Some((msg, names)) = load_project_context() {
        messages.push(msg);
        notes.push(format!("loaded {}", names.join(", ")));
    }
    let skills = commands::discover_skills();
    if let Some(list) = commands::skills_prompt(&skills) {
        messages.push(Message::system(list));
        notes.push(format!("{} skill(s) available", skills.len()));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if let Some(git) = tools::git_context(&cwd) {
        messages.push(Message::system(format!(
            "Current git state of the working directory (use as a clue to recent work):\n{git}"
        )));
        notes.push("loaded git history".into());
    }
    (messages, notes)
}

/// The three SYSTEM STATUS lines for the launch banner: hardware, network, and
/// the active LLM (model · provider · context window).
pub(super) fn status_lines(cfg: &Config, ascii: bool) -> Vec<String> {
    let sep = if ascii { " - " } else { " · " };
    let window = cfg.effective_context_window();
    let ctx = if window >= 1_000_000 {
        let m = window as f64 / 1e6;
        // 1M, not 1.0M; 1.5M keeps the decimal.
        format!("{}M ctx", if m.fract() == 0.0 { format!("{m:.0}") } else { format!("{m:.1}") })
    } else if window >= 1000 {
        format!("{}K ctx", window / 1000)
    } else {
        format!("{} ctx", window)
    };
    let mut lines = vec![
        format!("HW   {}", sysinfo::hardware_line(ascii)),
        format!("NET  {}", sysinfo::network_line(ascii)),
        format!("LLM  {}{sep}{}{sep}{ctx}", cfg.model, cfg.provider),
    ];
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if let Some(git) = tools::git_status_line(&cwd, sep) {
        lines.push(format!("GIT  {git}"));
    }
    lines
}

/// Instruction files, in precedence order within one directory.
const CONTEXT_FILES: [&str; 4] = ["HAIKUCODE.md", "AGENTS.md", "CLAUDE.md", "GEMINI.md"];
/// Per-file and overall caps (chars) on loaded instructions; a Pi's model
/// can't spare much context for them.
pub(super) const CONTEXT_FILE_CAP: usize = 12_000;
pub(super) const CONTEXT_TOTAL_CAP: usize = 32_000;

/// Instruction files for this session as one system message, plus their
/// display names for the startup notes.
fn load_project_context() -> Option<(Message, Vec<String>)> {
    let cwd = std::env::current_dir().ok()?;
    let found = collect_context(&cwd, &config::config_dir());
    let names: Vec<String> = found.iter().map(|(n, _)| n.clone()).collect();
    let text = match found.as_slice() {
        [] => return None,
        [(name, body)] => format!("Project context from {name}:\n{body}"),
        _ => {
            let parts: Vec<String> = found.iter().map(|(n, b)| format!("--- {n} ---\n{b}")).collect();
            format!(
                "Project instructions, general to specific (later files take precedence):\n\n{}",
                parts.join("\n\n")
            )
        }
    };
    Some((Message::system(text), names))
}

/// (display name, contents) of the instruction files that apply in `cwd`,
/// general to specific: the global one in `global` (haikucode's config dir),
/// then one per directory from the repo root down to `cwd` (just `cwd`
/// outside a repo), each the first of `CONTEXT_FILES` present there. The
/// overall cap is spent nearest-first, so a long global file can't crowd out
/// the project's own instructions.
pub(super) fn collect_context(cwd: &std::path::Path, global: &std::path::Path) -> Vec<(String, String)> {
    let mut dirs: Vec<(std::path::PathBuf, String)> = vec![(cwd.to_path_buf(), String::new())];
    let below_repo_root =
        !cwd.join(".git").exists() && cwd.ancestors().skip(1).any(|d| d.join(".git").exists());
    if below_repo_root {
        for (up, d) in cwd.ancestors().enumerate().skip(1) {
            dirs.push((d.to_path_buf(), "../".repeat(up)));
            if d.join(".git").exists() {
                break;
            }
        }
    }
    if dirs.iter().all(|(d, _)| d != global) {
        dirs.push((global.to_path_buf(), config::config_dir_label()));
    }
    let mut found = Vec::new();
    let mut left = CONTEXT_TOTAL_CAP;
    for (dir, shown) in dirs {
        if left == 0 {
            break;
        }
        let Some((name, text)) = CONTEXT_FILES.iter().find_map(|n| {
            let text = read_capped(&dir.join(n))?;
            (!text.trim().is_empty()).then_some((*n, text))
        }) else {
            continue;
        };
        let body = api::truncate(text.trim(), CONTEXT_FILE_CAP.min(left));
        left = left.saturating_sub(body.chars().count());
        found.push((format!("{shown}{name}"), body));
    }
    found.reverse();
    found
}

/// A file's text, read no further than 1 MB; None if it can't be read.
fn read_capped(path: &std::path::Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut s = String::new();
    std::io::BufReader::new(f.take(1_000_000)).read_to_string(&mut s).ok()?;
    Some(s)
}

/// The permission mode a session starts in: `--auto` forces YOLO, else the
/// saved mode ("bypass"/"auto" are the old names for "yolo").
pub(super) fn start_perm(cfg: &Config, auto: bool) -> u8 {
    if auto {
        return agent::PERM_AUTO;
    }
    match cfg.permission.as_str() {
        "yolo" | "bypass" | "auto" => agent::PERM_AUTO,
        "plan" => agent::PERM_PLAN,
        _ => agent::PERM_ASK,
    }
}

/// Chains onto the hook `ratatui::init` installed (it restores the terminal).
/// Every panic is appended to crash.log: under the Deskbar launcher the window
/// closed with the message before anyone could read it. Only a panic on the UI
/// thread tears the screen down — the worker catches its own (agent.rs) and
/// carries on, and restoring the terminal for one used to drop the live UI out
/// of the alternate screen.
fn install_panic_hook(console: bool) {
    let tui_hook = std::panic::take_hook();
    let ui_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        let log = config::config_dir().join("crash.log");
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let entry = format!(
            "[unix {secs}] haikucode {} panicked on thread '{thread}': {info}\n",
            env!("CARGO_PKG_VERSION")
        );
        let logged = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .and_then(|mut f| f.write_all(entry.as_bytes()))
            .is_ok();
        if std::thread::current().id() == ui_thread {
            ui::restore_terminal(console);
            tui_hook(info);
            if logged {
                eprintln!("(logged to {})", log.display());
            }
        }
    }));
}

pub(super) fn run_tui(
    cfg: Config,
    messages: Vec<Message>,
    notes: Vec<String>,
    auto: bool,
    session: std::path::PathBuf,
    replay: Vec<(String, String)>,
    pick_session: bool,
) {
    let console = ui::detect_ascii();
    let status = status_lines(&cfg, console);
    let history = load_history();
    // Capture UI-relevant config before the worker takes ownership of `cfg`.
    let ui_cfg_model = cfg.model.clone();
    let ui_cfg_theme = cfg.theme.clone();
    let ctx_limit = cfg.effective_context_window();
    let price_in = cfg.price_in;
    let price_out = cfg.price_out;
    let price_cached_in = cfg.price_cached_in;
    let price_currency = money::Currency::parse(&cfg.price_currency);
    // A custom server without a price list has no known cost: the readout is
    // hidden rather than priced at a guess.
    let cost_known = cfg.effective_price_list().is_some();
    let settings = cfg.clone();
    let perm_start = start_perm(&cfg, auto);

    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiEvent>();
    // Wire up sudo askpass before the worker exists (it mutates process env).
    askpass::setup(ui_tx.clone());
    let h = agent::spawn(cfg, messages, perm_start, Some(session), ui_tx);

    let mut terminal = match ui::setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to start terminal: {e}");
            return;
        }
    };
    install_panic_hook(console);
    let mut app = ui::App::new(
        ui::UiConfig {
            model: ui_cfg_model,
            theme: ui_cfg_theme,
            ascii: console,
            ctx_limit,
            price_in,
            price_out,
            price_cached_in,
            price_currency,
            cost_known,
            perm: h.shared.perm.clone(),
            settings,
        },
        history,
    );
    app.banner(status);
    app.welcome();
    app.replay(replay);
    for n in notes {
        app.note(n);
    }
    if pick_session {
        app.open_session_picker(&h);
    }

    let result = ui::run(&mut terminal, &mut app, ui_rx, &h);
    ui::restore_terminal(console);
    if let Err(e) = result {
        eprintln!("ui error: {e}");
    }

    save_history(app.history());
    h.shared.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = h.cmd_tx.send(WorkerCmd::Quit);
    let _ = h.join.join();
}

/// First line of a history file whose entries are escaped (`\\` and `\n`),
/// so a multi-line prompt stays one line on disk. A file without it is the
/// older one-entry-per-line format and is read as-is.
const HISTORY_V2: &str = "#haikucode-history v2";

fn load_history() -> Vec<String> {
    std::fs::read_to_string(config::history_path())
        .map(|s| parse_history(&s))
        .unwrap_or_default()
}

pub(super) fn parse_history(s: &str) -> Vec<String> {
    let mut lines = s.lines();
    if s.starts_with(HISTORY_V2) {
        lines.next();
        lines.map(unescape_history).collect()
    } else {
        lines.map(String::from).collect()
    }
}

fn save_history(history: &[String]) {
    let _ = std::fs::create_dir_all(config::config_dir());
    let _ = std::fs::write(config::history_path(), format_history(history));
}

pub(super) fn format_history(history: &[String]) -> String {
    let start = history.len().saturating_sub(1000);
    let mut out = String::from(HISTORY_V2);
    for entry in &history[start..] {
        out.push('\n');
        out.push_str(&entry.replace('\\', "\\\\").replace('\n', "\\n"));
    }
    out
}

fn unescape_history(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}
