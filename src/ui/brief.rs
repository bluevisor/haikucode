//! Brief view: persistent thoughts with transient tool activity instead of
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

/// Keep each thought as a list entry. Routine tool activity belongs only in
/// the current status line; failures remain visible for later review.
pub fn summarize(run: &[TLine]) -> Vec<(&'static str, String)> {
    run.iter().filter_map(|t| match t.kind {
        Kind::Reasoning => {
            let text = gist(&t.text, 120);
            (!text.is_empty()).then_some(("", text))
        }
        Kind::ToolErr => Some(("error", t.text.clone())),
        _ => None,
    }).collect()
}

/// A running tool's action, replaced in place as work progresses.
pub fn action(name: &str, summary: &str) -> String {
    let verb = match name {
        "read_file" => "Reading",
        "grep" | "glob" | "web_search" | "recall" => "Searching",
        "list_files" => "Listing",
        "web_fetch" => "Fetching",
        "write_file" => "Writing",
        "edit_file" | "multi_edit" => "Editing",
        "bash" | "bash_bg" => "Running",
        _ => {
            return format!("Running {name} {}", summary.trim())
                .trim_end()
                .to_string()
        }
    };
    format!("{verb} {}", summary.trim()).trim_end().to_string()
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
    fn thoughts_stay_while_routine_activity_disappears() {
        let rows = summarize(&[
            tl(Kind::Reasoning, "Find the renderer. Then update it."),
            tl(Kind::Tool, "read_file src/ui.rs"),
            tl(Kind::ToolResult, "contents"),
            tl(Kind::Reasoning, "Keep the thoughts visible."),
            tl(Kind::Tool, "edit_file src/ui.rs"),
            tl(Kind::DiffAdd, "+ new"),
            tl(Kind::Progress, "checking"),
            tl(Kind::ToolErr, "Permission denied"),
        ]);
        assert_eq!(rows, vec![
            ("", "Find the renderer.".into()),
            ("", "Keep the thoughts visible.".into()),
            ("error", "Permission denied".into()),
        ]);
        assert!(summarize(&[tl(Kind::Tool, "bash cargo test"), tl(Kind::ToolResult, "ok")]).is_empty());
    }
}
