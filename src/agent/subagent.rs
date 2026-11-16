//! Sub-agents for the `task` tool.

use super::{UiEvent, Worker, AUTO_STEPS};
use crate::api::Message;

impl Worker {
    /// Run a delegated task in an isolated sub-agent: a fresh conversation with
    /// filtered tools (no `task` for recursion, no `ask_user` — sub-agents must
    /// be autonomous). Only its final report returns to the parent — the
    /// intermediate steps never enter the parent's context. Wrapped in
    /// catch_unwind so a sub-agent panic cannot poison the parent worker.
    pub(super) fn run_subagent(&mut self, task: &str) -> String {
        // Build a tool schema for the sub-agent: built-ins minus task/ask_user + MCP.
        let sub_tools = crate::api::tools_spec_subagent(self.mcp.tools());
        let sub_max = if self.cfg.max_tool_calls == 0 {
            AUTO_STEPS
        } else {
            self.cfg.max_tool_calls as usize
        };
        // Swap in a fresh context; restore the parent's afterward.
        let saved_msgs = std::mem::replace(
            &mut self.messages,
            vec![
                Message::system(subagent_prompt(sub_max)),
                Message::user(task.to_string()),
            ],
        );
        let saved_len = self.system_len;
        let saved_prompt = self.last_prompt;
        let saved_tools = std::mem::replace(&mut self.tools, sub_tools);
        let saved_calls = std::mem::take(&mut self.recent_calls);
        let saved_images = std::mem::take(&mut self.pending_images);
        self.system_len = 1;
        self.quiet = true;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_loop()
        }));

        // Always restore parent state, even if the sub-agent panicked.
        self.quiet = false;
        self.system_len = saved_len;
        self.last_prompt = saved_prompt;
        self.tools = saved_tools;
        self.messages = saved_msgs;
        self.recent_calls = saved_calls;
        self.pending_images = saved_images;
        let _ = self.ui.send(UiEvent::Context(saved_prompt));

        match result {
            Ok(report) => {
                if report.trim().is_empty() {
                    "(sub-agent returned no report)".to_string()
                } else {
                    report
                }
            }
            Err(_) => {
                let _ = self.ui.send(UiEvent::Error("sub-agent panicked".into()));
                "(sub-agent panicked)".to_string()
            }
        }
    }
}

/// System prompt for a delegated sub-agent. Its whole job is one task; its
/// final message becomes the report handed back to the parent.
fn subagent_prompt(max: usize) -> String {
    let host = crate::sysinfo::host_descriptor();
    format!(
        "You are a sub-agent of haikucode, a terminal coding agent running ON {host}. You were \
delegated a single focused task by the main agent.

Rules:
- Use tools to inspect and change the real filesystem; never invent file contents.
- Work autonomously — your tools are a subset of the main agent's. You cannot \
ask the user questions or spawn further sub-agents.
- You have up to {max} tool-call rounds to complete the task.
- Stay strictly within the delegated task.
- When done, reply with a concise final report (findings, files changed, key results) \
and no tool call. That report is ALL the parent agent sees, so make it self-contained.{skills}",
        max = max,
        skills = crate::commands::skills_prompt(&crate::commands::discover_skills())
            .map(|s| format!("\n\n{s}"))
            .unwrap_or_default()
    )
}
