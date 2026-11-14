//! The composer prompt as a status light: its marker character changes with
//! what the agent is doing, so a glance at the cursor line says whether it's
//! thinking, writing, running a command, editing files, or waiting on you.
//!
//! Every theme's prompt ends in one marker before a space (`> `, `C:\> `,
//! `sun% `, `] `), and only that character is swapped, for another one cell
//! wide. The theme keeps its look and the prompt its width, so the composer
//! text never shifts as the state changes.

/// What the agent is doing, as far as the prompt shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    /// Nothing running: the theme's own prompt.
    Idle,
    /// Waiting on the model before any reply text (reasoning, or the first token).
    Thinking,
    /// Reply text is streaming in.
    Writing,
    /// A shell command is running.
    Shell,
    /// Files are being written or edited.
    Editing,
    /// Any other tool: reading, searching, fetching, an MCP call.
    Tool,
    /// Blocked on the user: an approval, a question, a sudo password.
    Asking,
}

impl Activity {
    /// The activity a running tool stands for, by its name.
    pub fn of_tool(name: &str) -> Activity {
        match name {
            "bash" | "bash_output" | "bash_kill" => Activity::Shell,
            "write_file" | "edit_file" | "multi_edit" => Activity::Editing,
            _ => Activity::Tool,
        }
    }

    /// The marker for this activity, or None to keep the theme's. `rich`
    /// terminals get the Unicode set; ASCII and single-width consoles get
    /// characters every font has.
    fn marker(self, rich: bool) -> Option<char> {
        Some(match (self, rich) {
            (Activity::Idle, _) => return None,
            (Activity::Thinking, true) => '∴',
            (Activity::Thinking, false) => '~',
            (Activity::Writing, true) => '»',
            (Activity::Writing, false) => '=',
            (Activity::Shell, _) => '$',
            (Activity::Editing, true) => '±',
            (Activity::Editing, false) => '+',
            (Activity::Tool, true) => '◉',
            (Activity::Tool, false) => '*',
            (Activity::Asking, _) => '?',
        })
    }
}

/// `base` (a theme's prompt) with its marker, the last non-space character,
/// swapped for the activity's.
pub fn prompt_for(base: &str, activity: Activity, rich: bool) -> String {
    let Some(marker) = activity.marker(rich) else { return base.to_string() };
    let body = base.trim_end();
    let Some(last) = body.chars().last() else { return base.to_string() };
    format!("{}{marker}{}", &body[..body.len() - last.len_utf8()], &base[body.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn idle_keeps_the_themes_prompt() {
        for base in ["> ", "❯ ", "C:\\> ", "sun% ", "] "] {
            assert_eq!(prompt_for(base, Activity::Idle, true), base);
        }
    }

    #[test]
    fn only_the_marker_changes_and_the_width_holds() {
        assert_eq!(prompt_for("> ", Activity::Thinking, true), "∴ ");
        assert_eq!(prompt_for("C:\\> ", Activity::Shell, true), "C:\\$ ");
        assert_eq!(prompt_for("NeXT> ", Activity::Asking, true), "NeXT? ");
        assert_eq!(prompt_for("❯ ", Activity::Editing, false), "+ ");
        let all = [
            Activity::Thinking,
            Activity::Writing,
            Activity::Shell,
            Activity::Editing,
            Activity::Tool,
            Activity::Asking,
        ];
        for base in ["> ", "❯ ", "C:\\> ", "sun% ", "irix# ", "] ", "~ "] {
            for a in all {
                for rich in [true, false] {
                    let p = prompt_for(base, a, rich);
                    assert_eq!(p.width(), base.width(), "{base:?} {a:?} -> {p:?}");
                    assert!(rich || p.is_ascii() || !base.is_ascii(), "{p:?}");
                }
            }
        }
    }

    #[test]
    fn tools_map_to_what_they_do() {
        assert_eq!(Activity::of_tool("bash"), Activity::Shell);
        assert_eq!(Activity::of_tool("multi_edit"), Activity::Editing);
        assert_eq!(Activity::of_tool("grep"), Activity::Tool);
        assert_eq!(Activity::of_tool("mcp__fs__read"), Activity::Tool);
    }
}
