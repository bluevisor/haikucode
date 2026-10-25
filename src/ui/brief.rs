//! Brief view: a run of thinking and tool activity collapsed into a few lines
//! (what was considered, what was looked at, which files changed) instead of
//! the full reasoning text, tool output and diffs. Toggled with Ctrl+T or
//! `/thinking`; the transcript keeps everything, so switching back shows it all.

use super::types::{Kind, TLine};

/// Entries that fold into a brief summary. Assistant replies, the user's
/// prompts and notices always show in full.
pub fn is_work(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Reasoning
            | Kind::Tool
            | Kind::ToolResult
            | Kind::ToolErr
            | Kind::DiffAdd
            | Kind::DiffDel
            | Kind::DiffCtx
            | Kind::Progress
    )
}

/// Most changed files listed before "+N more".
const MAX_FILES: usize = 4;

/// One reasoning chunk's opening thought: its first sentence, markdown
/// emphasis stripped, capped at `max` chars.
pub fn gist(text: &str, max: usize) -> String {
    let t = text.trim_start().replace("**", "");
    let t = t.trim_start_matches(['#', '-', '*', ' ']);
    let end = t
        .char_indices()
        .find(|&(i, c)| c == '\n' || (matches!(c, '.' | '?' | '!' | '。') && t[i + c.len_utf8()..].starts_with([' ', '\n'])))
        .map(|(i, c)| if c == '\n' { i } else { i + c.len_utf8() })
        .unwrap_or(t.len());
    clip(t[..end].trim(), max)
}

/// `s` cut to `max` chars with a trailing ellipsis.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Summary rows (label, text) for one run of work entries, in order: each
/// thought is its own `considered` row, standing where its live `thinking…`
/// line was (so committing it never makes the transcript jump), and the tool
/// work between thoughts becomes `explored` / `changed` rows below it.
pub fn summarize(run: &[TLine]) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let mut seg_start = 0;
    for (i, t) in run.iter().enumerate() {
        if t.kind == Kind::Reasoning {
            rows.extend(tool_rows(&run[seg_start..i]));
            seg_start = i + 1;
            let g = gist(&t.text, 120);
            if !g.is_empty() {
                rows.push(("considered", g));
            }
        }
    }
    rows.extend(tool_rows(&run[seg_start..]));
    rows
}

/// `explored` / `changed` rows for a stretch of tool calls.
fn tool_rows(run: &[TLine]) -> Vec<(&'static str, String)> {
    let (mut reads, mut searches, mut commands, mut fetches, mut other, mut errors) = (0, 0, 0, 0, 0, 0);
    // (path, added, removed) for edits that went through.
    let mut changed: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < run.len() {
        let t = &run[i];
        if t.kind == Kind::ToolErr {
            errors += 1;
        }
        if t.kind != Kind::Tool {
            i += 1;
            continue;
        }
        let (name, arg) = t.text.split_once(' ').unwrap_or((t.text.as_str(), ""));
        // This call's own diff and result run up to the next tool line.
        let end = run[i + 1..].iter().position(|x| x.kind == Kind::Tool).map(|p| i + 1 + p).unwrap_or(run.len());
        let body = &run[i + 1..end];
        let failed = body.iter().any(|x| x.kind == Kind::ToolErr);
        if failed {
            errors += 1;
        }
        match name {
            "read_file" => reads += 1,
            "grep" | "glob" | "list_files" | "web_search" | "recall" => searches += 1,
            "web_fetch" => fetches += 1,
            "bash" | "bash_bg" => commands += 1,
            "write_file" | "edit_file" | "multi_edit" if !failed => {
                let add = body.iter().filter(|x| x.kind == Kind::DiffAdd).count();
                let del = body.iter().filter(|x| x.kind == Kind::DiffDel).count();
                let path = if arg.trim().is_empty() { "files".to_string() } else { arg.trim().to_string() };
                match changed.iter_mut().find(|(p, ..)| *p == path) {
                    Some(c) => {
                        c.1 += add;
                        c.2 += del;
                    }
                    None => changed.push((path, add, del)),
                }
            }
            "write_file" | "edit_file" | "multi_edit" => {}
            _ => other += 1,
        }
        i = end;
    }

    let mut rows = Vec::new();
    let mut did = Vec::new();
    if reads > 0 {
        did.push(format!("read {}", plural(reads, "file", "files")));
    }
    if searches > 0 {
        did.push(plural(searches, "search", "searches"));
    }
    if fetches > 0 {
        did.push(plural(fetches, "fetch", "fetches"));
    }
    if commands > 0 {
        did.push(format!("ran {}", plural(commands, "command", "commands")));
    }
    if other > 0 {
        did.push(plural(other, "other tool", "other tools"));
    }
    if errors > 0 {
        did.push(plural(errors, "error", "errors"));
    }
    if !did.is_empty() {
        rows.push(("explored", did.join(" · ")));
    }
    if !changed.is_empty() {
        let mut parts: Vec<String> = changed
            .iter()
            .take(MAX_FILES)
            .map(|(p, a, d)| match (a, d) {
                (0, 0) => p.clone(),
                (a, 0) => format!("{p} +{a}"),
                (0, d) => format!("{p} −{d}"),
                (a, d) => format!("{p} +{a} −{d}"),
            })
            .collect();
        if changed.len() > MAX_FILES {
            parts.push(format!("+{} more", changed.len() - MAX_FILES));
        }
        rows.push(("changed", parts.join(" · ")));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl(kind: Kind, text: &str) -> TLine {
        TLine { kind, text: text.into(), lead: true, color: None }
    }

    #[test]
    fn gist_takes_the_first_sentence() {
        assert_eq!(gist("**Plan.** Look at the banner code first. Then wrap.", 80), "Plan.");
        assert_eq!(gist("The user wants a toggle. I should check keys.", 80), "The user wants a toggle.");
        assert_eq!(gist("no period here\nsecond line", 80), "no period here");
        assert_eq!(gist("version 1.2 of the parser is fine", 80), "version 1.2 of the parser is fine");
        assert_eq!(gist("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn a_run_collapses_to_considered_explored_changed() {
        let run = vec![
            tl(Kind::Reasoning, "Need to find where the banner is built. It lives in ui."),
            tl(Kind::Tool, "grep SYSTEM"),
            tl(Kind::ToolResult, "src/ui/banner.rs:145"),
            tl(Kind::Tool, "read_file src/ui/banner.rs"),
            tl(Kind::ToolResult, "..."),
            tl(Kind::Reasoning, "The status rows overflow the box."),
            tl(Kind::Tool, "edit_file src/ui/banner.rs"),
            tl(Kind::DiffDel, "- old"),
            tl(Kind::DiffAdd, "+ new"),
            tl(Kind::DiffAdd, "+ new2"),
            tl(Kind::ToolResult, "OK"),
            tl(Kind::Tool, "edit_file src/main.rs"),
            tl(Kind::DiffAdd, "+ x"),
            tl(Kind::ToolErr, "DENIED by user."),
            tl(Kind::Tool, "bash cargo test"),
            tl(Kind::ToolResult, "ok"),
        ];
        let rows = summarize(&run);
        // Each thought stays where it was, with the tool work it led to below.
        assert_eq!(
            rows,
            vec![
                ("considered", "Need to find where the banner is built.".to_string()),
                ("explored", "read 1 file · 1 search".to_string()),
                ("considered", "The status rows overflow the box.".to_string()),
                // The denied edit to main.rs is an error, not a change.
                ("explored", "ran 1 command · 1 error".to_string()),
                ("changed", "src/ui/banner.rs +2 −1".to_string()),
            ]
        );
    }

    #[test]
    fn thinking_only_runs_have_just_a_considered_row() {
        let rows = summarize(&[tl(Kind::Reasoning, "Simple question; answer directly.")]);
        assert_eq!(rows, vec![("considered", "Simple question; answer directly.".to_string())]);
    }
}
