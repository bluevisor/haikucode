//! The agent worker: a long-lived thread that owns the conversation and runs
//! the blocking model/tool loop, talking to the UI thread over channels.

use crate::api::{self, Message};
use crate::config::{Config, ConfigPatch};
use crate::hooks::Hooks;
use crate::mcp::Mcp;
use crate::policy::Policy;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Fallback limit used when max_tool_calls is 0 ("auto"). High enough to let
/// the model converge naturally; low enough to catch runaway loops.
const AUTO_STEPS: usize = 500;
const SESSION_START_TAG: &str = "SessionStart hook context:";

/// Permission modes (cycled with Shift+Tab in the UI).
pub const PERM_ASK: u8 = 0; // prompt before each write/edit/bash
pub const PERM_AUTO: u8 = 1; // YOLO — auto-approve everything
pub const PERM_PLAN: u8 = 2; // read-only — refuse writes/bash, ask the model to plan

/// Worker → UI.
pub enum UiEvent {
    Token(String),
    Reasoning(String),
    ResetLive,
    AssistantCommit,
    ToolStart { name: String, summary: String },
    Diff(String),
    ToolResult { ok: bool, preview: String },
    /// Ask the user to approve `desc`. `rule` is the permission rule the UI can
    /// offer as "don't ask again for this" (persisted via ApprovalResponse::Rule).
    Approval { desc: String, rule: Option<String> },
    /// sudo (via the askpass helper) needs a password. The UI pops a masked
    /// prompt and sends the result back over `reply` (None = user cancelled).
    PasswordRequest { prompt: String, reply: Sender<Option<String>> },
    /// The ask_user tool: the UI pops a visible input line and sends the
    /// answer back over `reply` (None = user declined).
    Question { prompt: String, reply: Sender<Option<String>> },
    ModelList(Vec<String>),
    /// The running MCP servers' prompts, as (`/mcp__server__prompt`, description)
    /// slash-palette entries.
    McpPrompts(Vec<(String, String)>),
    /// An MCP prompt fetched for `/mcp__server__prompt args`: send `text` as
    /// the user's turn (echoed as `shown`).
    McpPromptReady { shown: String, text: String },
    /// `/rewind` choices, newest first: "N. prompt preview".
    TurnList(Vec<String>),
    /// After a rewind: the prompt that was undone, to put back in the composer.
    Rewound(String),
    ModelChanged(String),
    /// One completion's tokens (cached input broken out, for cost).
    Usage(crate::money::Spent),
    /// Tokens spent by a background call (compaction summary, follow-up
    /// suggestion): counted toward session cost, but not the ctx bar.
    Spend(crate::money::Spent),
    /// Re-estimate of the next prompt's size (after compaction) so the UI's
    /// context bar drops immediately, without touching session totals.
    Context(u32),
    /// Auto-detected model context window (tokens): updates the ctx bar and the
    /// `/config` panel's displayed value live, without the user pinning it.
    ContextLimit(u32),
    /// A subscription sign-in finished: the provider and its token.
    SignedIn { provider: String, token: crate::config::OAuthToken },
    /// A subscription sign-in failed or was cancelled.
    SignInFailed { provider: String, error: String },
    /// `ListModels` failed (the UI decides: an error line, or a setup step).
    ModelListFailed(String),
    /// Account balance, with the currency the provider billed it in (see
    /// `money::Balance`), so the status line can't mix units silently.
    Balance(crate::money::Balance),
    /// The provider changed: drop the old account's balance from the status
    /// line (the new one may not report one at all).
    BalanceCleared,
    Notice(String),
    /// Progress chatter about tool work ("running 3 read-only calls in
    /// parallel…"): folded into the brief summary like the tools themselves.
    Progress(String),
    Error(String),
    TurnDone,
    /// A suggested follow-up prompt generated after the turn completes.
    Suggestion(String),
    /// `/resume` choices, newest first: "id  label · when · n msgs".
    SessionList(Vec<String>),
    /// Now in another session: the transcript is replaced by `replay`
    /// ((role, text) of its recent exchanges) and `label` names the session.
    SessionSwitched { label: String, replay: Vec<(String, String)> },
    /// The current session ("id — label"), for /status.
    SessionLabel(String),
}

/// UI → Worker control messages (processed between turns).
pub enum WorkerCmd {
    /// A prompt: `text` as the model gets it (attachments expanded), `typed`
    /// as the user wrote it, which `/rewind` hands back to the composer.
    User { text: String, images: Vec<String>, typed: String },
    Reset,
    /// `/compact [focus]`: summarize older turns, optionally steering the
    /// summary toward what the user wants preserved.
    Compact(Option<String>),
    /// `!cmd` from the composer: run a shell command directly, show its output,
    /// and add both to the conversation so the model can see what the user saw.
    Shell(String),
    /// Add a note to the conversation (as the user) without starting a turn —
    /// e.g. "/undo reverted commit X" so the model's picture of the tree stays true.
    Inject(String),
    /// Permission rules or hooks changed on disk: re-read them.
    ReloadSettings,
    SetModel(String),
    /// Run the OAuth subscription flow and store the token. No UI sends it
    /// since `/login` was hidden; kept for when native adapters exist.
    #[allow(dead_code)]
    Login(String),
    /// A `/config` panel change: apply to the live config and persist.
    Patch(ConfigPatch),
    ListModels,
    ListMcp,
    /// `/mcp approve|deny|login|logout <server>`.
    Mcp(String),
    /// `/mcp__server__prompt args`: fetch the prompt for the UI to send.
    McpPrompt { name: String, args: String },
    /// `/rewind`: list this session's prompts for the picker.
    ListTurns,
    /// Rewind to just before turn N (0-based, oldest first): undo haikucode's
    /// checkpoints since then and drop that prompt and everything after it.
    Rewind(usize),
    /// `/new`: start a new session; the current one stays saved.
    New,
    /// `/resume`: list this directory's sessions for the picker.
    ListSessions,
    /// Switch to the saved session `key` names (an id, id prefix or name).
    Resume(String),
    /// `/rename <name>` the current session.
    Rename(String),
    /// `/fork`: carry on in a copy of this conversation; the original stays.
    Fork,
    /// `/export [--all] [file]`: write the conversation as Markdown.
    Export(String),
    /// `/sessions [delete <id|name>]`: list or delete saved sessions.
    Sessions(String),
    /// `/update [--force]`: install the latest GitHub release.
    Update(bool),
    Quit,
}

#[derive(Clone)]
pub enum ApprovalResponse {
    Yes,
    No,
    Always,
    /// Yes, and persist this allow rule so the same call is never asked again.
    Rule(String),
}

pub struct Shared {
    pub cancel: Arc<AtomicBool>,
    pub perm: Arc<AtomicU8>,
}

pub struct Handles {
    pub join: JoinHandle<()>,
    pub cmd_tx: Sender<WorkerCmd>,
    pub appr_tx: Sender<ApprovalResponse>,
    pub shared: Shared,
}

struct Worker {
    http: ureq::Agent,
    cfg: Config,
    messages: Vec<Message>,
    system_len: usize,
    cancel: Arc<AtomicBool>,
    /// Cancels the in-flight follow-up suggestion call from the previous turn
    /// so a superseded suggestion neither bills nor surfaces stale.
    suggest_cancel: Arc<AtomicBool>,
    perm: Arc<AtomicU8>,
    ui: Sender<UiEvent>,
    appr_rx: Receiver<ApprovalResponse>,
    session: Option<PathBuf>,
    /// Prompt tokens of the last completion, for the auto-compaction trigger.
    last_prompt: u32,
    /// True while a sub-agent is running: suppresses streaming the sub-agent's
    /// tokens as the main reply, and blocks nested `task` calls.
    quiet: bool,
    /// Images queued by view_image, injected as a user message after the
    /// current round of tool results.
    pending_images: Vec<String>,
    /// Launched MCP servers and their tools.
    mcp: Mcp,
    /// Built-in + MCP tool schema, rebuilt once at startup; sent each request.
    tools: Value,
    /// Permission rules (allow/deny) merged from config + project settings.
    policy: Policy,
    /// Lifecycle hooks (PreToolUse, PostToolUse, UserPromptSubmit, Stop).
    hooks: Hooks,
    /// Hashes of this turn's tool calls, newest last — thrashing detection.
    recent_calls: Vec<u64>,
    /// Context window the provider's `/models` advertised, tagged with the
    /// model it was fetched for (filled by a background thread). Preferred
    /// over the built-in table when sizing auto-compaction.
    /// The window `/models` advertised, keyed by provider, base URL and
    /// model: the same model id behind another server can have another limit.
    ctx_detected: Arc<Mutex<Option<(String, u32)>>>,
    /// Bumped by every context-window / balance refresh, so a background
    /// lookup that finishes after a newer switch is dropped, not shown.
    ctx_gen: Arc<std::sync::atomic::AtomicU64>,
    balance_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Where each prompt of this run starts, for `/rewind`.
    turns: Vec<TurnMark>,
    /// When the session was last written mid-turn (see run_loop).
    last_save: std::time::Instant,
    /// Generation across all main-model calls for one user prompt.
    generation: GenerationStats,
}

/// A user prompt `/rewind` can go back to: its index in `messages` and the
/// git HEAD when it was sent (None without a checkpointed repo).
#[derive(Clone)]
struct TurnMark {
    at: usize,
    head: Option<String>,
    prompt: String,
}

mod compact;
mod provider;
mod session;
mod subagent;
mod tool_calls;
mod turn;
mod worker;

pub use session::repair_orphans;
pub use worker::spawn;

/// A background call's token usage as a session-cost event.
fn spend(u: &api::Usage) -> UiEvent {
    UiEvent::Spend(u.spent())
}

/// Aggregate generation time rather than averaging per-call speeds. Waiting
/// for the first token, tool execution, and approvals do not enter this clock.
#[derive(Default)]
struct GenerationStats {
    tokens: u64,
    elapsed: std::time::Duration,
}

impl GenerationStats {
    fn add(&mut self, tokens: u32, elapsed: std::time::Duration) {
        // Buffered/instantaneous responses have no useful streaming rate.
        if tokens > 0 && elapsed.as_millis() >= 100 {
            self.tokens += tokens as u64;
            self.elapsed += elapsed;
        }
    }

    fn rate(&self) -> Option<f64> {
        (self.tokens > 0).then(|| self.tokens as f64 / self.elapsed.as_secs_f64())
    }
}

fn preview(s: &str, maxlines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= maxlines {
        return s.trim_end().to_string();
    }
    let head = lines[..maxlines].join("\n");
    format!("{head}\n… (+{} more lines)", lines.len() - maxlines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn generation_speed_aggregates_the_whole_prompt_and_resets() {
        use std::time::Duration;
        let mut stats = super::GenerationStats::default();
        assert_eq!(stats.rate(), None);
        stats.add(80, Duration::from_secs(2));
        stats.add(120, Duration::from_secs(6));
        assert_eq!(stats.rate(), Some(25.0)); // 200 tokens / 8s, not mean(40, 20)
        stats.add(0, Duration::from_secs(10));
        stats.add(80, Duration::from_millis(5));
        assert_eq!(stats.rate(), Some(25.0));
        stats = super::GenerationStats::default();
        assert_eq!(stats.rate(), None);
    }
}
