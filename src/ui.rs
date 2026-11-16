//! Full-screen ratatui interface: a scrolling transcript, a multi-line
//! composer, and a status bar. Runs on the UI thread; the agent runs on a
//! worker thread and feeds this UI through a channel.

mod activity;
mod banner;
#[cfg_attr(not(target_os = "haiku"), allow(dead_code))]
mod beclip;
mod brief;
mod commands;
mod composer;
mod draw;
mod events;
mod helpers;
mod keys;
mod markdown;
mod mouse;
mod palette;
mod provider;
mod render;
mod settings;
mod statusline;
mod types;

pub use banner::banner_ansi;
pub use helpers::{expand_attachments, extract_images, term_width};
pub use palette::{is_theme_name, THEMES};
pub use types::{detect_ascii, UiConfig};

use crate::agent::{ApprovalResponse, Handles, UiEvent, WorkerCmd};
use crate::config::{Config, ConfigPatch, PROVIDERS};
use crate::money;
use activity::Activity;
use banner::{BRole, banner_lines};
use helpers::{
    bar, clipboard_image, complete_path, humanize, longest_common_prefix, pad1,
    pasted_image_attachments,
    perm_name, render_message, render_tline, setting_max_tool_calls,
};
use palette::{Palette, palette_by_name};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use types::{
    caps_char, ctrl_c_or_d, is_16color_terminal, picker_step_for_key, BannerColor, CursorKind,
    Glyphs, Kind, Mode, PickAction, Picker, ProviderDraft, ProviderStep, TLine, DOUBLE_PRESS_TIMEOUT, GLYPHS_A, GLYPHS_U,
    MAX_SUGGEST, MAX_TRANSCRIPT, PICKER_VISIBLE, SETTING_LABELS, SETTING_ROWS, SLASH_COMMANDS, SPIN_A, SPIN_U,
};

/// The current working directory as a display string, with `$HOME` collapsed to
/// `~`. Falls back to `haikucode` if the cwd can't be read.
fn cwd_label() -> String {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return "haikucode".to_string(),
    };
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Ok(rest) = cwd.strip_prefix(&home) {
                return if rest.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rest.display())
                };
            }
        }
    }
    cwd.display().to_string()
}

/// Result of `App::layout_input`: the composer text split into display rows
/// plus where the cursor lands (row, cell column, and chars into that row).
struct ComposerLayout {
    rows: Vec<String>,
    cur_row: usize,
    cur_col: usize,
    cur_idx: usize,
    /// Char index (into the whole input) where each row starts. A row ended by
    /// a newline doesn't hold the `\n`, so the next start skips past it.
    starts: Vec<usize>,
}

/// Cells a tab takes in the composer (tabs are kept verbatim in the buffer,
/// e.g. a pasted Makefile, but drawn as spaces).
const TAB_W: usize = 4;

/// A char that belongs to a word for line-breaking purposes: letters, digits
/// and the punctuation that glues identifiers, paths and flags together
/// (`src/main.rs`, `--flag`, `foo_bar`). Punctuation that ends a clause or
/// starts a quote (`+ = , ( "`) is not — a break there splits nothing.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '~' | '@')
}

/// True when a line break between these two chars would cut a word in half.
fn splits_word_between(before: Option<char>, after: Option<char>) -> bool {
    matches!((before, after), (Some(b), Some(a)) if word_char(b) && word_char(a))
}

/// Lines of a prompt echoed into the transcript before the rest is elided.
const ECHO_LINES: usize = 24;

/// A prompt as echoed into the transcript: whole up to ECHO_LINES lines, else
/// its first lines and a count of the rest.
fn echo_excerpt(text: &str) -> String {
    let n = text.lines().count();
    if n <= ECHO_LINES {
        return text.to_string();
    }
    let keep = ECHO_LINES - 4;
    let head: Vec<&str> = text.lines().take(keep).collect();
    format!("{}\n… (+{} more lines)", head.join("\n"), n - keep)
}

pub struct App {
    transcript: Vec<TLine>,
    /// SYSTEM rows for the launch banner (see `Kind::BannerBlock`).
    banner_status: Vec<String>,
    live: String,
    live_reasoning: String,
    /// Current activity only; never retained as a status history.
    current_activity: Option<String>,
    /// Immediate acknowledgement until this turn produces its first content.
    awaiting_response: bool,
    input: String,
    cursor: usize, // char index
    /// Terminal size at the last render, so key handlers can lay the composer
    /// out exactly as drawn (Up/Down move between its wrapped rows).
    term_w: u16,
    term_h: u16,
    history: Vec<String>,
    hist_idx: usize,
    pending: String,
    /// Messages typed while the agent was busy, sent in order as turns finish.
    queued: Vec<String>,
    /// Set by Esc: the interrupted turn's TurnDone must not fire the queue —
    /// the user stopped the agent, so queued follow-ups wait for their next
    /// message before they resume sending.
    queue_paused: bool,
    /// Suggested next prompt from the agent, shown as a dimmed hint.
    suggestion: Option<String>,
    /// Highlighted row of the `/` command palette (clamped at use).
    suggest_idx: usize,
    /// State for Mode::Select (the /model list, etc.).
    picker: Option<Picker>,
    mode: Mode,
    /// Buffer + reply channel for an in-flight masked sudo password prompt. Held
    /// in memory only; never pushed to the transcript, history, or session.
    pw_input: String,
    pw_reply: Option<std::sync::mpsc::Sender<Option<String>>>,
    /// Buffer + reply channel for an in-flight ask_user question.
    q_input: String,
    /// The `/provider` wizard's answers so far, and the line being typed.
    wizard: Option<ProviderDraft>,
    wiz_input: String,
    wiz_cursor: usize,
    q_reply: Option<std::sync::mpsc::Sender<Option<String>>>,
    follow: bool,
    /// True while the view is pinned above the live end: shows the scroll
    /// affordance in the status bar.
    scrolled_up: bool,
    /// Output arrived while the view was pinned, so the status bar hint can say
    /// "↓ new" instead of the neutral "↓ end".
    new_below: bool,
    scroll: usize,
    max_top: usize,
    view_h: usize,
    /// A mouse drag: what it is over and its two corners, in screen cells,
    /// clamped to that box. Released, it is copied and cleared.
    select: Option<Select>,
    /// Where the output box was last drawn, so a drag that runs past it can't
    /// grab the composer or the status bar.
    output_area: Rect,
    /// The same for the composer's text, and zero when no composer is on
    /// screen (an approval prompt, say).
    composer_area: Rect,
    /// The layout row drawn on the composer's first screen row: the text
    /// scrolls to the caret once it outgrows the box, so a cell only maps back
    /// to a char if the offset it was drawn at is known.
    composer_first: usize,
    /// A short note the status bar shows for a moment ("copied 214 chars"), for
    /// actions that shouldn't write a line into the transcript.
    flash: Option<(String, Instant)>,
    /// The cells of the frame drawn last while a selection is up, so releasing
    /// it can read them back. Kept only while selecting: after `draw` returns,
    /// ratatui's own buffers have already been swapped, so the frame that was
    /// drawn is the one it hands back, not the one `current_buffer_mut` holds.
    screen: Option<ratatui::buffer::Buffer>,
    spinner: usize,
    spin_counter: usize,
    /// Clock time when the agent last entered Busy; used to show an elapsed timer.
    busy_since: Option<Instant>,
    /// The tool the agent is running (from ToolStart until its result), for
    /// the prompt's activity marker.
    tool_running: Option<String>,
    model_info: String,
    last_models: Vec<String>,
    /// The current session ("id — label"), for /status.
    session_label: String,
    should_quit: bool,
    esc_deadline: Option<Instant>,
    /// Time of last Ctrl+C with empty input; used for double-press-to-quit.
    last_ctrl_c: Option<Instant>,
    /// Text cut by Ctrl+K/U/W, put back by Ctrl+Y — readline's kill ring
    /// (a single slot: the last kill).
    kill_ring: String,
    /// Cached slash-command usage counts (command → times used). Rebuilt when
    /// history grows so we don't scan full history per keystroke.
    cmd_uses: HashMap<String, usize>,
    /// Set by Ctrl+L; makes the event loop clear the backend before the next
    /// draw, forcing a full repaint (recovers from any screen desync).
    force_clear: bool,
    /// Terminal draws every glyph in one cell (ASCII mode, or the Linux
    /// framebuffer console) — wide chars must be replaced before rendering.
    single_width: bool,
    glyphs: Glyphs,
    ascii: bool,
    palette: Palette,
    /// Cached rendered lines for the static (non-`live`) transcript. Rebuilding
    /// this for every frame meant re-wrapping thousands of lines per streamed
    /// token — the dominant cost on a single-core Pi. We rebuild only when the
    /// transcript content (`tver`) or terminal `width` actually changes, and
    /// each frame clones just the visible window.
    disp_cache: Vec<Line<'static>>,
    disp_cache_width: usize,
    disp_cache_tver: u64,
    /// (show_thinking, awaiting approval) the cache was built for.
    disp_cache_view: (bool, bool),
    /// Full reasoning/tool output/diffs (true) or thinking summaries (false).
    show_thinking: bool,
    /// Bumped on every transcript-content or palette change to invalidate
    /// `disp_cache`. (`single_width`/`glyphs` are fixed at construction.)
    tver: u64,
    // Claude-style status data:
    perm: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ctx_limit: u32,
    price_in: f64,
    price_out: f64,
    /// Price per 1M cached input tokens for the active provider's list.
    price_cached_in: Option<f64>,
    /// Unit the prices above are quoted in; the account balance carries its own.
    price_currency: money::Currency,
    /// False when the active server's price is unknown (a custom provider
    /// without a price list): the cost readout is hidden.
    cost_known: bool,
    last_prompt_tokens: u32,
    sess_prompt: u64,
    sess_completion: u64,
    /// Session input tokens served from / written to the provider's cache.
    sess_cached: u64,
    sess_written: u64,
    /// Cost already priced: when the price list changes (a provider or model
    /// switch), the tokens so far are priced at the old list and banked, so
    /// the readout doesn't re-price past spend at the new one.
    banked_cost: f64,
    /// Token totals at the last bank, so only spend since then is priced live.
    banked_at: money::Spent,
    balance: Option<money::Balance>,
    /// True once the "balance and prices are in different currencies" notice
    /// has been shown, so the per-turn balance refresh doesn't repeat it.
    settings: Config,
    /// Current working directory shown in the output box title, with $HOME
    /// collapsed to `~`. Computed once at startup (haikucode never chdirs).
    cwd_label: String,
    /// The "don't ask again" rule offered with the current approval prompt.
    appr_rule: Option<String>,
    /// HEAD when this session started, so /diff can show everything since.
    session_rev: Option<String>,
    /// Custom slash commands from .haikucode/commands etc. (see commands.rs).
    custom_cmds: Vec<crate::commands::CustomCommand>,
    /// The same, as palette rows (name/description leaked to 'static — they
    /// live as long as the process, and reloads are rare and tiny).
    custom_palette: Vec<(&'static str, &'static str)>,
    /// MCP prompts (`/mcp__server__prompt`) as palette rows, from the worker.
    mcp_palette: Vec<(&'static str, &'static str)>,
    /// Cached `(branch, dirty)` for the title's git indicator. `None` outside a
    /// repo. Refreshed on a throttle (see `git_checked_at`) since the agent's
    /// edits/auto-commits change the dirty state mid-session.
    git_head: Option<(String, bool)>,
    git_checked_at: Option<Instant>,
    /// The first status row's segments, in order (`/statusline`).
    statusline: Vec<String>,
    /// Minute last drawn by the `time` segment, so the clock redraws when it
    /// turns even while nothing else changes; checked at most once a second.
    clock_min: i32,
    clock_checked: Option<Instant>,
}

/// Lines moved per arrow key / wheel notch. PgUp/PgDn move a full viewport.
const SCROLL_STEP: isize = 3;

/// How long a status-bar flash note ("copied 214 chars") stays up.
const FLASH_TIME: Duration = Duration::from_millis(2200);

/// Which pane of the UI a drag is over: the two select differently, because
/// the transcript is copied from the cells that were drawn and the composer
/// from the input text itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectKind {
    /// The transcript.
    Output,
    /// The composer's text.
    Composer,
}

/// A drag in progress: what it is over, how far it may reach, and its two
/// corners — where the button went down and where it is now.
#[derive(Clone, Copy, Debug)]
struct Select {
    kind: SelectKind,
    /// The rect the drag is confined to (the transcript's inner box, or the
    /// composer).
    area: Rect,
    /// Leftmost column a cell may be in: the composer's text starts past its
    /// prompt, and the prompt is not selectable.
    left: u16,
    anchor: (u16, u16),
    cursor: (u16, u16),
}

impl Select {
    /// The corners in reading order, top-left first.
    fn corners(&self) -> ((u16, u16), (u16, u16)) {
        let (a, b) = (self.anchor, self.cursor);
        if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) }
    }
}

/// What a mouse event asks the event loop to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    /// Nothing changed: no repaint.
    None,
    /// Repaint (the wheel scrolled, or the selection moved).
    Redraw,
    /// Copy this range of cells (top-left first) out of the frame just drawn,
    /// and repaint. The selection itself is already gone.
    Copy(SelectKind, (u16, u16), (u16, u16)),
}

/// Whether a blank separator line should precede an entry of kind `next` given
/// the previous entry's kind. The transcript is made of distinct blocks — a
/// turn's prose, its tool activity, and the next prompt — and without a gap they
/// butt together into one wall of text. Only genuinely new blocks get a gap; a
/// notice or an error that follows a tool stays attached to it.
fn needs_sep(prev: Kind, next: Kind) -> bool {
    use Kind::*;
    let tool_like = |k: Kind| matches!(k, Tool | ToolResult | ToolErr | DiffAdd | DiffDel | DiffCtx);
    match next {
        // A new prompt opens a new turn: give it room above (but not above the
        // launch banner, which already pads itself).
        User => !matches!(prev, User | Banner | BannerDim),
        // Separate the prose that precedes tool activity from the tools.
        Tool => matches!(prev, Assistant | ToolResult | ToolErr | DiffAdd | DiffDel | DiffCtx),
        // Separate the assistant's next words (prose or reasoning) from the tool
        // activity that has just scrolled past, and set the answer apart from the
        // reasoning that produced it.
        Assistant => tool_like(prev) || prev == Reasoning,
        Reasoning => tool_like(prev),
        // An agent-level failure is its own block, not a footnote to a tool.
        ErrorK => tool_like(prev) || matches!(prev, Assistant | Reasoning),
        _ => false,
    }
}

/// Resolve a theme name for this terminal. A 16-color console (the Pi's
/// framebuffer, `TERM=linux`) can't display a theme's `Rgb` shades at all — it
/// emits the escape anyway, the console ignores the unknown parameters, and the
/// cell keeps whatever color was last in effect. So those terminals get the
/// theme with every shade snapped to the nearest ANSI color (see
/// `palette::for_16color`); a 256-color or truecolor terminal is left alone.
fn palette_for(theme: &str) -> Palette {
    let p = palette_by_name(theme);
    if is_16color_terminal() {
        palette::for_16color(p)
    } else {
        p
    }
}

impl App {
    pub fn new(cfg: UiConfig, history: Vec<String>) -> App {
        let hist_idx = history.len();
        let segments = statusline::effective(&cfg.settings.statusline);
        let mut app = App {
            transcript: Vec::new(),
            banner_status: Vec::new(),
            live: String::new(),
            live_reasoning: String::new(),
            current_activity: None,
            awaiting_response: false,
            input: String::new(),
            cursor: 0,
            term_w: 80,
            term_h: 24,
            history,
            hist_idx,
            pending: String::new(),
            queued: Vec::new(),
            queue_paused: false,
            suggestion: None,
            suggest_idx: 0,
            picker: None,
            mode: Mode::Idle,
            pw_input: String::new(),
            pw_reply: None,
            q_input: String::new(),
            wizard: None,
            wiz_input: String::new(),
            wiz_cursor: 0,
            q_reply: None,
            follow: true,
            scrolled_up: false,
            new_below: false,
            scroll: 0,
            max_top: 0,
            view_h: 0,
            select: None,
            output_area: Rect::new(0, 0, 0, 0),
            composer_area: Rect::new(0, 0, 0, 0),
            composer_first: 0,
            flash: None,
            screen: None,
            spinner: 0,
            spin_counter: 0,
            busy_since: None,
            tool_running: None,
            model_info: cfg.model,
            last_models: Vec::new(),
            session_label: String::new(),
            should_quit: false,
            esc_deadline: None,
            last_ctrl_c: None,
            kill_ring: String::new(),
            cmd_uses: HashMap::new(),
            force_clear: false,
            single_width: cfg.ascii
                || matches!(std::env::var("TERM").as_deref(), Ok("linux")),
            glyphs: if cfg.ascii { GLYPHS_A } else { GLYPHS_U },
            ascii: cfg.ascii,
            palette: palette_for(&cfg.theme),
            disp_cache: Vec::new(),
            disp_cache_width: usize::MAX,
            disp_cache_tver: u64::MAX,
            disp_cache_view: (false, false),
            show_thinking: cfg.settings.show_thinking,
            tver: 0,
            perm: cfg.perm,
            ctx_limit: cfg.ctx_limit.max(1),
            price_in: cfg.price_in,
            price_out: cfg.price_out,
            price_cached_in: cfg.price_cached_in,
            price_currency: cfg.price_currency.clone(),
            cost_known: cfg.cost_known,
            last_prompt_tokens: 0,
            sess_prompt: 0,
            sess_completion: 0,
            sess_cached: 0,
            sess_written: 0,
            banked_cost: 0.0,
            banked_at: money::Spent::default(),
            balance: None,
            settings: cfg.settings,
            cwd_label: cwd_label(),
            git_head: None,
            git_checked_at: None,
            statusline: segments,
            clock_min: -1,
            clock_checked: None,
            appr_rule: None,
            session_rev: std::env::current_dir().ok().and_then(|d| crate::tools::git_rev(&d)),
            custom_cmds: Vec::new(),
            custom_palette: Vec::new(),
            mcp_palette: Vec::new(),
        };
        app.rebuild_cmd_uses();
        app.reload_custom_commands();
        app
    }

    /// (Re)discover custom slash commands; returns how many were found.
    fn reload_custom_commands(&mut self) -> usize {
        self.custom_cmds = crate::commands::load();
        self.custom_palette = self
            .custom_cmds
            .iter()
            .map(|c| {
                let name: &'static str = Box::leak(format!("/{}", c.name).into_boxed_str());
                let desc: &'static str = Box::leak(c.description.clone().into_boxed_str());
                (name, desc)
            })
            .collect();
        self.custom_cmds.len()
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    fn refresh_git_head(&mut self) {
        // Throttle: only probe git once every 5 seconds within render_output.
        if let Some(t) = self.git_checked_at {
            if t.elapsed() < Duration::from_secs(5) {
                return;
            }
        }
        self.git_checked_at = Some(Instant::now());
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return,
        };
        self.git_head = crate::tools::git_head(&cwd);
    }

    fn push(&mut self, kind: Kind, text: impl Into<String>) {
        // The terminal can't lay out a raw tab (ratatui gives it no width), and
        // read_file's line numbers end in one: show tabs as spaces.
        let text = text.into().replace('\t', "    ");
        if text.is_empty() && kind != Kind::Banner && kind != Kind::BannerDim {
            return;
        }
        self.separate(kind);
        self.transcript.push(TLine { kind, text, lead: true, color: None });
        self.after_push();
    }

    /// Insert a blank separator line when `kind` opens a new block (see
    /// `needs_sep`), so turns, prose, and tool activity don't run together into
    /// one unreadable wall of text.
    fn separate(&mut self, kind: Kind) {
        if let Some(last) = self.transcript.last() {
            if needs_sep(last.kind, kind) {
                self.transcript.push(TLine {
                    kind: Kind::Blank,
                    text: String::new(),
                    lead: true,
                    color: None,
                });
            }
        }
    }

    fn dirty(&mut self) {
        self.tver = self.tver.wrapping_add(1);
    }

    fn set_palette(&mut self, p: Palette) {
        self.palette = self.terminal_safe(p);
        self.dirty();
    }

    /// Adapt a palette to what this terminal can actually display, so a theme
    /// switch on the Pi's console doesn't reintroduce colors it can't show.
    fn terminal_safe(&self, p: Palette) -> Palette {
        if is_16color_terminal() {
            palette::for_16color(p)
        } else {
            p
        }
    }

    fn after_push(&mut self) {
        self.dirty();
        if self.transcript.len() > MAX_TRANSCRIPT {
            let excess = self.transcript.len() - MAX_TRANSCRIPT;
            self.transcript.drain(0..excess);
        }
        if self.follow {
            self.scroll = self.max_top;
        } else {
            self.scrolled_up = true;
            self.new_below = true;
        }
    }

    #[allow(dead_code)]
    fn push_assistant(&mut self, text: &str) {
        if let Some(last) = self.transcript.last_mut() {
            if last.kind == Kind::Assistant {
                last.text.push_str(text);
                self.dirty();
                return;
            }
        }
        self.transcript.push(TLine { kind: Kind::Assistant, text: text.to_string(), lead: true, color: None });
        self.dirty();
    }

    fn flush_live(&mut self) {
        // Reasoning precedes the answer it produced (and `build_live_lines`
        // shows them in this order), so commit it first.
        let reasoning = std::mem::take(&mut self.live_reasoning);
        if !reasoning.trim().is_empty() {
            self.separate(Kind::Reasoning);
            self.transcript.push(TLine { kind: Kind::Reasoning, text: reasoning, lead: true, color: None });
            self.after_push();
        }
        if !self.live.is_empty() {
            self.separate(Kind::Assistant);
            self.transcript.push(TLine { kind: Kind::Assistant, text: std::mem::take(&mut self.live), lead: true, color: None });
            self.after_push();
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests;

/// The UI event loop. Owns the terminal; returns when the user quits.
pub fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    ui_rx: Receiver<UiEvent>,
    h: &Handles,
) -> std::io::Result<()> {
    // Paint once up front: the Pi's local console sends no initial resize
    // event, so without this the screen stays blank until the first keypress.
    terminal.draw(|f| app.render(f))?;
    loop {
        let mut dirty = false;
        while let Ok(ev) = ui_rx.try_recv() {
            app.handle_event(ev, h);
            dirty = true;
        }
        if app.should_quit() {
            break;
        }
        // Haiku sends no SIGHUP when the terminal goes away (window closed,
        // ssh dropped): the tty just polls as hung up, which crossterm ignores,
        // so this loop would spin on as an orphan. Leave as if quit.
        #[cfg(target_os = "haiku")]
        if helpers::tty_hung_up() {
            break;
        }
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(k)
                    if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    app.on_key(k, h);
                    dirty = true;
                }
                Event::Paste(s) => {
                    app.on_paste(s);
                    dirty = true;
                }
                Event::Mouse(m) => match app.on_mouse(m.kind, m.column, m.row) {
                    MouseAction::None => {}
                    MouseAction::Redraw => dirty = true,
                    MouseAction::Copy(kind, start, end) => {
                        match kind {
                            SelectKind::Output => app.copy_range(start, end),
                            SelectKind::Composer => app.copy_input_range(start, end),
                        }
                        dirty = true;
                    }
                },
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
        // Flush an expired ESC timeout (no key arrived after ESC).
        if let Some(deadline) = app.esc_deadline {
            if Instant::now() >= deadline {
                app.esc_deadline = None;
                app.do_esc(h);
                dirty = true;
            }
        }
        if app.busy() && app.tick_spinner() {
            dirty = true;
        }
        if app.clock_due() {
            dirty = true;
        }
        if app.flash_due() {
            dirty = true;
        }
        if app.force_clear {
            app.force_clear = false;
            terminal.clear()?;
            dirty = true;
        }
        if dirty {
            let frame = terminal.draw(|f| app.render(f))?;
            // The frame just drawn is only borrowable here (see `screen`).
            if app.selection().is_some() {
                app.capture_frame(frame.buffer);
            }
        }
    }
    Ok(())
}

/// Capture wheel events by default in every terminal, including Warp, so the
/// terminal cannot translate them into Up/Down keys that navigate input history.
/// Explicit HAIKUCODE_MOUSE=0/false/off/no opts out of application mouse handling.
pub fn want_mouse_capture(_term_program: &str, override_env: Option<&str>) -> bool {
    match override_env.map(str::to_ascii_lowercase).as_deref() {
        Some("1") | Some("true") | Some("on") | Some("yes") => true,
        Some("0") | Some("false") | Some("off") | Some("no") => false,
        _ => true,
    }
}

/// Enter alt-screen + raw mode with bracketed paste; returns a ready Terminal.
pub fn setup_terminal() -> std::io::Result<Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>>
{
    let mut term = ratatui::init();
    let _ = execute!(std::io::stdout(), event::EnableBracketedPaste);
    let mouse = want_mouse_capture(
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        std::env::var("HAIKUCODE_MOUSE").ok().as_deref(),
    );
    if mouse {
        let _ = execute!(std::io::stdout(), event::EnableMouseCapture);
    }
    // Ask the terminal to report modified keys unambiguously (Kitty keyboard
    // protocol). Without this, terminals like Warp drop the Option/Alt modifier
    // on Backspace in full-screen apps and send a bare 0x7f, so Option+Backspace
    // is indistinguishable from a plain Backspace. With DISAMBIGUATE_ESCAPE_CODES
    // it arrives as Alt+Backspace (and Option+Delete as Alt+Delete), which the
    // composer already turns into word-delete. Terminals that don't support it
    // ignore the push, so this is safe everywhere.
    if matches!(
        ratatui::crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::all())
        );
    }
    term.clear()?;
    Ok(term)
}

pub fn restore_terminal(console: bool) {
    use ratatui::crossterm::cursor::MoveTo;
    use ratatui::crossterm::terminal::{Clear, ClearType};
    let mut out = std::io::stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, event::DisableMouseCapture, event::DisableBracketedPaste);
    ratatui::restore();
    if console {
        let _ = execute!(out, Clear(ClearType::All), MoveTo(0, 0));
    }
}
