//! UI types: glyphs, line kinds, modes, pickers, and config snapshots.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;
use std::time::Duration;

pub const SPIN_U: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPIN_A: [&str; 4] = ["|", "/", "-", "\\"];
pub const MAX_TRANSCRIPT: usize = 4000;

#[derive(Clone, Copy, PartialEq)]
pub enum CursorKind {
    Caret,   // hardware caret
    Reverse, // reverse-video over the char (Claude-style)
    Block,   // a solid ▒/# block (Apple ][ / DOS)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    ToolResult,
    ToolErr,
    DiffAdd,
    DiffDel,
    DiffCtx,
    Notice,
    ErrorK,
    /// A deliberate blank separator line between transcript blocks.
    Blank,
    #[allow(dead_code)] Code,
    #[allow(dead_code)] Heading,
    #[allow(dead_code)] Banner,
    #[allow(dead_code)] BannerDim,
    /// Placeholder for the launch banner; expanded at the current width on
    /// every render so it reflows when the terminal is resized.
    BannerBlock,
    /// Tool-work progress note; styled like a notice, hidden in brief view.
    Progress,
}

/// Glyphs vary by terminal: the Pi's framebuffer console (TERM=linux) lacks
/// the fancy Unicode used over SSH, so we fall back to ASCII there.
#[derive(Clone, Copy)]
pub struct Glyphs {
    pub user: &'static str,
    pub assistant: &'static str,
    pub tool: &'static str,
    pub result: &'static str,
    pub error: &'static str,
    pub prompt: &'static str,
    pub rounded: bool,
}

pub const GLYPHS_U: Glyphs = Glyphs {
    user: "❯ ",
    assistant: "● ",
    // ⏺ and ⎿ are missing from DejaVu Sans Mono (the font that draws box
    // characters correctly in Haiku Terminal) and show as tofu; ◉ and └ aren't.
    tool: "◉ ",
    result: "└ ",
    error: "✗ ",
    prompt: "❯ ",
    rounded: true,
};
pub const GLYPHS_A: Glyphs = Glyphs {
    user: "> ",
    assistant: "* ",
    tool: "* ",
    result: "> ",
    error: "x ",
    prompt: "> ",
    rounded: false,
};

/// Choose a glyph set. ASCII for dumb terminals, or when forced via
/// HAIKUCODE_ASCII=1; Unicode otherwise (or forced via HAIKUCODE_UNICODE=1).
/// TERM=linux stays Unicode: the framebuffer console renders the glyphs we
/// use at single-cell width (set HAIKUCODE_ASCII=1 on consoles whose font
/// doesn't cover them).
pub fn detect_ascii() -> bool {
    if std::env::var("HAIKUCODE_UNICODE").is_ok() {
        return false;
    }
    if std::env::var("HAIKUCODE_ASCII").is_ok() {
        return true;
    }
    match std::env::var("TERM").as_deref() {
        Ok("dumb") | Ok("vt100") | Ok("") | Err(_) => true,
        _ => false,
    }
}

/// True when the terminal likely has only 16 colors (Linux console without truecolor).
pub fn is_16color_terminal() -> bool {
    // COLORTERM=truecolor or COLORTERM=24bit means true color support.
    matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor") | Ok("24bit"))
        == false
        && matches!(std::env::var("TERM").as_deref(), Ok("linux") | Ok("dumb") | Ok("vt100"))
}

/// Slash commands with the one-line description shown in the `/` palette.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/model", "pick a model from the provider's list"),
    ("/new", "start a new session (the current one stays saved)"),
    ("/config", "settings: provider, model, key, reasoning, …"),
    ("/compact", "summarize older turns to free context"),
    ("/effort", "reasoning effort: auto, off, low, medium, high, xhigh, max"),
    ("/reset", "clear conversation context"),
    ("/yolo", "toggle YOLO mode: approve everything (also /auto)"),
    ("/mcp", "MCP servers: state, tools, prompts · /mcp approve|deny|login|logout <server>"),
    ("/memory", "show persistent memory"),
    ("/theme", "open the theme picker"),
    ("/init", "summarize this project into HAIKUCODE.md"),
    ("/diff", "show what changed since this session started"),
    ("/undo", "revert the last haikucode checkpoint commit"),
    ("/rewind", "pick an earlier prompt: undo haikucode's edits since then and resume from there"),
    ("/resume", "switch to another saved session in this directory"),
    ("/rename", "name this session: /rename <name>"),
    ("/fork", "carry on in a copy of this conversation (the original stays)"),
    ("/export", "save the conversation as Markdown: /export [--all] [file]"),
    ("/sessions", "list saved sessions; /sessions delete <id|name>"),
    ("/review", "review the working tree / branch changes"),
    ("/status", "model, cwd, git, permissions, context, cost"),
    ("/cost", "session token usage and cost"),
    ("/permissions", "list / allow / deny permission rules"),
    ("/hooks", "list configured lifecycle hooks"),
    ("/commands", "reload and list custom slash commands"),
    ("/thinking", "toggle full thinking + tool output vs brief summaries (Ctrl+T)"),
    ("/clear", "clear the screen transcript"),
    ("/update", "install the latest Haiku Code release from GitHub"),
    ("/help", "show help"),
    ("/exit", "quit Haiku Code"),
];

/// Most suggestions shown under the composer for a `/` prefix.
pub const MAX_SUGGEST: usize = 8;
/// Window within which a second Ctrl+C/Ctrl+D exits (Claude Code style).
pub const DOUBLE_PRESS_TIMEOUT: Duration = Duration::from_secs(2);

/// Each slash command's options, as the full text they complete to. Shown in
/// the palette once a command and a space are typed (and listed by commands
/// run bare); an entry ending in a space still needs an argument, so picking
/// it fills the line instead of running it.
pub const SLASH_OPTIONS: &[(&str, &str)] = &[
    ("/effort auto", "send nothing: the model's own default"),
    ("/effort off", "no reasoning (or the provider's lowest)"),
    ("/effort low", "quick, light reasoning"),
    ("/effort medium", "balanced"),
    ("/effort high", "thorough"),
    ("/effort xhigh", "deeper; high where the provider stops there"),
    ("/effort max", "deepest; high/xhigh where the provider stops there"),
    ("/permissions list", "show the allow / deny rules in force"),
    ("/permissions allow ", "<rule> never ask, e.g. bash(cargo test:*)"),
    ("/permissions deny ", "<rule> always refuse, e.g. bash(rm:*)"),
    ("/permissions remove ", "<rule> drop a rule from the settings files"),
    ("/permissions reload", "re-read the settings files"),
    ("/yolo on", "approve everything"),
    ("/yolo off", "back to asking before edits"),
    ("/thinking full", "show reasoning, tool output and diffs"),
    ("/thinking brief", "one-line summaries of each stretch of work"),
    ("/mcp approve ", "<server> trust a project's MCP server"),
    ("/mcp deny ", "<server> refuse a project's MCP server"),
    ("/mcp login ", "<server> sign in to a server that needs OAuth"),
    ("/mcp logout ", "<server> forget a server's login"),
    ("/sessions delete ", "<id|name> delete a saved session"),
    ("/export --all", "include haikucode's system/context messages"),
    ("/export ", "<file> write to this path (default: a file in this directory)"),
    ("/rename ", "<name> name this session"),
    ("/resume ", "<id|name> switch straight to that session"),
    ("/model ", "<model id> switch without the picker"),
    ("/review ", "<base> branch to review against (default main/master)"),
    ("/compact ", "<focus> what the summary should keep"),
    ("/update --force", "reinstall the latest release even if it's this version"),
];

/// `SLASH_OPTIONS` for one command (`/effort`), for listing when it runs bare.
pub fn options_of(cmd: &str) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    SLASH_OPTIONS
        .iter()
        .copied()
        .filter(move |(o, _)| o.strip_prefix(cmd).is_some_and(|r| r.starts_with(' ')))
}

/// Rows of the `/config` panel, in display order.
pub const SETTING_LABELS: &[&str] = &[
    "provider",
    "base url",
    "model",
    "api key",
    "auth mode",
    "reasoning",
    "permissions",
    "auto-commit",
    "theme",
    "context window",
    "max tool calls",
    "price currency",
];

/// The `/config` rows shown, as indexes into `SETTING_LABELS` (the panel's
/// cursor is a position in this list). Row 4, auth mode, is hidden along with
/// `/login`: subscription tokens only work through the vendors' own apps, and
/// haikucode has no native adapter for them.
pub const SETTING_ROWS: &[usize] = &[0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11];

#[derive(Clone, Copy)]
pub enum BannerColor {
    /// A fixed color (no palette reactivity).
    #[allow(dead_code)] Fixed(Color),
    /// Rainbow art row: uses `palette.mono_banner` when set, else
    /// the Apple rainbow at the given index; `true` for a half-block row,
    /// whose lower halves take the next index's color.
    Rainbow(usize, bool),
    /// Uses `palette.accent`.
    Accent,
}

pub struct TLine {
    pub kind: Kind,
    pub text: String,
    /// First line of a block — shows the glyph; later lines align under it.
    pub lead: bool,
    /// Optional per-line fg override that resolves against the current palette.
    /// Banner colors are deferred so theme switches update the logo in real time.
    pub color: Option<BannerColor>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Mode {
    Idle,
    Busy,
    Approval(String),
    /// Interactive theme picker (`/theme` with no argument). `cursor` is the
    /// highlighted theme index; `prev` is the theme to restore if cancelled.
    ThemeSelect { cursor: usize, prev: String },
    /// Masked sudo password entry, requested by the askpass helper. `prompt` is
    /// the text sudo asked with (e.g. "[sudo] password for user:").
    Password { prompt: String },
    /// The ask_user tool: a visible one-line answer to the agent's question.
    Question { prompt: String },
    /// The `/config` panel. `edit` holds the text buffer while a free-text
    /// row (base url, model, api key, context window) is being edited.
    Settings { cursor: usize, edit: Option<String> },
    /// A generic selection list (state lives in `App::picker`): cursor +
    /// type-to-filter, used by `/model` for the fetched model list.
    Select,
}

/// What committing a `Select` picker entry does.
#[derive(Clone, Copy)]
pub enum PickAction {
    /// Switch model to the chosen id.
    #[allow(dead_code)] Model,
    /// Rewind to before the chosen prompt ("N. preview").
    Rewind,
    /// Switch to the chosen session ("id  label · …").
    Resume,
}

/// State for `Mode::Select`: a filterable, scrollable list of choices.
pub struct Picker {
    pub title: String,
    pub items: Vec<String>,
    /// Item highlighted as the current value (shown with a marker).
    pub current: Option<usize>,
    pub filter: String,
    /// Cursor within the *filtered* view.
    pub cursor: usize,
    pub scroll: usize,
    pub action: PickAction,
}

/// Visible rows of a Select picker before it scrolls.
pub const PICKER_VISIBLE: usize = 8;

impl Picker {
    /// Indices of items matching the filter (case-insensitive substring).
    pub fn filtered(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let f = self.filter.to_lowercase();
        (0..self.items.len())
            .filter(|&i| self.items[i].to_lowercase().contains(&f))
            .collect()
    }

    /// Keep the cursor inside the filtered list and the scroll window.
    pub fn clamp(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self.cursor.min(filtered_len - 1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + PICKER_VISIBLE {
            self.scroll = self.cursor + 1 - PICKER_VISIBLE;
        }
    }

    /// Move the highlight `delta` rows through the *filtered* view, clamping at
    /// both ends and scrolling the window along. Shared by the arrow keys and
    /// the Ctrl+j/k aliases in the picker key handler.
    pub fn step(&mut self, delta: isize) {
        let len = self.filtered().len();
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        self.clamp(len);
    }
}

/// Everything the UI needs from config, captured before the worker consumes it.
pub struct UiConfig {
    pub model: String,
    pub theme: String,
    pub ascii: bool,
    pub ctx_limit: u32,
    pub price_in: f64,
    pub price_out: f64,
    /// Unit `price_in`/`price_out` are quoted in (ISO 4217).
    pub price_currency: crate::money::Currency,
    pub perm: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Snapshot for the `/config` panel; kept in sync as patches are sent.
    pub settings: crate::config::Config,
}

/// Apply Caps Lock to a typed character. Under the Kitty keyboard protocol
/// (pushed in setup_terminal) the terminal reports the BASE key — 'a' even
/// with Caps Lock on — plus a CAPS_LOCK state flag, leaving case to the app.
/// Caps alone uppercases letters; Caps+Shift lowercases them, like a real
/// keyboard. Legacy terminals pre-shift the char and set no state flag, so
/// this is a no-op there.
pub fn caps_char(key: &KeyEvent, c: char) -> char {
    use ratatui::crossterm::event::KeyEventState;
    if !key.state.contains(KeyEventState::CAPS_LOCK) || !c.is_alphabetic() {
        return c;
    }
    let mapped: Vec<char> = if key.modifiers.contains(ratatui::crossterm::event::KeyModifiers::SHIFT) {
        c.to_lowercase().collect()
    } else {
        c.to_uppercase().collect()
    };
    if mapped.len() == 1 {
        mapped[0]
    } else {
        c
    }
}

/// Returns true if the key is Ctrl+C or Ctrl+D.
pub fn ctrl_c_or_d(key: &KeyEvent) -> bool {
    use ratatui::crossterm::event::KeyModifiers;
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
}

/// How far a key moves the `Picker` highlight, or `None` if it isn't a
/// movement key and should be handled as filter input instead.
///
/// Plain arrows always work; `Ctrl+j`/`Ctrl+k` are the vi-style aliases, and
/// the bare `j`/`k` characters deliberately stay out of it because the list is
/// type-to-filter. PageUp/PageDown jump a whole visible window.
pub fn picker_step_for_key(key: &KeyEvent) -> Option<isize> {
    use ratatui::crossterm::event::KeyModifiers;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::Char('k') if ctrl => Some(-1),
        KeyCode::Char('j') if ctrl => Some(1),
        KeyCode::PageUp => Some(-(PICKER_VISIBLE as isize)),
        KeyCode::PageDown => Some(PICKER_VISIBLE as isize),
        _ => None,
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

    fn picker(n: usize) -> Picker {
        Picker {
            title: String::new(),
            items: (0..n).map(|i| format!("m{i}")).collect(),
            current: None,
            filter: String::new(),
            cursor: 0,
            scroll: 0,
            action: PickAction::Model,
        }
    }

    #[test]
    fn plain_arrows_move_the_cursor() {
        let mut p = picker(5);
        p.step(picker_step_for_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap());
        assert_eq!(p.cursor, 1);
        p.step(picker_step_for_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).unwrap());
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn plain_arrows_clamp_at_both_ends() {
        let mut p = picker(3);
        for _ in 0..5 {
            p.step(picker_step_for_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap());
        }
        assert_eq!(p.cursor, 2);
        for _ in 0..5 {
            p.step(picker_step_for_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).unwrap());
        }
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn ctrl_j_k_alias_the_arrows_but_bare_j_k_are_filter_input() {
        assert_eq!(
            picker_step_for_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(1)
        );
        assert_eq!(
            picker_step_for_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(-1)
        );
        assert_eq!(
            picker_step_for_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            picker_step_for_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn movement_respects_the_filter_and_scrolls() {
        let mut p = picker(20);
        p.filter = "m1".into(); // m1, m10..m19 => 11 rows
        assert_eq!(p.filtered().len(), 11);
        p.step(8);
        assert_eq!(p.cursor, 8);
        assert_eq!(p.scroll, 1, "cursor past the window scrolls the view");
        p.step(100);
        assert_eq!(p.cursor, 10);
        assert_eq!(p.scroll + PICKER_VISIBLE, 11);
        p.filter = "nope".into();
        p.step(1);
        assert_eq!((p.cursor, p.scroll), (0, 0), "empty filter list is safe");
    }
}
