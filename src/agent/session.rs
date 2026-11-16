//! Saved sessions: save, label, /resume, /rename, /fork, /export, /sessions.

use super::compact::estimate_tokens;
use super::{UiEvent, Worker, WorkerCmd};
use crate::api::Message;
use crate::system_prompt;
use crate::tools;
use std::sync::atomic::Ordering;

impl Worker {
    /// The conversation as sessions store it. Image payloads are stripped: a
    /// few photos would balloon a file rewritten every turn (painful on a Pi
    /// Zero); the text around each image keeps a resumed session coherent.
    fn slim_messages(&self) -> Vec<Message> {
        self.messages
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.images.clear();
                m
            })
            .collect()
    }

    pub(super) fn save_session(&self) {
        let Some(path) = &self.session else { return };
        // Atomic writes: a power loss mid-save must not corrupt the session
        // `--continue` depends on — the previous turn's file stays intact.
        let _ = crate::sessions::save(&crate::sessions::Store::here(), path, &self.slim_messages());
    }

    /// "id — label" of the current session, for /status.
    pub(super) fn session_label(&self) -> String {
        let Some(path) = &self.session else {
            return "not saved (one-shot run)".into();
        };
        let meta = crate::sessions::read_meta(path).unwrap_or_else(|| crate::sessions::Meta {
            id: path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            title: crate::sessions::title_of(&self.messages),
            ..Default::default()
        });
        format!("{} — {}", meta.id, meta.label())
    }

    /// /resume, /rename, /fork, /export and /sessions.
    pub(super) fn session_cmd(&mut self, cmd: WorkerCmd) {
        use crate::sessions::{self as ss, Store};
        let store = Store::here();
        let now = crate::config::now_unix();
        let err = |w: &Self, e: String| {
            let _ = w.ui.send(UiEvent::Error(e));
        };
        match cmd {
            WorkerCmd::ListSessions => {
                let items: Vec<String> = store
                    .list()
                    .iter()
                    .map(|(m, p)| ss::summary_line(m, now, self.session.as_ref() == Some(p)))
                    .collect();
                if items.is_empty() {
                    let _ = self.ui.send(UiEvent::Notice("no saved sessions in this directory yet.".into()));
                }
                let _ = self.ui.send(UiEvent::SessionList(items));
            }
            WorkerCmd::Resume(key) => match store.find(&key) {
                Err(e) => err(self, e),
                Ok((_, path)) if self.session.as_ref() == Some(&path) => {
                    let _ = self.ui.send(UiEvent::Notice("already in that session.".into()));
                }
                Ok((meta, path)) => match ss::load(&path) {
                    None => err(self, format!("session {} is empty or unreadable.", meta.id)),
                    Some(mut msgs) => {
                        repair_orphans(&mut msgs);
                        self.suggest_cancel.store(true, Ordering::Relaxed);
                        self.messages = msgs;
                        self.system_len = system_prefix_len(&self.messages);
                        // The saved prompt describes the settings it was saved
                        // under; speak to the live ones (as the AutoCommit patch does).
                        if self.system_len > 0 {
                            self.messages[0] = Message::system(system_prompt(self.cfg.auto_commit));
                        }
                        self.session = Some(path);
                        // Turn marks index the old conversation.
                        self.turns.clear();
                        self.recent_calls.clear();
                        self.pending_images.clear();
                        let est = estimate_tokens(&self.messages);
                        self.last_prompt = est;
                        let _ = self.ui.send(UiEvent::Context(est));
                        let _ = self.ui.send(UiEvent::SessionSwitched {
                            label: self.session_label(),
                            replay: ss::replay(&self.messages),
                        });
                    }
                },
            },
            WorkerCmd::Rename(name) => {
                let Some(path) = self.session.clone() else {
                    return err(self, "this run isn't saved, so it can't be named.".into());
                };
                self.save_session();
                match ss::rename(&path, &name) {
                    Ok(m) => {
                        let _ = self.ui.send(UiEvent::Notice(format!("session {} is now \"{}\".", m.id, m.label())));
                        let _ = self.ui.send(UiEvent::SessionLabel(self.session_label()));
                    }
                    Err(e) => err(self, format!("rename failed: {e}")),
                }
            }
            WorkerCmd::Fork => {
                let Some(from) = self.session.clone() else {
                    return err(self, "this run isn't saved, so there's nothing to fork.".into());
                };
                match ss::fork(&store, &from, &self.slim_messages()) {
                    Ok(to) => {
                        let old = from.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                        self.session = Some(to);
                        let _ = self.ui.send(UiEvent::Notice(format!(
                            "forked: you're now in {}; {old} stays as it was (/resume to go back).",
                            self.session_label()
                        )));
                        let _ = self.ui.send(UiEvent::SessionLabel(self.session_label()));
                    }
                    Err(e) => err(self, format!("fork failed: {e}")),
                }
            }
            WorkerCmd::Export(arg) => {
                let all = arg.split_whitespace().any(|a| a == "--all");
                let file: String = arg.split_whitespace().filter(|a| *a != "--all").collect::<Vec<_>>().join(" ");
                let meta = self.session.as_deref().and_then(ss::read_meta).unwrap_or_else(|| ss::Meta {
                    id: self.session_label().split(' ').next().unwrap_or("").to_string(),
                    title: ss::title_of(&self.messages),
                    created: now,
                    cwd: store.cwd.display().to_string(),
                    ..Default::default()
                });
                let target = if file.is_empty() {
                    let name = ss::export_name(&meta);
                    let p = store.cwd.join(&name);
                    if p.exists() {
                        store.cwd.join(name.replace(".md", &format!("-{}.md", meta.id)))
                    } else {
                        p
                    }
                } else {
                    store.cwd.join(tools::expand(&file))
                };
                match std::fs::write(&target, ss::to_markdown(&meta, &self.messages, all)) {
                    Ok(()) => {
                        let _ = self.ui.send(UiEvent::Notice(format!("exported the conversation to {}", target.display())));
                    }
                    Err(e) => err(self, format!("export failed: {e}")),
                }
            }
            WorkerCmd::Sessions(arg) => {
                let arg = arg.trim();
                if let Some(key) = arg.strip_prefix("delete").map(str::trim).filter(|_| arg.starts_with("delete")) {
                    if key.is_empty() {
                        return err(self, "usage: /sessions delete <id|name>".into());
                    }
                    match store.find(key) {
                        Err(e) => err(self, e),
                        Ok((_, p)) if self.session.as_ref() == Some(&p) => {
                            err(self, "that's the session you're in; /new or /resume another first.".into())
                        }
                        Ok((m, p)) => {
                            ss::delete(&p);
                            let _ = self.ui.send(UiEvent::Notice(format!("deleted session {} ({}).", m.id, m.label())));
                        }
                    }
                } else if arg.is_empty() {
                    let list = store.list();
                    let _ = self.ui.send(UiEvent::Notice(format!(
                        "{} saved session(s) here, newest first (/resume to switch, /sessions delete <id>):",
                        list.len()
                    )));
                    for (m, p) in list.iter().take(30) {
                        let _ = self.ui.send(UiEvent::Notice(format!(
                            "  {}",
                            ss::summary_line(m, now, self.session.as_ref() == Some(p))
                        )));
                    }
                } else {
                    err(self, "usage: /sessions, or /sessions delete <id|name>".into());
                }
            }
            _ => {}
        }
    }
}

/// Answer every `tool_call` in the last assistant message that has no `tool`
/// result following it, so the history is valid for the next request. Used
/// after a panic mid tool-round and when loading a session from disk.
pub fn repair_orphans(messages: &mut Vec<Message>) {
    let Some(ai) = messages.iter().rposition(|m| m.role == "assistant") else { return };
    let Some(calls) = messages[ai].tool_calls.clone() else { return };
    let answered: Vec<String> = messages[ai + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    // Results must directly follow their call; insert after the last tool result.
    let mut at = ai + 1;
    while at < messages.len() && messages[at].role == "tool" {
        at += 1;
    }
    for c in calls {
        if !answered.contains(&c.id) {
            messages.insert(at, Message::tool(c.id.clone(), "(interrupted: tool did not run)".into()));
            at += 1;
        }
    }
}

/// Length of the leading run of system messages — the conversation prefix
/// that /reset and /compact must preserve.
pub(super) fn system_prefix_len(messages: &[Message]) -> usize {
    messages.iter().take_while(|m| m.role == "system").count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_orphans_answers_unanswered_tool_calls_only() {
        use crate::api::{FunctionCall, ToolCall};
        let call = |id: &str| ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: "bash".into(), arguments: "{}".into() },
        };
        let mut msgs = vec![
            Message::system("s"),
            Message::user("u"),
            Message { role: "assistant".into(), content: String::new(), images: vec![], tool_calls: Some(vec![call("a"), call("b"), call("c")]), tool_call_id: None, native: None },
            Message::tool("a".into(), "done".into()),
        ];
        repair_orphans(&mut msgs);
        let ids: Vec<_> = msgs.iter().filter(|m| m.role == "tool").map(|m| m.tool_call_id.clone().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(msgs[3].content, "done", "the real result is kept");
        // Idempotent, and a no-op on a healthy history.
        let before = msgs.len();
        repair_orphans(&mut msgs);
        assert_eq!(msgs.len(), before);
    }

    #[test]
    fn system_prefix_counts_leading_system_only() {
        // Fresh session: every startup message is system.
        let fresh = vec![Message::system("a"), Message::system("b")];
        assert_eq!(system_prefix_len(&fresh), 2);
        // Resumed session: the prefix ends at the first user message, even
        // though the whole history was passed in at spawn.
        let resumed = vec![
            Message::system("prompt"),
            Message::system("memory"),
            Message::user("hi"),
            Message::system("not a prefix message"),
        ];
        assert_eq!(system_prefix_len(&resumed), 2);
        assert_eq!(system_prefix_len(&[]), 0);
    }
}
