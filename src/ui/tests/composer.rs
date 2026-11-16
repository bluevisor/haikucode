use super::*;

#[test]
fn composer_layout_wraps_and_places_the_cursor_by_display_width() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.single_width = false;
    // 日本語 are 2 cells each: 日本 (4) fits in 5, 語 (2) would overflow → wraps.
    app.input = "日本語abc".to_string();
    app.cursor = app.char_len();
    let l = app.layout_input(5);
    // Cursor after the last char of a full row spills onto a fresh row.
    assert_eq!(l.rows, vec!["日本", "語abc", ""]);
    assert_eq!((l.cur_row, l.cur_col, l.cur_idx), (2, 0, 0));
    // Cursor on 語: row 1, column 0 — not char-index 2 % 5 = column 2.
    app.cursor = 2;
    let l = app.layout_input(5);
    assert_eq!((l.cur_row, l.cur_col, l.cur_idx), (1, 0, 0));
    // Cursor on `c`: 語 took 2 cells, so column 4, chars-into-row 3.
    app.cursor = 5;
    let l = app.layout_input(5);
    assert_eq!((l.cur_row, l.cur_col, l.cur_idx), (1, 4, 3));
    // Row count matches the layout: two rows with the cursor mid-text,
    // three once it sits past the full last row.
    assert_eq!(app.input_rows(5 + 2 + app.prompt_w() as u16), 2);
    app.cursor = app.char_len();
    assert_eq!(app.input_rows(5 + 2 + app.prompt_w() as u16), 3);
    // Plain ASCII behaves exactly as the old char arithmetic did.
    app.input = "abcdefgh".to_string();
    app.cursor = 8;
    let l = app.layout_input(4);
    assert_eq!(l.rows, vec!["abcd", "efgh", ""]);
    assert_eq!((l.cur_row, l.cur_col), (2, 0));
    app.cursor = 6;
    assert_eq!(app.layout_input(4).cur_col, 2);
}

#[test]
fn the_caret_lands_on_the_glyph_after_wide_characters() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.single_width = false;
    app.palette.cursor = crate::ui::types::CursorKind::Caret;
    app.mode = Mode::Idle;
    app.input = "日本a".to_string();
    app.cursor = 2; // on `a`, which sits in cell 4 after two double-width glyphs
    let backend = ratatui::backend::TestBackend::new(60, 12);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let pos = term.get_cursor_position().unwrap();
    // pad1 insets the composer by one column; the prompt precedes the text.
    assert_eq!(pos.x as usize, 1 + app.prompt_w() + 4);
}

#[test]
fn single_width_terminals_get_a_placeholder_for_wide_glyphs() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.single_width = true;
    app.on_paste("a日b".to_string());
    assert_eq!(app.input, "a?b");
    assert_eq!(app.cell_w('日'), 1);
}

#[test]
fn arrows_recall_history_and_ctrl_arrows_roll_the_transcript() {
    let (mut app, h) = scrollable_app();

    // Plain Up/Down walk the prompt history, like a shell; the view stays.
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.input, "second", "Up recalls the previous prompt");
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.input, "first");
    assert_eq!(app.scroll, 40, "history recall doesn't move the view");
    app.on_key(key(KeyCode::Down), &h);
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.input, "", "Down past the newest returns to the empty draft");

    // Ctrl+Up/Down move the view instead.
    app.on_key(ctrl(KeyCode::Up), &h);
    assert_eq!(app.scroll, 37, "Ctrl+Up rolls the output back a step");
    assert!(!app.follow, "scrolling up pins the view");
    assert!(app.scrolled_up, "the more-below hint shows");
    assert_eq!(app.input, "", "Ctrl+Up must not type history into the composer");

    app.on_key(ctrl(KeyCode::Down), &h);
    assert_eq!(app.scroll, 40);
    assert!(app.follow, "returning to the live end resumes following");
    assert!(!app.scrolled_up);

    app.on_key(ctrl(KeyCode::Up), &h);
    assert!(app.scrolled_up);
    assert!(!app.new_below, "a manual scroll-up has nothing new below");

    // New output while pinned flags the hint as "new" and leaves the
    // user's reading position alone.
    app.handle_event(UiEvent::Token("streaming".into()), &h);
    app.handle_event(UiEvent::AssistantCommit, &h);
    assert!(app.new_below, "arriving output is flagged as new");
    assert!(!app.follow, "...without yanking the view to the bottom");
    assert_eq!(app.scroll, 37, "the reading position is preserved");

    app.on_key(ctrl(KeyCode::Down), &h);
    assert!(app.follow);
    assert!(!app.scrolled_up && !app.new_below, "hint clears at the end");
}

#[test]
fn scrolling_clamps_at_both_ends_and_never_pins_an_unscrollable_view() {
    let (mut app, h) = scrollable_app();
    for _ in 0..30 {
        app.on_key(ctrl(KeyCode::Down), &h);
    }
    assert_eq!(app.scroll, 40, "clamped at the live end");
    assert!(app.follow);

    for _ in 0..30 {
        app.on_key(ctrl(KeyCode::Up), &h);
    }
    assert_eq!(app.scroll, 0, "clamped at the top");
    assert!(!app.follow);

    // Short transcript: nothing above the fold, so Up is a no-op and we
    // don't tease a scroll that can't happen.
    app.max_top = 0;
    app.follow = true;
    app.scrolled_up = false;
    app.new_below = false;
    app.on_key(ctrl(KeyCode::Up), &h);
    assert_eq!((app.scroll, app.follow, app.scrolled_up), (0, true, false));
}

#[test]
fn page_keys_move_a_full_screenful() {
    let (mut app, h) = scrollable_app();
    app.on_key(key(KeyCode::PageUp), &h);
    assert_eq!(app.scroll, 30, "PgUp moves one viewport (view_h = 10)");
    app.on_key(key(KeyCode::PageDown), &h);
    assert_eq!(app.scroll, 40);
    assert!(app.follow);
}

#[test]
fn sending_a_message_snaps_back_to_the_live_end() {
    let (mut app, h) = scrollable_app();
    app.on_key(ctrl(KeyCode::Up), &h);
    app.on_key(ctrl(KeyCode::Up), &h);
    assert!(!app.follow);

    for c in "hello".chars() {
        app.on_key(key(KeyCode::Char(c)), &h);
    }
    app.on_key(key(KeyCode::Enter), &h);
    assert!(app.follow, "Enter returns to the live end for the reply");
    assert!(!app.scrolled_up, "and drops the new-output hint");
    assert!(!app.new_below);
}

#[test]
fn scrolling_works_while_the_agent_is_busy() {
    let (mut app, h) = scrollable_app();
    app.mode = Mode::Busy;
    app.on_key(ctrl(KeyCode::Up), &h);
    assert_eq!(app.scroll, 37, "queued-input mode scrolls too");
    assert!(!app.follow);
    assert!(matches!(app.mode, Mode::Busy), "still busy, not interrupted");
}

/// The composer carries the readline/Emacs line-editing shortcuts: Ctrl+A/E
/// (start/end), Ctrl+K/U (kill to end/start), Ctrl+W (kill word), Ctrl+Y
/// (yank it back), Ctrl+B/F and Alt+B/F (char/word motion), Ctrl+D
/// (delete-char, quitting only on an empty line), Alt+D (kill word
/// forward) — and Ctrl+K/U must not eat the newline that ends the line.
#[test]
fn composer_accepts_readline_shortcuts() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    typed(&mut app, &h, "one two three");

    app.on_key(ctrl(KeyCode::Char('a')), &h);
    assert_eq!(app.cursor, 0, "Ctrl+A goes to the start of the line");
    app.on_key(ctrl(KeyCode::Char('e')), &h);
    assert_eq!(app.cursor, 13, "Ctrl+E goes to the end of the line");

    app.on_key(ctrl(KeyCode::Char('b')), &h);
    app.on_key(ctrl(KeyCode::Char('b')), &h);
    assert_eq!(app.cursor, 11, "Ctrl+B moves back a char");
    app.on_key(ctrl(KeyCode::Char('f')), &h);
    assert_eq!(app.cursor, 12, "Ctrl+F moves forward a char");

    // Alt+B/F move by word (they used to type a bare 'b'/'f').
    app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT), &h);
    assert_eq!(app.cursor, 8, "Alt+B lands on the start of \"three\"");
    assert_eq!(app.input, "one two three", "Alt+B doesn't type its letter");
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT), &h);
    assert_eq!(app.cursor, 13);

    // Ctrl+K kills to the end of the line, Ctrl+U back to its start, and
    // Ctrl+Y puts the last kill back at the cursor.
    app.on_key(ctrl(KeyCode::Char('b')), &h);
    app.on_key(ctrl(KeyCode::Char('b')), &h);
    app.on_key(ctrl(KeyCode::Char('k')), &h);
    assert_eq!(app.input, "one two thr", "Ctrl+K kills to the end of the line");
    app.on_key(ctrl(KeyCode::Char('u')), &h);
    assert_eq!(app.input, "", "Ctrl+U kills back to the start");
    app.on_key(ctrl(KeyCode::Char('y')), &h);
    assert_eq!(app.input, "one two thr", "Ctrl+Y yanks the last kill");

    // Ctrl+W kills the word before the cursor.
    app.on_key(ctrl(KeyCode::Char('w')), &h);
    assert_eq!(app.input, "one two ", "Ctrl+W kills the word before");
    app.on_key(ctrl(KeyCode::Char('y')), &h);
    assert_eq!(app.input, "one two thr");

    // Alt+D kills the word after the cursor.
    app.on_key(ctrl(KeyCode::Char('a')), &h);
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT), &h);
    assert_eq!(app.input, "two thr", "Alt+D kills the word forward");

    // Lines are separate: Ctrl+K stops at the newline, Ctrl+D joins them.
    app.input = "abc\ndef".into();
    app.cursor = 1;
    app.on_key(ctrl(KeyCode::Char('k')), &h);
    assert_eq!(app.input, "a\ndef", "Ctrl+K stops at the end of the line");
    app.on_key(ctrl(KeyCode::Char('d')), &h);
    assert_eq!(app.input, "adef", "Ctrl+D deletes the newline, joining the lines");

    // Ctrl+D on a non-empty line edits instead of arming the quit; an
    // empty line still quits on the second press.
    app.cursor = 0;
    app.on_key(ctrl(KeyCode::Char('d')), &h);
    assert_eq!(app.input, "def");
    assert!(!app.should_quit(), "a delete must not quit");
    app.input.clear();
    app.cursor = 0;
    app.last_ctrl_c = None;
    app.on_key(ctrl(KeyCode::Char('d')), &h);
    assert!(!app.should_quit(), "one press only arms the quit");
    assert!(app.last_ctrl_c.is_some(), "the empty line does arm it");
}

/// The word-delete spellings: Alt+Backspace / Alt+Delete and the Ctrl
/// variants (the Alt-free route on Haiku, where Alt drives the menus) both
/// cut a whole word and leave it in the kill ring for Ctrl+Y.
#[test]
fn modifier_word_delete_works_with_alt_and_ctrl() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    typed(&mut app, &h, "alpha beta gamma");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT), &h);
    assert_eq!(app.input, "alpha beta ", "Alt+Backspace deletes the word before");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL), &h);
    assert_eq!(app.input, "alpha ", "Ctrl+Backspace does the same");
    app.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT), &h);
    assert_eq!(app.input, "alpha ", "nothing to the right of the end");
    app.on_key(ctrl(KeyCode::Char('a')), &h);
    app.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL), &h);
    assert_eq!(app.input, "", "Ctrl+Delete deletes the word after the cursor");
    app.on_key(ctrl(KeyCode::Char('y')), &h);
    assert_eq!(app.input, "alpha ", "and Ctrl+Y yanks it back");
}

/// Ctrl+D keeps readline's meaning (delete-char) while a turn is running
/// and there is a draft to edit — Esc and Ctrl+C already interrupt, so
/// interrupting on Ctrl+D too would only cost the edit. An empty line is
/// still an interrupt, as it has always been.
#[test]
fn ctrl_d_deletes_under_the_cursor_while_busy_too() {
    let (mut app, h) = scrollable_app();
    app.mode = Mode::Busy;
    typed(&mut app, &h, "abc");
    app.cursor = 1;
    app.on_key(ctrl(KeyCode::Char('d')), &h);
    assert_eq!(app.input, "ac", "Ctrl+D is delete-char, not interrupt, with a draft");
    assert!(matches!(app.mode, Mode::Busy), "the turn keeps running");
    app.input.clear();
    app.cursor = 0;
    app.on_key(ctrl(KeyCode::Char('d')), &h);
    assert!(matches!(app.mode, Mode::Idle), "an empty line still interrupts");
}

/// A newline key never cuts a word in two: mid-word (Shift+Enter, Ctrl+J or
/// a trailing backslash) it does nothing at all, while at a word boundary —
/// after a space, at either end of the line — it breaks as usual.
#[test]
fn a_newline_key_never_splits_a_word() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    typed(&mut app, &h, "foo bar");
    app.cursor = 2; // fo|o
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &h);
    assert_eq!(app.input, "foo bar", "Shift+Enter inside a word adds no line");
    app.on_key(ctrl(KeyCode::Char('j')), &h);
    assert_eq!(app.input, "foo bar", "nor does Ctrl+J");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &h);
    assert_eq!(app.input, "foo bar", "nor Alt+Enter");

    // A path or an identifier is one word, punctuation and all.
    app.input = "see src/main.rs".into();
    app.cursor = 8; // src/|main.rs
    app.on_key(ctrl(KeyCode::Char('j')), &h);
    assert_eq!(app.input, "see src/main.rs", "a path isn't split either");

    // A space is a boundary: the break goes in.
    app.cursor = 4; // see |
    app.on_key(ctrl(KeyCode::Char('j')), &h);
    assert_eq!(app.input, "see \nsrc/main.rs", "after a space the line breaks");
    // So is either end of the line.
    app.cursor = app.char_len();
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &h);
    assert_eq!(app.input, "see \nsrc/main.rs\n", "and at the end of the line");
    app.cursor = 0;
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL), &h);
    assert_eq!(app.input, "\nsee \nsrc/main.rs\n", "and at the start");

    // The backslash idiom: mid-word nothing happens and the backslash
    // stays put (it isn't eaten for nothing); at a boundary it becomes the
    // break.
    app.input = "ab\\".into();
    app.cursor = 3;
    app.on_key(key(KeyCode::Enter), &h);
    assert_eq!(app.input, "ab\n", "a trailing backslash plus Enter breaks the line");
    app.input = "fo\\o".into(); // the backslash sits inside the word
    app.cursor = 3;
    app.on_key(key(KeyCode::Enter), &h);
    assert_eq!(app.input, "fo\\o", "mid-word the backslash is left alone");

    // A paste keeps its own newlines: it is deliberate, whole text.
    app.on_paste("x\ny".into());
    assert!(app.input.contains("x\ny"));
}

/// The composer wraps at a word boundary: a word that doesn't fit moves
/// whole to the next row instead of being cut in half, and the rows still
/// cover the text so the caret and Up/Down stay exact.
#[test]
fn composer_wraps_at_word_boundaries() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.single_width = true;
    app.input = "hello world foo".into();
    app.cursor = app.char_len();
    let l = app.layout_input(10);
    assert_eq!(l.rows, vec!["hello ", "world foo"], "the word moves down, not in half");
    assert_eq!(l.starts, vec![0, 6], "rows still tile the text exactly");
    assert_eq!((l.cur_row, l.cur_idx), (1, 9), "the caret follows the text");

    // A word wider than a row has no boundary to break at: it is split.
    app.input = "abcdefgh".into();
    app.cursor = 6;
    assert_eq!(app.layout_input(4).rows, vec!["abcd", "efgh"]);
    app.input = "aaa bbbbb".into();
    app.cursor = 7;
    let l = app.layout_input(5);
    assert_eq!(l.rows, vec!["aaa ", "bbbbb"], "the whole word takes the next row");
    assert_eq!((l.cur_row, l.cur_idx), (1, 3));
    app.cursor = 4; // the first char of the moved word
    let l = app.layout_input(5);
    assert_eq!((l.cur_row, l.cur_col, l.cur_idx), (1, 0, 0));

    // Indented text with no room left: the indent stays, the word moves.
    app.input = "0123     deed".into();
    app.cursor = app.char_len();
    let l = app.layout_input(10);
    assert_eq!(l.rows, vec!["0123     ", "deed"]);
}

/// The readline keys edit the queued draft while the agent is running too;
/// only Esc, Ctrl+C and Ctrl+D on an empty line interrupt there.
#[test]
fn readline_shortcuts_work_while_busy() {
    let (mut app, h) = scrollable_app();
    app.mode = Mode::Busy;
    typed(&mut app, &h, "queued draft");
    app.on_key(ctrl(KeyCode::Char('a')), &h);
    assert_eq!(app.cursor, 0);
    app.on_key(ctrl(KeyCode::Char('e')), &h);
    assert_eq!(app.cursor, 12);
    app.on_key(ctrl(KeyCode::Char('w')), &h);
    assert_eq!(app.input, "queued ", "Ctrl+W edits the queued draft");
    assert!(matches!(app.mode, Mode::Busy), "and doesn't interrupt the turn");
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), &h);
    assert_eq!(app.cursor, 0, "Ctrl+← moves by word while busy too");
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL), &h);
    assert_eq!(app.cursor, 7, "and Ctrl+→");
}

/// Ctrl+C must work with a draft in the composer. It used to be swallowed:
/// the `Char` arm's CONTROL guard rejected it, so nothing happened at all —
/// no clear, no quit, no way out of the TUI short of killing the process.
#[test]
fn ctrl_c_clears_a_draft_line_and_quits_on_the_second_press() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    app.on_key(key(KeyCode::Char('h')), &h);
    app.on_key(key(KeyCode::Char('i')), &h);
    assert_eq!(app.input, "hi");

    // First press: the line goes, the app stays (and isn't a history entry).
    app.on_key(ctrl_c, &h);
    assert_eq!(app.input, "");
    assert_eq!(app.cursor, 0);
    assert!(!app.should_quit(), "one press must not quit");
    assert!(app.history.is_empty(), "a dropped draft isn't a command");

    // Second press inside the double-press window: quit.
    app.on_key(ctrl_c, &h);
    assert!(app.should_quit());
}

/// Every newline key inserts a line instead of sending: Shift/Alt+Enter,
/// Ctrl+J, and a trailing backslash + Enter (the backslash is consumed).
/// Plain Enter then sends the whole draft, newlines intact, and the worker
/// gets the typed text for /rewind.
#[test]
fn newline_keys_build_a_multi_line_prompt_and_enter_sends_it() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    typed(&mut app, &h, "one");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &h);
    typed(&mut app, &h, "two");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &h);
    typed(&mut app, &h, "three");
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL), &h);
    typed(&mut app, &h, "four\\");
    app.on_key(key(KeyCode::Enter), &h);
    typed(&mut app, &h, "five");
    assert_eq!(app.input, "one\ntwo\nthree\nfour\nfive");
    assert!(rx.try_recv().is_err(), "nothing sent yet");
    app.on_key(key(KeyCode::Enter), &h);
    match rx.try_recv() {
        Ok(WorkerCmd::User { text, typed, .. }) => {
            assert_eq!(text, "one\ntwo\nthree\nfour\nfive");
            assert_eq!(typed, text, "/rewind gets the whole prompt back");
        }
        _ => panic!("Enter sends the draft"),
    }
    assert_eq!(app.history.last().map(String::as_str), Some("one\ntwo\nthree\nfour\nfive"));
}

#[test]
fn newlines_and_tabs_lay_out_as_hard_rows() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.input = "ab\n\tc\n".into();
    app.cursor = 4; // just after the tab
    let lay = app.layout_input(20);
    assert_eq!(lay.rows, vec!["ab", "\tc", ""]);
    assert_eq!(lay.starts, vec![0, 3, 6]);
    assert_eq!((lay.cur_row, lay.cur_col, lay.cur_idx), (1, TAB_W, 1), "a tab is TAB_W cells");
    app.cursor = 6; // after the trailing newline: the empty last row
    let lay = app.layout_input(20);
    assert_eq!((lay.cur_row, lay.cur_col), (2, 0));
}

/// Up/Down move between the draft's lines (keeping the column when they
/// can); on the first/last line they walk the prompt history.
#[test]
fn up_down_walk_the_draft_then_the_history() {
    let (mut app, h) = scrollable_app();
    app.input = "first line\nsecond\nthird line here".into();
    app.cursor = app.char_len(); // end of "third line here"
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.cursor, "first line\nsecond".chars().count(), "clamped to the shorter line's end");
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.cursor, 6, "column 6 on the first line");
    let before = app.scroll;
    let draft = app.input.clone();
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.input, "second", "Up on the first line recalls the previous prompt…");
    assert_eq!(app.scroll, before, "…without scrolling the transcript");
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.input, draft, "Down past the newest brings the draft back");
    app.cursor = 6; // column 6 on the first line again
    app.on_key(key(KeyCode::Down), &h);
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.cursor, "first line\nsecond\n".chars().count() + 6);
    // Home/End stay on the cursor's line.
    app.on_key(key(KeyCode::Home), &h);
    assert_eq!(app.cursor, "first line\nsecond\n".chars().count());
    app.on_key(key(KeyCode::End), &h);
    assert_eq!(app.cursor, app.char_len());
}

/// A one-line draft wraps across rows by display width; Up/Down follow the
/// wrapped rows too, never landing on the next row's first char.
#[test]
fn up_down_follow_soft_wrapped_rows() {
    let (mut app, h) = scrollable_app();
    app.term_w = (2 + app.prompt_w() + 10) as u16; // 10 cells of text
    app.input = "abcdefghij0123456789xyz".into();
    app.cursor = app.char_len(); // on the third row, after "xyz"
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.cursor, 13, "row 2, column 3");
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.cursor, 3, "row 1, column 3");
    app.cursor = 9; // end of the first (full) row
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.cursor, 19, "last char of row 2, not the start of row 3");
}

/// The things that read the draft cope with newlines: a newline ends the
/// `/` palette (it's not a bare command any more), and Tab completes an
/// @path on a later line.
#[test]
fn slash_palette_and_at_completion_handle_newlines() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.input = "/mo".into();
    app.cursor = 3;
    assert!(!app.slash_suggestions().is_empty());
    app.input = "/mo\nmore".into();
    assert!(app.slash_suggestions().is_empty());
    let dir = std::env::temp_dir().join(format!("haikucode_ml_tab_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("shader.glsl"), "").unwrap();
    let (h, _rx) = test_handles();
    app.input = format!("look at\n@{}/sha", dir.display());
    app.cursor = app.char_len();
    app.on_key(key(KeyCode::Tab), &h);
    assert_eq!(app.input, format!("look at\n@{}/shader.glsl", dir.display()));
    std::fs::remove_dir_all(&dir).ok();
}

/// While busy, a multi-line queued message is one row (its first line and
/// a count), and a multi-line draft draws one row per line, tabs as spaces.
/// The prompt's marker tracks a turn: thinking, writing, a shell command,
/// an approval, then back to the theme's own prompt.
#[test]
fn the_prompt_marker_follows_the_agents_activity() {
    use crate::ui::activity::Activity;
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    let idle = app.prompt_str();
    app.mode = Mode::Busy;
    assert_eq!(app.activity(), Activity::Thinking);
    app.handle_event(UiEvent::Token("Let me look.".into()), &h);
    assert_eq!(app.activity(), Activity::Writing);
    app.handle_event(UiEvent::ToolStart { name: "bash".into(), summary: "ls".into() }, &h);
    assert_eq!(app.activity(), Activity::Shell);
    assert_ne!(app.prompt_str(), idle);
    assert_eq!(app.str_w(&app.prompt_str()), app.prompt_w(), "the marker never changes the width");
    app.handle_event(UiEvent::ToolResult { ok: true, preview: "a.rs".into() }, &h);
    assert_eq!(app.activity(), Activity::Thinking);
    app.handle_event(UiEvent::Approval { desc: "run: rm a.rs".into(), rule: None }, &h);
    assert_eq!(app.activity(), Activity::Asking);
    app.mode = Mode::Busy;
    app.handle_event(UiEvent::TurnDone, &h);
    assert_eq!(app.activity(), Activity::Idle);
    assert_eq!(app.prompt_str(), idle);
}

#[test]
fn busy_view_draws_multi_line_queue_and_draft() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new(test_ui_config(), Vec::new());
    app.transcript.clear();
    app.mode = Mode::Busy;
    app.queued.push("queued first\nqueued second\nqueued third".into());
    app.input = "draft one\n\tdraft two".into();
    app.cursor = app.char_len();
    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("queued: queued first (+2 lines)"), "{screen}");
    assert!(!screen.contains("queued second"), "{screen}");
    assert!(screen.contains("draft one"), "{screen}");
    assert!(screen.contains("    draft two"), "tab drawn as spaces:\n{screen}");
}

#[test]
fn a_long_prompt_is_echoed_in_part() {
    let short = "a\nb\nc";
    assert_eq!(echo_excerpt(short), short);
    let long: String = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let shown = echo_excerpt(&long);
    assert_eq!(shown.lines().count(), ECHO_LINES - 4 + 1);
    assert!(shown.ends_with("… (+80 more lines)"), "{shown}");
}

/// Tab completes an `@path` mid-sentence, at the cursor, keeping the text
/// around it (it used to work only when the whole line began with `@`).
#[test]
fn tab_completes_an_at_path_anywhere_in_the_line() {
    let dir = std::env::temp_dir().join(format!("haikucode_tab_mid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main_file.rs"), "").unwrap();
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.input = format!("fix @{}/mai please", dir.display());
    app.cursor = app.input.chars().count() - " please".chars().count();
    app.on_key(key(KeyCode::Tab), &h);
    assert_eq!(app.input, format!("fix @{}/main_file.rs please", dir.display()));
    assert_eq!(
        app.cursor,
        app.input.chars().count() - " please".chars().count(),
        "cursor lands after the completed path"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Esc clears the line too (as the README promises) and must never quit.
#[test]
fn esc_clears_a_draft_line_without_quitting() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.on_key(key(KeyCode::Char('x')), &h);
    app.on_key(key(KeyCode::Esc), &h);
    assert_eq!(app.input, "");
    assert!(!app.should_quit());
}

/// A long, multi-line ask_user question must come out whole in the panel.
/// It used to be one `Line`, so ratatui clipped it at the panel's right edge
/// (and an embedded newline in the prompt reached the frame as a stray
/// newline character) — the answer line is the only thing the user could
/// read, and they had to answer a question they never saw.
#[test]
fn a_long_question_is_wrapped_and_fully_visible() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.handle_event(
        UiEvent::Question {
            prompt: "Which database should I target?\nThe options are postgres or sqlite.".into(),
            reply: std::sync::mpsc::channel().0,
        },
        &h,
    );
    assert!(matches!(app.mode, Mode::Question { .. }));

    let mut term = Terminal::new(TestBackend::new(46, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("? Which database"), "question lead:\n{screen}");
    // The tail of the question survives: both its own newline and the width
    // wrap land on later rows instead of being cut off.
    assert!(
        screen.contains("sqlite."),
        "the whole question is on screen:\n{screen}"
    );

    // Typing keeps the answer on the row under the question, led by the
    // indent so it lines up with the question text.
    app.on_key(key(KeyCode::Char('p')), &h);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("  p"), "the answer line is indented:\n{screen}");

    // A question that would overflow the panel still can't eat the screen:
    // the transcript keeps its rows.
    app.note("transcript marker".to_string());
    app.handle_event(
        UiEvent::Question {
            prompt: (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
            reply: std::sync::mpsc::channel().0,
        },
        &h,
    );
    let screen = draw(&mut app, &mut term);
    assert!(
        screen.contains("transcript marker"),
        "the panel is capped:\n{screen}"
    );
}
