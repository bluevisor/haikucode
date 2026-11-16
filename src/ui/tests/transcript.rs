use super::*;

#[test]
fn clean_text_passes_plain_text_through_borrowed() {
    assert!(matches!(clean_text("hello world", true), std::borrow::Cow::Borrowed(_)));
    assert!(matches!(clean_text("héllo ❯ wörld", false), std::borrow::Cow::Borrowed(_)));
}

#[test]
fn clean_text_strips_escapes_and_expands_tabs() {
    // ANSI color codes from tool output must not reach the terminal — and
    // neither may their parameter bytes: `ls --color` emits `ESC [ 1 ; 31 m`
    // and dropping only the ESC leaves `[1;31m` on screen as literal text.
    assert_eq!(clean_text("\x1b[31mred\x1b[0m", false), "red");
    assert_eq!(clean_text("\x1b[1;31mERROR\x1b[0m: disk full", false), "ERROR: disk full");
    assert_eq!(clean_text("a\tb", false), "a    b");
    assert_eq!(clean_text("a\rb\x07", false), "ab");
}

#[test]
fn clean_text_eats_whole_escape_sequences() {
    // OSC title, two-character charset, and an unterminated trailing ESC.
    assert_eq!(clean_text("\x1b]0;title\x07x", false), "x");
    assert_eq!(clean_text("\x1b]0;t\x1b\\x", false), "x");
    assert_eq!(clean_text("\x1b(Bbold", false), "bold");
    assert_eq!(clean_text("dangling\x1b", false), "dangling");
    // A CSI without its final byte must not eat the rest of the line.
    assert_eq!(clean_text("\x1b[32mok", false), "ok");
}

/// A pasted snippet often carries raw escapes (copied out of a colored
/// terminal or a log file). Drawn into the frame they desync the terminal
/// from ratatui's cell grid, so every later cell comes out in the wrong
/// color until a full repaint. Control bytes must never reach the buffer —
/// except newlines and tabs, which are text (a pasted snippet keeps its
/// lines and its Makefile tabs); CRLF and lone CR become \n.
#[test]
fn pasted_control_characters_never_reach_the_composer() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.on_paste("safe \x1b[31mred\x1b[0m text\tend\r\nnext\rlast".to_string());
    assert_eq!(app.input, "safe [31mred[0m text\tend\nnext\nlast");
    assert_eq!(app.cursor, app.input.chars().count());
}

/// On a 16-color console the console ignores `ESC[38;2;…m`, so a theme's RGB
/// shades made text come out in whatever color was last in effect. The
/// adapted palette must contain no RGB left at all (and must stay usable:
/// readable text, a visible highlight band).
#[test]
fn a_16color_terminal_gets_a_palette_with_no_rgb_left_in_it() {
    use ratatui::style::Color;
    for theme in THEMES {
        let safe = crate::ui::palette::for_16color(palette_by_name(theme));
        for (name, c) in [
            ("accent", safe.accent),
            ("assistant", safe.assistant),
            ("assistant_glyph", safe.assistant_glyph),
            ("reasoning", safe.reasoning),
            ("tool", safe.tool),
            ("tool_result", safe.tool_result),
            ("notice", safe.notice),
            ("code", safe.code),
            ("heading", safe.heading),
            ("diff_add", safe.diff_add),
            ("diff_del", safe.diff_del),
            ("diff_ctx", safe.diff_ctx),
            ("error", safe.error),
            ("chrome", safe.chrome),
            ("secondary", safe.secondary),
        ] {
            assert!(
                !matches!(c, Color::Rgb(..)),
                "{theme}: {name} is still RGB on a 16-color terminal"
            );
        }
        assert_eq!(safe.user_bg, Color::DarkGray, "{theme}: band stays visible");
        // A truecolor terminal is left completely alone.
        let full = palette_for(theme);
        assert_eq!(full.assistant, palette_by_name(theme).assistant);
    }
    // The Rainbow theme's near-white still reads as a bright color, and the
    // dim gray still as a dim one: the snap must not collapse them together.
    let safe = crate::ui::palette::for_16color(palette_by_name("Rainbow"));
    assert_eq!(safe.assistant, Color::White);
    assert_eq!(safe.reasoning, Color::DarkGray);
}

#[test]
fn clean_text_ascii_replaces_non_single_width() {
    assert_eq!(clean_text("ok 🚀 漢", true), "ok ? ?");
    assert_eq!(clean_text("héllo", true), "héllo");
    assert_eq!(clean_text("🚀\t", false), "🚀    ");
}

/// The permission-mode binding is advertised as "shift+tab/ctrl+p" in the
/// status bar, so every spelling of it must cycle — in every mode. The
/// refactor once dropped this to a single `BackTab` arm inside `Idle`, which
/// silently broke Shift+Tab under the Kitty keyboard protocol (where it
/// arrives as Tab+SHIFT and fell through to Tab autocomplete) and Ctrl+P
/// everywhere, while the hint still promised both.
#[test]
fn shift_tab_and_ctrl_p_cycle_permissions_in_every_mode() {
    let cases: [(&str, KeyEvent); 3] = [
        ("BackTab (ANSI)", KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        ("Tab+SHIFT (Kitty)", KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
        ("Ctrl+P", KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
    ];
    // Idle, Busy (agent running) and an approval prompt all honour it.
    for mode in [Mode::Idle, Mode::Busy, Mode::Approval("edit x".into())] {
        for (name, key) in cases {
            let mut app = App::new(test_ui_config(), Vec::new());
            let (h, _rx) = test_handles();
            app.mode = mode.clone();
            app.on_key(key, &h);
            assert_eq!(
                app.perm(),
                crate::agent::PERM_AUTO,
                "{name} in {mode:?} must cycle ask → yolo"
            );
            assert_eq!(app.input, "", "{name} must not type into the composer");
        }
    }

    // A masked prompt and an ask_user question capture every key, so a Tab
    // or Ctrl+P there must not leak out as a mode change.
    for mode in [Mode::Password { prompt: "pw".into() }, Mode::Question { prompt: "q".into() }] {
        let mut app = App::new(test_ui_config(), Vec::new());
        let (h, _rx) = test_handles();
        app.mode = mode.clone();
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.perm(), 0, "{mode:?} must keep the captured key");
    }
}

/// Brief mode keeps thinking summaries, hides completed tool activity, keeps the
/// diff of a change awaiting approval visible, and Ctrl+T's flag brings the
/// full output back.
#[test]
fn brief_mode_summarizes_and_full_mode_shows_everything() {
    use crate::ui::types::Kind;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.transcript.clear();
    app.show_thinking = false;
    app.push(Kind::Reasoning, "Check the config loader first. Then the UI.".to_string());
    app.push(Kind::Tool, "read_file src/config.rs".to_string());
    app.push(Kind::ToolResult, "SECRET_TOOL_OUTPUT".to_string());
    app.push(Kind::Tool, "edit_file src/ui.rs".to_string());
    app.push(Kind::DiffAdd, "+ added_line".to_string());
    let mut term = Terminal::new(TestBackend::new(80, 16)).unwrap();

    app.mode = Mode::Approval("edit src/ui.rs".into());
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Check the config loader first."), "{screen}");
    assert!(!screen.contains("considered") && !screen.contains("explored"), "{screen}");
    assert!(!screen.contains("read 1 file"), "{screen}");
    assert!(!screen.contains("SECRET_TOOL_OUTPUT"), "{screen}");
    assert!(screen.contains("added_line"), "pending edit's diff stays visible:\n{screen}");

    app.mode = Mode::Idle;
    app.push(Kind::ToolResult, "OK".to_string());
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("changed") && !screen.contains("src/ui.rs"), "{screen}");
    assert!(!screen.contains("added_line"), "{screen}");

    app.show_thinking = true;
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("SECRET_TOOL_OUTPUT") && screen.contains("Then the UI."), "{screen}");
}

/// Committing live thinking (and the answer streaming after it) must not
/// move anything: the brief summary keeps the same text and screen line,
/// and the answer keeps its line too.
#[test]
fn committing_live_thinking_does_not_shift_the_screen() {
    use crate::ui::types::Kind;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let row_of = |screen: &str, needle: &str| screen.lines().position(|l| l.contains(needle));
    for show in [false, true] {
        for after_tool in [false, true] {
            let mut app = App::new(test_ui_config(), Vec::new());
            app.transcript.clear();
            app.show_thinking = show;
            app.push(Kind::User, "do it".to_string());
            if after_tool {
                app.push(Kind::Tool, "read_file a.rs".to_string());
                app.push(Kind::ToolResult, "ok".to_string());
            }
            app.live_reasoning = "Look at the banner first. Then wrap.".into();
            app.live = "Here is the answer.".into();
            let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
            let before = draw(&mut app, &mut term);
            app.flush_live();
            let after = draw(&mut app, &mut term);
            let thought = "Look at the banner first.";
            let committed = thought;
            let ctx = format!("show={show} after_tool={after_tool}\n--before--\n{before}\n--after--\n{after}");
            assert!(row_of(&before, thought).is_some(), "{ctx}");
            assert_eq!(row_of(&before, thought), row_of(&after, committed), "{ctx}");
            assert_eq!(row_of(&before, "Here is the answer."), row_of(&after, "Here is the answer."), "{ctx}");
        }
    }
}

#[test]
fn prompt_is_acknowledged_before_the_first_model_token() {
    use ratatui::{backend::TestBackend, Terminal};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.transcript.clear();
    app.show_thinking = false;
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    app.send_prompt("hi".into(), &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Waiting for model  [.      ]"), "{screen}");
    app.spin_counter = 30;
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Waiting for model  [   ... ]"), "{screen}");
    app.handle_event(UiEvent::Reasoning("A greeting.".into()), &h);
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("Waiting for model") && screen.contains("A greeting."), "{screen}");
    app.handle_event(UiEvent::TurnDone, &h);
    app.send_prompt("hello again".into(), &h);
    app.handle_event(UiEvent::Token("Hello!".into()), &h);
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("Waiting for model") && screen.contains("Hello!"), "{screen}");
    app.handle_event(UiEvent::TurnDone, &h);
    app.send_prompt("cancel this".into(), &h);
    app.clear_busy();
    assert!(!draw(&mut app, &mut term).contains("Waiting for model"));
    assert!(!app.transcript.iter().any(|t| t.text.contains("Waiting for model")));
}

#[test]
fn current_activity_is_replaced_and_removed_without_losing_thoughts() {
    use ratatui::{backend::TestBackend, Terminal};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.transcript.clear();
    app.show_thinking = false;
    app.set_busy();
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    app.handle_event(UiEvent::Reasoning("Check the renderer first.".into()), &h);
    app.handle_event(UiEvent::ToolStart { name: "read_file".into(), summary: "src/ui.rs".into() }, &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Check the renderer first.") && screen.contains("Reading src/ui.rs"), "{screen}");
    app.handle_event(UiEvent::Progress("Checking layout".into()), &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Checking layout") && !screen.contains("Reading src/ui.rs"), "{screen}");
    app.handle_event(UiEvent::ToolResult { ok: true, preview: "ok".into() }, &h);
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("Checking layout"), "{screen}");
    app.handle_event(UiEvent::ToolStart { name: "bash".into(), summary: "cargo test".into() }, &h);
    app.handle_event(UiEvent::TurnDone, &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Check the renderer first."), "{screen}");
    assert!(!screen.contains("bash cargo test") && !screen.contains("considered"), "{screen}");
    app.show_thinking = true;
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("read_file src/ui.rs") && screen.contains("bash cargo test"), "{screen}");
}

#[test]
fn brief_activity_updates_in_place_and_disappears_after_the_turn() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.transcript.clear();
    app.show_thinking = false;
    let (h, _rx) = test_handles();
    let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
    app.set_busy();
    app.handle_event(UiEvent::Reasoning("Check the configuration. Then test it.".into()), &h);
    app.handle_event(UiEvent::ToolStart { name: "read_file".into(), summary: "src/config.rs".into() }, &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Check the configuration.") && screen.contains("Reading src/config.rs"), "{screen}");
    assert!(!screen.contains("considered") && !screen.contains("explored"), "{screen}");

    app.handle_event(UiEvent::ToolResult { ok: true, preview: "PRIVATE_TOOL_OUTPUT".into() }, &h);
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("src/config.rs") && !screen.contains("PRIVATE_TOOL_OUTPUT"), "{screen}");
    assert!(screen.contains("Thinking"), "{screen}");

    app.handle_event(UiEvent::Progress("running 3 read-only calls in parallel…".into()), &h);
    assert!(draw(&mut app, &mut term).contains("running 3 read-only calls"));
    app.handle_event(UiEvent::Token("Done.".into()), &h);
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("running 3 read-only calls") && screen.contains("Writing response"), "{screen}");

    app.handle_event(UiEvent::ToolStart { name: "edit_file".into(), summary: "src/ui.rs".into() }, &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Editing src/ui.rs") && !screen.contains("Reading src/config.rs"), "{screen}");
    // Finishing (including an interrupted tool) clears the live action.
    app.handle_event(UiEvent::TurnDone, &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Check the configuration.") && screen.contains("Done."), "{screen}");
    assert!(!screen.contains("Editing") && !screen.contains("src/ui.rs"), "{screen}");

    app.show_thinking = true;
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("PRIVATE_TOOL_OUTPUT") && screen.contains("Then test it."), "{screen}");
}

#[test]
fn current_activity_is_sanitized_and_fits_the_status_line() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.set_busy();
    app.current_activity = Some(format!("Running \x1b[31mred\x1b[0m\n{}", "文件".repeat(60)));
    for width in [0, 1, 4, 25, 60, 80, 120] {
        let status = app.working_status(width);
        assert!(!status.contains('\x1b') && !status.contains('\n'), "{status:?}");
        assert!(app.str_w(&status) <= width, "{width}: {status}");
    }
}

/// A tool result with newlines keeps its line structure. The transcript used
/// to hand the whole multi-line string to the wrapper, which turned every
/// newline into a space and reflowed `ls`/stack-trace output into one
/// garbled paragraph.
#[test]
fn multi_line_tool_output_keeps_its_lines() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.show_thinking = true;
    app.transcript.clear();
    app.push(
        crate::ui::types::Kind::ToolResult,
        "total 4\n-rw-r--r-- 1 a b\nroot -> /private/root".to_string(),
    );
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("total 4"), "{screen}");
    assert!(screen.contains("-rw-r--r-- 1 a b"), "second line survived:\n{screen}");
    assert!(screen.contains("root -> /private/root"), "third line survived:\n{screen}");
    assert!(
        !screen.contains("total 4 -rw-r--r--"),
        "lines must not be reflowed together:\n{screen}"
    );
}

/// The user's own prompt is echoed into the transcript, tagged so it draws
/// with the full-width band that makes it scannable while scrolling back.
#[test]
fn sending_a_message_echoes_the_prompt_into_the_transcript() {
    let (mut app, h) = scrollable_app();
    app.mode = Mode::Idle;
    app.input = "rename the field".to_string();
    app.cursor = app.input.chars().count();
    app.submit(&h);
    let echoed = app
        .transcript
        .iter()
        .find(|l| crate::ui::types::Kind::User == l.kind)
        .expect("the prompt is shown");
    assert_eq!(echoed.text, "rename the field");
    assert!(matches!(app.mode, Mode::Busy), "the turn started");
}

#[test]
fn bang_prefix_runs_a_shell_command_instead_of_prompting_the_model() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.mode = Mode::Idle;
    app.input = "!git status".to_string();
    app.cursor = app.input.chars().count();
    app.submit(&h);
    match rx.try_recv() {
        Ok(WorkerCmd::Shell(cmd)) => assert_eq!(cmd, "git status"),
        other => panic!("expected Shell, got {}", if other.is_ok() { "another command" } else { "nothing" }),
    }
    let echoed = app.transcript.iter().find(|l| l.kind == crate::ui::types::Kind::User).expect("echoed");
    assert_eq!(echoed.text, "! git status");
    assert!(matches!(app.mode, Mode::Busy));
    // A bare "!" is just text.
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.mode = Mode::Idle;
    app.input = "!".to_string();
    app.cursor = 1;
    app.submit(&h);
    assert!(matches!(rx.try_recv(), Ok(WorkerCmd::User { .. })));
}

#[test]
fn the_approval_prompt_offers_a_pattern_and_p_persists_it() {
    use std::sync::mpsc;
    let mut app = App::new(test_ui_config(), Vec::new());
    let (mut h, _rx) = test_handles();
    let (appr_tx, appr_rx) = mpsc::channel();
    h.appr_tx = appr_tx;
    app.handle_event(
        UiEvent::Approval { desc: "run: cargo test".into(), rule: Some("bash(cargo test:*)".into()) },
        &h,
    );
    assert!(matches!(app.mode, Mode::Approval(_)));
    // The offer is visible in the rendered prompt.
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("don't ask again for bash(cargo test:*)"), "{screen}");
    app.on_key(key(KeyCode::Char('p')), &h);
    match appr_rx.try_recv() {
        Ok(crate::agent::ApprovalResponse::Rule(r)) => assert_eq!(r, "bash(cargo test:*)"),
        _ => panic!("expected a Rule response"),
    }
    assert!(matches!(app.mode, Mode::Busy));
    // Without an offered rule, P does nothing and the prompt stays up.
    let mut app = App::new(test_ui_config(), Vec::new());
    app.handle_event(UiEvent::Approval { desc: "call MCP tool x".into(), rule: None }, &h);
    app.on_key(key(KeyCode::Char('p')), &h);
    assert!(matches!(app.mode, Mode::Approval(_)));
    assert!(appr_rx.try_recv().is_err());
}

#[test]
fn custom_slash_commands_expand_into_a_prompt_and_show_in_the_palette() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.custom_cmds = vec![crate::commands::CustomCommand {
        name: "fix".into(),
        description: "Fix an issue".into(),
        body: "Fix issue $ARGUMENTS in this repo.".into(),
        source: std::path::PathBuf::from("/x/fix.md"),
    }];
    app.custom_palette = vec![("/fix", "Fix an issue")];
    app.mode = Mode::Idle;
    app.input = "/fi".to_string();
    assert!(app.slash_suggestions().iter().any(|(c, _)| *c == "/fix"));
    app.input = "/fix 42".to_string();
    app.cursor = app.input.chars().count();
    app.submit(&h);
    match rx.try_recv() {
        Ok(WorkerCmd::User { text, .. }) => assert_eq!(text, "Fix issue 42 in this repo."),
        _ => panic!("expected a User turn"),
    }
    // Unknown commands still report as unknown.
    app.mode = Mode::Idle;
    app.input = "/nope".to_string();
    app.cursor = 5;
    app.submit(&h);
    assert!(app.transcript.iter().any(|l| l.kind == crate::ui::types::Kind::ErrorK && l.text.contains("unknown command")));
}

/// The transcript separates blocks with blank lines so turns, prose, and
/// tool activity don't run together — but only at real boundaries: a notice
/// or a tool result that belongs to the preceding block stays attached.
#[test]
fn transcript_inserts_blank_lines_between_blocks() {
    use crate::ui::needs_sep;
    use crate::ui::types::Kind;

    // A new prompt always opens a fresh, roomy block…
    assert!(needs_sep(Kind::Assistant, Kind::User));
    assert!(needs_sep(Kind::ToolResult, Kind::User));
    // …but two prompts in a row are one block, and the banner pads itself.
    assert!(!needs_sep(Kind::User, Kind::User));
    assert!(!needs_sep(Kind::Banner, Kind::User));

    // Prose → tools, and tools → the reply that follows, both get a gap.
    assert!(needs_sep(Kind::Assistant, Kind::Tool));
    assert!(needs_sep(Kind::ToolResult, Kind::Tool));
    assert!(needs_sep(Kind::ToolResult, Kind::Assistant));
    assert!(needs_sep(Kind::Reasoning, Kind::Assistant));

    // A notice or a result never detaches itself from its block.
    assert!(!needs_sep(Kind::Assistant, Kind::Notice));
    assert!(!needs_sep(Kind::Tool, Kind::ToolResult));
    assert!(!needs_sep(Kind::Assistant, Kind::Assistant));
}

/// The whole point of the separator: pushing a prompt after a reply leaves a
/// blank line in the transcript, and the renderer draws it as an empty row.
#[test]
fn committed_blocks_are_separated_by_a_blank_row() {
    use crate::ui::types::Kind;
    let mut app = App::new(test_ui_config(), Vec::new());
    app.push(Kind::Assistant, "first answer");
    app.push(Kind::User, "second question");
    let kinds: Vec<Kind> = app.transcript.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![Kind::Assistant, Kind::Blank, Kind::User],
        "a blank line lands between the turn's answer and the next prompt"
    );
    // The blank row renders as a genuinely empty line, not stray padding.
    app.ensure_display_cache(40);
    let blank = app
        .disp_cache
        .iter()
        .find(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
        .expect("the separator renders as an empty row");
    assert!(blank.spans.iter().all(|s| s.content.is_empty()));
}
