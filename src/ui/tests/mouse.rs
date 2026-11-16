use super::*;

#[test]
fn mouse_capture_defaults_to_on_everywhere_and_honours_the_override() {
    assert!(want_mouse_capture("iTerm.app", None));
    assert!(want_mouse_capture("", None));
    assert!(want_mouse_capture("WarpTerminal", None));

    assert!(want_mouse_capture("WarpTerminal", Some("1")));
    assert!(want_mouse_capture("WarpTerminal", Some("true")));
    assert!(want_mouse_capture("WarpTerminal", Some("ON")));
    assert!(!want_mouse_capture("iTerm.app", Some("0")));
    assert!(!want_mouse_capture("iTerm.app", Some("off")));
    // Unrecognised value: keep application mouse handling enabled.
    assert!(want_mouse_capture("WarpTerminal", Some("maybe")));
    assert!(want_mouse_capture("iTerm.app", Some("maybe")));
}

/// A drag is a range of screen cells in the output box: it can't start
/// outside it, and dragging past an edge stops at the edge (otherwise the
/// composer would get selected too).
#[test]
fn a_drag_selection_stays_inside_the_transcript_and_reads_forwards() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.output_area = Rect::new(2, 1, 20, 4);

    assert!(!app.select_begin(0, 0), "a press outside the box selects nothing");
    assert!(app.selection().is_none());

    assert!(app.select_begin(10, 3));
    assert!(app.select_drag(99, 99), "dragging past the corner still extends");
    assert_eq!(app.selection(), Some(((10, 3), (21, 4))), "clamped to the box's last cell");

    // Anchored at the bottom right, the same drag still reads top-left first.
    app.select_clear();
    assert!(app.select_begin(21, 4));
    assert!(app.select_drag(4, 1));
    assert_eq!(app.selection(), Some(((4, 1), (21, 4))));

    // A key gives the selection up.
    app.select_drag(4, 2);
    app.select_clear();
    assert!(app.selection().is_none());
}

/// A composer drag selects the input's characters, not the cells: tabs and
/// wide characters can't shift the result, and the prompt stays out of it.
#[test]
fn a_drag_in_the_composer_copies_input_text() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.mode = Mode::Idle;
    app.composer_area = Rect::new(0, 20, 60, 2);
    let pw = app.prompt_w() as u16;
    let row = 20;

    app.input = "git commit -m wip".to_string();
    assert_eq!(app.input_text((pw + 4, row), (pw + 9, row)), "commit");
    // Either direction is the same selection.
    assert_eq!(app.input_text((pw + 9, row), (pw + 4, row)), "commit");
    // The cell past a row's last character stops there, not past the end.
    assert_eq!(app.input_text((pw, row), (pw + 40, row)), "git commit -m wip");

    // A tab is one char in the buffer however many cells it draws over.
    app.input = "a\tb".to_string();
    assert_eq!(app.input_text((pw + 1, row), (pw + 2, row)), "\t");
    assert_eq!(app.input_text((pw, row), (pw + 5, row)), "a\tb");

    // Wide characters map to their own char, not to two cells of it.
    app.input = "世界 x".to_string();
    assert_eq!(app.input_text((pw, row), (pw + 3, row)), "世界");
    assert_eq!(app.input_text((pw + 5, row), (pw + 5, row)), "x");

    // A click in the prompt selects from the first input column.
    app.input = "abc".to_string();
    assert!(app.select_begin(0, row));
    let select = app.select.expect("a selection started");
    assert_eq!(select.kind, SelectKind::Composer);
    assert_eq!(select.anchor, (pw, row), "clamped past the prompt");
    assert_eq!(app.input_text(select.anchor, (pw + 2, row)), "abc");
}

/// A click (no drag) in the composer puts the caret where it landed, which
/// is what a text field is expected to do; the transcript ignores clicks.
#[test]
fn a_click_in_the_composer_places_the_caret() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.mode = Mode::Idle;
    app.input = "hello world".to_string();
    app.cursor = 0;
    app.output_area = Rect::new(0, 0, 60, 3);
    app.composer_area = Rect::new(0, 8, 60, 1);
    let pw = app.prompt_w() as u16;

    assert_eq!(
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), pw + 6, 8),
        MouseAction::Redraw
    );
    assert_eq!(
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), pw + 6, 8),
        MouseAction::Redraw
    );
    assert_eq!(app.cursor, 6, "the caret goes before the word clicked on");
    assert!(app.select.is_none(), "a click leaves no selection behind");

    // A drag in the composer asks for the copy instead.
    assert!(app.select_begin(pw + 2, 8));
    assert!(app.select_drag(pw + 6, 8));
    assert_eq!(
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), pw + 6, 8),
        MouseAction::Copy(SelectKind::Composer, (pw + 2, 8), (pw + 6, 8))
    );
    assert_eq!(app.cursor, 6, "a selection never moves the caret");
}

/// The composer that was drawn is the one drags map to: the box the mouse
/// is over, the row the text sits on, and the cells the highlight covers.
#[test]
fn the_composer_drawn_on_screen_is_the_one_drags_map_to() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.mode = Mode::Idle;
    app.input = "select me please".to_string();
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let screen = draw(&mut app, &mut term);

    let row = app.composer_area.y;
    let line = screen.lines().nth(row as usize).unwrap();
    assert!(line.contains("select me please"), "{screen}");
    let col = |byte: usize| line[..byte].chars().count() as u16;
    let from = col(line.find("me please").unwrap());
    let to = from + 1;

    assert!(app.select_begin(from, row), "the press lands in the composer");
    assert!(app.select_drag(to, row));
    draw(&mut app, &mut term);

    let buf = term.backend().buffer();
    for c in from..=to {
        assert!(
            buf.cell((c, row)).unwrap().modifier.contains(Modifier::REVERSED),
            "column {c} of the composer is not highlighted"
        );
    }
    assert_eq!(app.input_text((from, row), (to, row)), "me");
}

/// A wrapped composer maps the second and later rows back to the chars
/// drawn on them, and pulls up to the caret when the text outgrows a box
/// that is only two rows tall.
#[test]
fn a_wrapped_composer_maps_each_row_back_to_its_chars() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.mode = Mode::Idle;
    app.composer_area = Rect::new(0, 20, 16, 2);
    let pw = app.prompt_w() as u16;
    app.input = "abcdefghij klmnopqrstuvwxyz abcd efgh".to_string();
    app.cursor = app.input.chars().count();

    let layout = app.layout_input((16 - pw) as usize);
    assert!(layout.rows.len() > 2, "the text needs more rows than the box: {:?}", layout.rows);
    // The box shows the last two rows, so its first screen row is row 1.
    app.composer_first = layout.cur_row - 1;
    let second = layout.rows[app.composer_first + 1].clone();
    let first_on_screen = layout.starts[app.composer_first];
    assert_eq!(app.input_text((pw, 20), (pw, 20)).chars().count(), 1);
    assert_eq!(
        app.input_text((pw, 20), (pw + 1, 20)),
        app.input.chars().skip(first_on_screen).take(2).collect::<String>()
    );
    assert_eq!(
        app.input_text((pw, 21), (pw + 1, 21)),
        second.chars().take(2).collect::<String>()
    );
}

/// A press outside both boxes starts nothing, and a drag never leaves the
/// box it began in.
#[test]
fn a_drag_stays_in_the_box_it_started_in() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.output_area = Rect::new(0, 0, 40, 6);
    app.composer_area = Rect::new(0, 8, 40, 1);

    assert_eq!(
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 7),
        MouseAction::None,
        "the rule between the boxes takes no selection"
    );
    assert!(app.select_begin(5, 5));
    assert!(app.select_drag(5, 30));
    let select = app.select.expect("still dragging");
    assert_eq!(select.kind, SelectKind::Output);
    assert_eq!(select.cursor.1, 5, "clamped to the transcript's last row");
}

/// The loop's mouse arms: the wheel scrolls and drops a selection, a left
/// drag selects, letting go asks for the copy, and stray motion does
/// nothing.
#[test]
fn mouse_events_map_to_the_loop_actions() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.output_area = Rect::new(1, 1, 20, 5);

    assert_eq!(app.on_mouse(MouseEventKind::Moved, 5, 3), MouseAction::None);
    assert_eq!(
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 3),
        MouseAction::Redraw
    );
    assert_eq!(
        app.on_mouse(MouseEventKind::Drag(MouseButton::Left), 9, 3),
        MouseAction::Redraw
    );
    assert_eq!(
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), 9, 3),
        MouseAction::Copy(SelectKind::Output, (5, 3), (9, 3))
    );
    assert_eq!(
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), 9, 3),
        MouseAction::None,
        "the selection is gone once it has been taken"
    );

    // The wheel scrolls, and a selection doesn't survive it.
    assert!(app.select_begin(5, 3));
    assert!(app.select_drag(9, 3));
    assert_eq!(app.on_mouse(MouseEventKind::ScrollUp, 0, 0), MouseAction::Redraw);
    assert!(app.selection().is_none());

    // A press outside the transcript is not a selection.
    assert_eq!(
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), 40, 20),
        MouseAction::None
    );
}

/// A plain click is not a selection: it must not copy a stray character,
/// and must not claim it copied anything.
#[test]
fn a_click_copies_nothing() {
    use ratatui::layout::Rect;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.output_area = Rect::new(0, 0, 10, 3);
    let buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 10, 3));

    assert!(app.select_begin(3, 1));
    assert_eq!(
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), 3, 1),
        MouseAction::None,
        "a press and release on one cell is a click, not a selection"
    );
    assert!(app.selection().is_none(), "the selection is let go");
    assert!(app.flash.is_none(), "and no note claims a copy");

    // A drag over blank cells says so, rather than copying whitespace.
    assert!(app.select_begin(1, 0));
    assert!(app.select_drag(6, 2));
    app.capture_frame(&buf);
    app.copy_range((1, 0), (6, 2));
    assert_eq!(
        app.flash.as_ref().map(|(note, _)| note.as_str()),
        Some("nothing to copy")
    );
}

/// A drag across the output lights up exactly the cells it covers, and the
/// copy reads those same cells back: the highlight and the clipboard can't
/// disagree about what was selected, because both come from the frame.
#[test]
fn a_drag_highlights_the_cells_it_covers_and_copies_them() {
    use crate::ui::helpers;
    use crate::ui::types::Kind;
    use ratatui::style::Modifier;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(test_ui_config(), Vec::new());
    app.transcript.clear();
    app.push(Kind::Assistant, "the quick brown fox".to_string());
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let screen = draw(&mut app, &mut term);

    let row = screen.lines().position(|l| l.contains("quick brown")).unwrap() as u16;
    let line = screen.lines().nth(row as usize).unwrap();
    // Byte offsets are not columns: the assistant's glyph is multibyte.
    let col = |byte: usize| line[..byte].chars().count() as u16;
    let from = col(line.find("quick").unwrap());
    let to = col(line.find("brown").unwrap() + "brown".len() - 1);

    assert!(app.select_begin(from, row), "the press lands in the transcript");
    assert!(app.select_drag(to, row));
    // Redraw with the selection up, then read the frame back.
    draw(&mut app, &mut term);

    let buf = term.backend().buffer();
    assert_eq!(helpers::selected_text(buf, app.output_area, (from, row), (to, row)), "quick brown");
    for col in from..=to {
        let cell = buf.cell((col, row)).unwrap();
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "column {col} is not highlighted: {:?}",
            cell.modifier
        );
    }
}

#[test]
fn the_output_actually_moves_and_the_hint_shows_in_the_rendered_screen() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (mut app, h) = scrollable_app();
    app.max_top = 0;
    app.view_h = 0;
    for i in 0..80 {
        app.note(format!("transcript line {i}"));
    }
    let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();

    let first = draw(&mut app, &mut term); // sets max_top from the real layout
    assert!(app.max_top > 0, "the test transcript overflows the view");
    assert!(first.contains("transcript line 79"), "the live end is visible");
    assert!(!first.contains("end"), "no hint while following");

    app.on_key(ctrl(KeyCode::Up), &h);
    let scrolled = draw(&mut app, &mut term);
    assert!(
        !scrolled.contains("transcript line 79"),
        "the live end scrolled off the bottom"
    );
    assert!(
        scrolled.contains("↓ end"),
        "the status bar advertises the live end below:\n{scrolled}"
    );
    assert!(!scrolled.contains("↓ new"), "nothing new arrived yet");

    // Streaming while pinned keeps the position and flips the hint to "new".
    let before = app.scroll;
    app.handle_event(UiEvent::Token("a fresh reply".into()), &h);
    app.handle_event(UiEvent::AssistantCommit, &h);
    let streaming = draw(&mut app, &mut term);
    assert_eq!(app.scroll, before, "the reading position is preserved");
    assert!(
        streaming.contains("↓ new"),
        "new output is called out:\n{streaming}"
    );

    app.on_key(key(KeyCode::PageDown), &h);
    let bottom = draw(&mut app, &mut term);
    assert!(bottom.contains("a fresh reply"), "PgDn returns to the live end");
    assert!(!bottom.contains("end") && !bottom.contains("new"));
}

#[test]
fn wheel_over_output_or_input_only_scrolls_output() {
    use ratatui::{backend::TestBackend, Terminal};
    let (mut app, _h) = scrollable_app();
    for i in 0..80 { app.note(format!("transcript line {i}")); }
    app.input = "draft first line\ndraft second line".into();
    app.cursor = app.input.chars().count();
    let draft = app.input.clone();
    let cursor = app.cursor;
    let history = app.hist_idx;
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    draw(&mut app, &mut term);
    let composer_area = app.composer_area;
    let bottom = app.scroll;
    for area in [app.output_area, app.composer_area] {
        app.on_mouse(MouseEventKind::ScrollUp, area.x, area.y);
        draw(&mut app, &mut term);
        assert!(app.scroll < bottom);
        assert_eq!(app.input, draft);
        assert_eq!(app.cursor, cursor);
        assert_eq!(app.hist_idx, history);
        assert_eq!(app.composer_area, composer_area);
    }
    for _ in 0..2 { app.on_mouse(MouseEventKind::ScrollDown, composer_area.x, composer_area.y); }
    draw(&mut app, &mut term);
    assert_eq!(app.scroll, bottom);
    assert_eq!(app.input, draft);
    assert_eq!(app.cursor, cursor);
    assert_eq!(app.hist_idx, history);
}
