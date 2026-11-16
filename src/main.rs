//! Haiku Code (`haikucode`) — a full-screen agentic coding CLI for Haiku, the
//! BeOS-inspired OS. A port of picoder (the Raspberry Pi Zero W agent).
//!
//! Self-contained binary (rustls TLS, no system deps beyond libroot). Talks to any
//! OpenAI-compatible chat API; defaults to DeepSeek. Codex/Claude-Code-style
//! agent loop with a ratatui terminal UI.

mod agent;
mod api;
mod askpass;
mod auth;
mod commands;
mod config;
mod diff;
mod hooks;
mod mcp;
mod money;
mod native;
mod oneshot;
mod policy;
mod sandbox;
mod sessions;
mod startup;
mod subcommands;
mod sysinfo;
mod tools;
mod ui;
mod update;

use config::Config;
use oneshot::{read_stdin_capped, run_oneshot, stdin_is_pipe, stdin_ready, OutputMode};
use startup::{run_tui, start_perm, start_session, status_lines};
use subcommands::{model_subcommand, sessions_subcommand};
use std::io::IsTerminal;

/// System prompt, built at runtime so haikucode honestly describes whatever host
/// it's on (a Haiku VM, or the dev Mac it's tested on). A host is treated as build-capable when
/// it has ≥4 cores or ≥2GB RAM (so the quad-core Jetson Nano, which can compile
/// its own Rust, isn't told to avoid compiles); smaller hosts get a "keep it
/// light" caution instead.
///
/// It names the working directory: outside a git repo nothing else did, and a
/// model left to guess invented `/home/<someone>/…/haikucode` and spent a whole
/// turn `cd`-ing into a directory that never existed.
pub fn system_prompt(auto_commit: bool) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    system_prompt_in(auto_commit, &cwd)
}

fn system_prompt_in(auto_commit: bool, cwd: &std::path::Path) -> String {
    let host = sysinfo::host_descriptor();
    let capable = sysinfo::cpu_cores() >= 4 || sysinfo::mem_total_mb().unwrap_or(512) >= 2048;
    let resource_rule = if !capable {
        "- Use bash for git, builds, tests. Keep commands fast and memory-light — this machine is \
tiny. Avoid heavy installs/compiles unless asked."
    } else {
        "- Use bash for git, builds, tests. This machine is reasonably capable (multi-core, GBs of \
RAM + swap), so builds and compiles are fine when useful."
    };
    // The model assumes GNU/Linux tools unless told otherwise.
    let shell_rule = if cfg!(target_os = "haiku") {
        HAIKU_SHELL_RULE
    } else if cfg!(target_os = "macos") {
        "\n- The shell is macOS (BSD userland): there is no GNU `timeout` (use the bash tool's \
`timeout` argument instead), `sed -i` needs an explicit '' suffix, `echo -n` prints a literal \
`-n` (use printf), and `readlink -f`/`date -d` differ from Linux."
    } else {
        ""
    };
    // Promising checkpoints in a directory with no repo sends the model to a
    // `git log` that isn't there, and lets it edit as if every change could be
    // undone.
    let git_rule = if auto_commit && tools::in_git_repo(cwd) {
        "\n- Every successful edit is auto-committed to git as a checkpoint. Use git history as a \
clue: `git log`, `git show`, and `git diff` tell you what changed and let you restore a previous \
state with `git revert`/`git checkout` if an edit was wrong. Don't commit manually unless asked."
    } else {
        ""
    };
    format!(
        "You are Haiku Code, a terminal coding agent running ON {host}. You help with \
software tasks using tools.

Rules:
- The working directory is {cwd}. Relative paths resolve there, and every bash command starts \
there (a `cd` does not carry over to the next call). Never guess other paths: list or search \
to find them.
- Use tools to inspect and change the real filesystem; never invent file contents.
- Prefer read_file/list_files/grep/glob before editing. Make minimal, correct edits with \
edit_file; use write_file for new files; use multi_edit to change several files at once.
{resource_rule}{shell_rule}{git_rule}
- Stop only processes you started, by PID. Never pkill/killall an app by name: the user's own \
copy (their browser, editor) may be running on this machine.
- When the task is complete, reply with a short plain-text summary and no tool call.
- Be concise.",
        cwd = cwd.display()
    )
}

/// Haiku is not Linux: without this the model reaches for `sudo`, `apt`, `which`
/// and /proc, and writes into the read-only /boot/system tree.
const HAIKU_SHELL_RULE: &str = "
- The OS is Haiku (BeOS lineage), not Linux. The shell is bash with GNU coreutils, but: there is \
no `sudo` (you already have full rights) and no `which` (use `command -v`); there is no /proc or \
/sys (use `ps`, `sysinfo`, `listdev`, `df`). HOME is /boot/home.
- /boot/system is packagefs and read-only. Install software with `pkgman install -y <pkg>` (search \
with `pkgman search`); put hand-built files in ~/config/non-packaged/{bin,lib,data} instead.
- Settings live in ~/config/settings, not ~/.config. `finddir B_USER_SETTINGS_DIRECTORY` etc. print \
the standard paths.
- Files carry BFS attributes (MIME type, icons): inspect with `listattr`/`catattr`, set with \
`addattr`. `open <file|url>` opens with the preferred app; GUI apps live in /boot/system/apps and \
can be scripted with `hey`.
- Native apps use the C++ Be API (`#include <Application.h>`, link `-lbe`); build with gcc/g++ \
or `cargo`.";

const HELP: &str = "haikucode — Haiku Code, an agentic coding CLI for Haiku, macOS and Linux
`hc` is a short alias for `haikucode` in the Haiku package.

usage:
  haikucode                 interactive full-screen TUI
  haikucode \"do a thing\"     one-shot task, then exit
  cmd | haikucode \"task\"     one-shot with stdin attached as context (or as the task)
  haikucode --continue      resume this directory's latest session (alias: -c)
  haikucode -r [id|name]    resume a saved session; with no id, pick one in the TUI (--resume)
  haikucode --fork-session  with -c / -r: carry on in a copy, leaving the original as it was
  haikucode sessions        list this directory's saved sessions (--json; delete <id|name>)
  haikucode --auto ...      auto-approve tool calls (deny rules still apply); one-shot
                          runs otherwise follow config \"permission\" and decline prompts
  haikucode --output FILE   one-shot: also write the final reply to FILE (alias: -o)
  haikucode --json ...      one-shot: print one JSON object {result, usage, cost, …}
  haikucode --stream-json   one-shot: print JSON-lines events as they happen
  haikucode --config        set up provider + API key
  haikucode model [id]      list models, or set the model
  haikucode update          install the latest release from GitHub (--force reinstalls it)
  haikucode --banner        print the launch banner (debug) and exit
  haikucode -h | --help     this help
  haikucode --version

in the TUI: @path attaches a file · !cmd runs a shell command · Tab autocompletes

config: ~/config/settings/haikucode/config.json  (key also via HAIKUCODE_API_KEY, or the
        configured provider's var, e.g. DEEPSEEK_API_KEY / OPENAI_API_KEY)
project: .haikucode/settings.json {permissions: {allow, deny}, hooks, auto_bump_version}
         .haikucode/commands/*.md  custom slash commands ($ARGUMENTS)
instructions: the first of HAIKUCODE.md / AGENTS.md / CLAUDE.md / GEMINI.md in ~/config/settings/haikucode
              and in each directory from the repo root down to the working directory

exit status: 0 ok · 1 the run reported an error · 2 bad usage";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Internal: invoked by sudo as the SUDO_ASKPASS helper. Talks to the running
    // haikucode over the given socket and prints the password; never starts the TUI.
    if let Some(pos) = args.iter().position(|a| a == "--askpass") {
        let sock = args.get(pos + 1).cloned().unwrap_or_default();
        let prompt = args.get(pos + 2).cloned().unwrap_or_else(|| "[sudo] password:".into());
        std::process::exit(askpass::run_helper(&sock, &prompt));
    }

    if args.iter().any(|a| a == "-h" || a == "--help") {
        // The paths in HELP are Haiku's; show this host's.
        let dir = config::config_dir_label();
        println!("{}", HELP.replace("~/config/settings/haikucode/", &dir).replace("~/config/settings/haikucode", dir.trim_end_matches('/')));
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("haikucode {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Haiku Code replaces picoder: carry its setup over on first run.
    let adopted = config::adopt_picoder();
    if let Some(a) = &adopted {
        eprintln!("haikucode: {a}");
    }
    if args.iter().any(|a| a == "--config" || a == "--setup") {
        if let Err(e) = config::run_setup() {
            eprintln!("setup failed: {e}");
        }
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--banner") {
        let ascii = ui::detect_ascii();
        let cfg = Config::load();
        let status = status_lines(&cfg, ascii);
        // Optional theme name after --banner, else the configured theme.
        // Only consume the next arg if it looks like a theme name (prevents
        // --banner from swallowing e.g. "fix" as a theme name).
        let theme = match args.get(pos + 1) {
            Some(n) if ui::is_theme_name(n) => {
                let name = n.to_string();
                args.remove(pos + 1);
                name
            }
            _ => cfg.theme,
        };
        print!("{}", ui::banner_ansi(ui::term_width(), ascii, &theme, &status));
        return;
    }

    let auto = args.iter().any(|a| a == "--auto");
    let cont = args.iter().any(|a| a == "--continue" || a == "-c");
    let fork_session = args.iter().any(|a| a == "--fork-session");
    // -r / --resume [id|name]: the next argument is the session unless it's a
    // flag; with none, the TUI opens the session picker.
    let mut resume: Option<Option<String>> = None;
    if let Some(pos) = args.iter().position(|a| a == "-r" || a == "--resume") {
        args.remove(pos);
        let key = (pos < args.len() && !args[pos].starts_with('-')).then(|| args.remove(pos));
        resume = Some(key);
    }
    let out_mode = if args.iter().any(|a| a == "--stream-json") {
        OutputMode::StreamJson
    } else if args.iter().any(|a| a == "--json" || a == "--output-format=json") {
        OutputMode::Json
    } else {
        OutputMode::Text
    };
    args.retain(|a| {
        !matches!(
            a.as_str(),
            "--auto" | "--continue" | "-c" | "--fork-session" | "--json" | "--stream-json" | "--output-format=json"
        )
    });

    let mut output: Option<String> = None;
    if let Some(pos) = args.iter().position(|a| a == "--output" || a == "-o") {
        if pos + 1 >= args.len() {
            eprintln!("--output needs a file path");
            std::process::exit(2);
        }
        output = Some(args.remove(pos + 1));
        args.remove(pos);
    }

    let mut cfg = Config::load();

    if !args.is_empty() && args[0] == "model" {
        model_subcommand(&cfg, args.get(1).cloned());
        return;
    }
    // `haikucode update [--force]` (a bare word, so "update the README" stays a task).
    if !args.is_empty() && args[0] == "update" && args[1..].iter().all(|a| a == "--force") {
        let http = api::agent_http();
        match update::run(&http, args.len() > 1, &|m| println!("{m}")) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("update failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if !args.is_empty() && args[0] == "sessions" {
        // `--json` was taken off `args` with the one-shot output flags.
        std::process::exit(sessions_subcommand(&args[1..], out_mode != OutputMode::Text));
    }

    // The saved session to start from, if any.
    let store = sessions::Store::here();
    let start = match &resume {
        Some(Some(key)) => match store.find(key) {
            Ok(hit) => Some(hit),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        _ if cont => store.latest(),
        _ => None,
    };
    if fork_session && start.is_none() {
        eprintln!("--fork-session needs a session to copy: add --continue or --resume <id|name>");
        std::process::exit(2);
    }
    let resumed = start.and_then(|(meta, path)| sessions::load(&path).map(|msgs| (meta, path, msgs)));

    if !cfg.has_credentials() {
        eprintln!("\x1b[33m{} isn't set up yet (no API key or sign-in).\x1b[0m", cfg.provider);
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            match config::run_setup() {
                Ok(c) => cfg = c,
                Err(e) => {
                    eprintln!("setup failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        if !cfg.has_credentials() {
            eprintln!("Run: haikucode --config");
            std::process::exit(1);
        }
    }

    // Piped stdin (`git diff | haikucode "review this"`) rides along as context;
    // with no task argument it *is* the task. A terminal stdin is left alone.
    let piped = if std::io::stdin().is_terminal() {
        String::new()
    } else if !args.is_empty() && !stdin_is_pipe() && !stdin_ready(300) {
        // A non-tty stdin that nobody is writing to (a supervisor's idle pipe,
        // a socket) must not wedge a scripted `haikucode "task"`: with a task
        // given, only read stdin when data or EOF is already there.
        String::new()
    } else {
        read_stdin_capped(1_000_000)
    };
    let mut task = args.join(" ");
    if !piped.trim().is_empty() {
        if task.trim().is_empty() {
            task = piped;
        } else {
            task = format!("{task}\n\n<stdin>\n{}\n</stdin>", piped.trim_end());
        }
    }

    let wants_session = cont || resume.is_some();
    if !task.trim().is_empty() {
        if resume == Some(None) {
            eprintln!("-r needs a session id or name in one-shot mode (`haikucode sessions` lists them; -c resumes the latest)");
            std::process::exit(2);
        }
        let (messages, session, _) = start_session(&store, resumed, fork_session, &cfg);
        // Only persist a one-shot turn when explicitly continuing a session,
        // so a stray `haikucode "x"` doesn't pile up sessions.
        let session = wants_session.then_some(session);
        let perm = start_perm(&cfg, auto);
        if !run_oneshot(cfg, messages, session, task, output, out_mode, perm) {
            std::process::exit(1);
        }
    } else {
        if output.is_some() {
            eprintln!("--output only applies to one-shot mode (haikucode \"task\" -o file)");
            std::process::exit(2);
        }
        if out_mode != OutputMode::Text {
            eprintln!("--json / --stream-json only apply to one-shot mode (haikucode \"task\" --json)");
            std::process::exit(2);
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("no task given and stdin is not a terminal — pass a task: haikucode \"do a thing\"");
            std::process::exit(2);
        }
        let replay = resumed.as_ref().map(|(_, _, msgs)| sessions::replay(msgs)).unwrap_or_default();
        let (messages, session, mut notes) = start_session(&store, resumed, fork_session, &cfg);
        notes.splice(0..0, adopted);
        run_tui(cfg, messages, notes, auto, session, replay, resume == Some(None));
    }
}

#[cfg(test)]
mod tests {
    /// A saved "yolo" (or the old "bypass"/"auto") starts the session in YOLO.
    #[test]
    fn saved_permission_names_start_the_right_mode() {
        let mut c = Config::default();
        for (name, want) in [("yolo", agent::PERM_AUTO), ("bypass", agent::PERM_AUTO), ("plan", agent::PERM_PLAN), ("ask", agent::PERM_ASK)] {
            c.permission = name.into();
            assert_eq!(start_perm(&c, false), want, "{name}");
        }
    }

    use super::*;
    use startup::{collect_context, format_history, parse_history, CONTEXT_FILE_CAP, CONTEXT_TOTAL_CAP};
    use std::fs;

    #[test]
    fn history_round_trips_multi_line_entries() {
        let entries = vec![
            "one line".to_string(),
            "two\nlines".to_string(),
            "a literal \\n and a backslash \\".to_string(),
            "tab\there".to_string(),
        ];
        let text = format_history(&entries);
        assert_eq!(text.lines().count(), entries.len() + 1, "one line per entry plus the marker");
        assert_eq!(parse_history(&text), entries);
        // The old format (no marker) is one entry per line, read verbatim.
        assert_eq!(parse_history("old \\n entry\nsecond"), vec!["old \\n entry", "second"]);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("haikucode_ctx_{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Outside a repo the prompt named no directory and promised git
    /// checkpoints; the model invented a path and chased it for 30 calls.
    #[test]
    fn the_prompt_names_the_directory_and_promises_checkpoints_only_in_a_repo() {
        let plain = scratch("norepo");
        let p = system_prompt_in(true, &plain);
        assert!(p.contains(&format!("The working directory is {}.", plain.display())), "{p}");
        assert!(!p.contains("auto-committed"), "{p}");

        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        if tools::in_git_repo(repo) {
            assert!(system_prompt_in(true, repo).contains("auto-committed"));
            assert!(!system_prompt_in(false, repo).contains("auto-committed"));
        }
    }

    /// On a Mac the prompt said "Linux", and the model ran GNU-only `timeout`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mac_is_described_as_macos_with_its_shell_quirks() {
        assert!(crate::sysinfo::os_name().starts_with("macOS"), "{}", crate::sysinfo::os_name());
        let p = system_prompt(false);
        assert!(p.contains("(macOS") && p.contains("no GNU `timeout`"), "{p}");
    }

    #[test]
    fn instruction_files_stack_from_global_to_the_working_directory() {
        let root = scratch("repo");
        let repo = root.join("repo");
        let deep = repo.join("sub/deep");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&deep).unwrap();
        fs::write(root.join("AGENTS.md"), "above the repo: never read").unwrap();
        fs::write(repo.join("AGENTS.md"), "repo root").unwrap();
        fs::write(repo.join("sub/CLAUDE.md"), "sub").unwrap();
        fs::write(deep.join("HAIKUCODE.md"), "deep").unwrap();
        fs::write(deep.join("AGENTS.md"), "shadowed by HAIKUCODE.md").unwrap();
        let global = root.join("global");
        fs::create_dir_all(&global).unwrap();
        fs::write(global.join("AGENTS.md"), "global").unwrap();

        let got = collect_context(&deep, &global);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        let global_agents = format!("{}AGENTS.md", config::config_dir_label());
        assert_eq!(names, [global_agents.as_str(), "../../AGENTS.md", "../CLAUDE.md", "HAIKUCODE.md"]);
        let bodies: Vec<&str> = got.iter().map(|(_, b)| b.as_str()).collect();
        assert_eq!(bodies, ["global", "repo root", "sub", "deep"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn outside_a_repo_only_the_working_directory_counts() {
        let root = scratch("plain");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(root.join("AGENTS.md"), "parent, not a repo").unwrap();
        fs::write(cwd.join("GEMINI.md"), "here").unwrap();
        let got = collect_context(&cwd, &root.join("no-global"));
        assert_eq!(got, vec![("GEMINI.md".to_string(), "here".to_string())]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_size_budget_goes_to_the_nearest_files_first() {
        let root = scratch("budget");
        let cwd = root.join("work");
        let global = root.join("global");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&global).unwrap();
        fs::write(cwd.join("AGENTS.md"), "x".repeat(CONTEXT_FILE_CAP + 500)).unwrap();
        fs::write(global.join("AGENTS.md"), "g".repeat(CONTEXT_TOTAL_CAP)).unwrap();
        let got = collect_context(&cwd, &global);
        let total: usize = got.iter().map(|(_, b)| b.chars().count()).sum();
        assert!(total <= CONTEXT_TOTAL_CAP, "{total}");
        let (near, far) = (&got[1].1, &got[0].1);
        assert!(near.chars().count() <= CONTEXT_FILE_CAP && near.starts_with("xxx"));
        assert!(far.chars().count() < CONTEXT_TOTAL_CAP, "global got only what was left");
        fs::remove_dir_all(&root).ok();
    }
}
