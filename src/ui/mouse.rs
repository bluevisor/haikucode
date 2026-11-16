use super::*;

impl App {
    pub fn mouse_scroll(&mut self, up: bool) {
        if up {
            self.scroll_lines(-SCROLL_STEP);
        } else {
            self.scroll_lines(SCROLL_STEP);
        }
    }

    /// Handle a mouse event and say what the loop should do about it: the
    /// wheel scrolls (dropping any selection), a left drag selects in the
    /// transcript or the composer, and letting go asks for a copy. Kept apart
    /// from the event loop so the mapping is tested; reading the drawn frame
    /// and repainting stay the loop's job.
    pub fn on_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) -> MouseAction {
        match kind {
            MouseEventKind::ScrollUp => {
                self.select_clear();
                self.mouse_scroll(true);
                MouseAction::Redraw
            }
            MouseEventKind::ScrollDown => {
                self.select_clear();
                self.mouse_scroll(false);
                MouseAction::Redraw
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.select_begin(col, row) { MouseAction::Redraw } else { MouseAction::None }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.select_drag(col, row) { MouseAction::Redraw } else { MouseAction::None }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Letting go ends the selection either way.
                let Some(select) = self.select.take() else {
                    return MouseAction::None;
                };
                let (start, end) = select.corners();
                if start == end {
                    // A click: in the composer it puts the caret where it
                    // landed (the transcript has nothing to put it in).
                    if select.kind == SelectKind::Composer {
                        self.place_caret(end.0, end.1);
                        return MouseAction::Redraw;
                    }
                    return MouseAction::None;
                }
                MouseAction::Copy(select.kind, start, end)
            }
            _ => MouseAction::None,
        }
    }

    /// Start a drag at a screen cell: in the transcript, or in the composer's
    /// text. A press anywhere else starts nothing. Returns whether one began
    /// (so the caller repaints).
    pub fn select_begin(&mut self, col: u16, row: u16) -> bool {
        let pos = Position::new(col, row);
        let (kind, area, left) = if self.output_area.contains(pos) {
            (SelectKind::Output, self.output_area, self.output_area.x)
        } else if self.composer_area.contains(pos) {
            // The prompt isn't text: the drag starts at the first input column.
            let left = self.composer_area.x + self.prompt_w() as u16;
            (SelectKind::Composer, self.composer_area, left.min(self.composer_area.right() - 1))
        } else {
            return false;
        };
        if area.width == 0 || area.height == 0 {
            return false;
        }
        let at = (col.clamp(left, area.right() - 1), row.clamp(area.y, area.bottom() - 1));
        self.select = Some(Select { kind, area, left, anchor: at, cursor: at });
        true
    }

    /// Extend the drag to a screen cell: past the box's edge the selection
    /// stops at the edge rather than running into the rest of the screen.
    pub fn select_drag(&mut self, col: u16, row: u16) -> bool {
        let Some(select) = self.select.as_mut() else {
            return false;
        };
        select.cursor = (
            col.clamp(select.left, select.area.right().saturating_sub(1)),
            row.clamp(select.area.y, select.area.bottom().saturating_sub(1)),
        );
        true
    }

    /// The selection in reading order — top-left cell first — for the copy and
    /// the highlight alike.
    pub fn selection(&self) -> Option<((u16, u16), (u16, u16))> {
        self.select.map(|s| s.corners())
    }

    /// Forget the selection (the view scrolled, the user typed).
    pub fn select_clear(&mut self) {
        self.select = None;
        self.screen = None;
    }

    /// Copy a range of screen cells out of the frame that was drawn and put it
    /// on the system clipboard. A cell read back from the buffer is exactly what
    /// the user saw, so wrapped paragraphs, diffs and tool output all copy as
    /// written on screen — the renderer's layout never has to be reconstructed.
    pub fn copy_range(&mut self, start: (u16, u16), end: (u16, u16)) {
        let Some(buf) = self.screen.take() else {
            return;
        };
        let text = helpers::selected_text(&buf, self.output_area, start, end);
        self.copy_text(text);
    }

    /// Copy the composer's text between two cells of it: the input string, not
    /// the cells, so the prompt, the wrap points and the block cursor stay out
    /// of what lands on the clipboard.
    pub fn copy_input_range(&mut self, start: (u16, u16), end: (u16, u16)) {
        let text = self.input_text(start, end);
        self.copy_text(text);
    }

    /// The input text a cell range covers. Char indices, so a wide character or
    /// a tab can't shift the result.
    pub(super) fn input_text(&self, a: (u16, u16), b: (u16, u16)) -> String {
        let chars: Vec<char> = self.input.chars().collect();
        let (Some(from), Some(to)) = (self.composer_index(a), self.composer_index(b)) else {
            return String::new();
        };
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if chars.is_empty() {
            return String::new();
        }
        // The cell past a row's last character selects up to it, not past the
        // end of the input.
        let hi = hi.min(chars.len() - 1);
        if lo > hi {
            return String::new();
        }
        chars[lo..=hi].iter().collect()
    }

    /// Put the caret where a click landed in the composer.
    fn place_caret(&mut self, col: u16, row: u16) {
        if let Some(index) = self.composer_index((col, row)) {
            self.cursor = index.min(self.input.chars().count());
        }
    }

    /// The char of the input a composer cell maps to, laid out exactly as the
    /// composer was drawn (same wrap width, same scrolled-to-the-caret offset).
    /// Past the end of a row it is the position after that row's last char;
    /// below the last row, the end of the input.
    fn composer_index(&self, at: (u16, u16)) -> Option<usize> {
        let area = self.composer_area;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let pw = self.prompt_w();
        let width = (area.width as usize).saturating_sub(pw).max(1);
        let layout = self.layout_input(width);
        let offset = at.1.saturating_sub(area.y) as usize;
        let row = self.composer_first + offset;
        let Some(line) = layout.rows.get(row) else {
            return Some(self.input.chars().count());
        };
        // Columns are measured from the first cell of the input on that row.
        let mut want = (at.0.saturating_sub(area.x) as usize).saturating_sub(pw);
        let mut chars = 0;
        for c in line.chars() {
            let cells = self.cell_w(c).max(1);
            if want < cells {
                return Some(layout.starts[row] + chars);
            }
            want -= cells;
            chars += 1;
        }
        Some(layout.starts[row] + chars)
    }

    /// Hand `text` to the clipboard and say how much went, or why it couldn't.
    fn copy_text(&mut self, text: String) {
        if text.trim().is_empty() {
            self.flash_note("nothing to copy".to_string());
            return;
        }
        match helpers::copy_to_clipboard(&text) {
            Ok(()) => {
                let chars = text.chars().count();
                let lines = text.lines().count().max(1);
                let what = if lines > 1 {
                    format!("{chars} chars, {lines} lines")
                } else {
                    format!("{chars} chars")
                };
                self.flash_note(format!("copied {what}"));
            }
            Err(e) => self.flash_note(format!("copy failed: {e}")),
        }
    }

    /// Keep the frame just drawn, for the copy a release will make.
    pub fn capture_frame(&mut self, buf: &ratatui::buffer::Buffer) {
        self.screen = Some(buf.clone());
    }

    /// A one-line note in the status bar, in place for a moment.
    fn flash_note(&mut self, text: String) {
        self.flash = Some((text, Instant::now()));
    }

    /// Drop an expired flash note; true when it went, so the loop repaints.
    pub fn flash_due(&mut self) -> bool {
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() > FLASH_TIME) {
            self.flash = None;
            return true;
        }
        false
    }

    /// Scroll the transcript by `n` lines (negative scrolls up). Scrolling up
    /// pins the view (`follow = false`); coming back to the bottom resumes
    /// following the live end.
    pub(super) fn scroll_lines(&mut self, n: isize) {
        if n < 0 {
            // Nothing above the fold: don't pin or flash the "more below" hint.
            if self.max_top == 0 {
                return;
            }
            self.follow = false;
            self.scroll = self.scroll.saturating_sub(n.unsigned_abs());
            self.scrolled_up = true;
        } else {
            self.scroll = (self.scroll + n as usize).min(self.max_top);
            if self.scroll >= self.max_top {
                self.follow = true;
                self.scrolled_up = false;
                self.new_below = false;
            }
        }
    }

    /// Scroll a whole screenful (PgUp/PgDn), using the last rendered viewport
    /// height so a page is exactly what the user can see.
    pub(super) fn scroll_page(&mut self, up: bool) {
        let page = self.view_h.max(1) as isize;
        self.scroll_lines(if up { -page } else { page });
    }

    /// Snap back to the live end, dropping the "[arrow] new" hint. Called when
    /// the user sends or queues a message so the answer they're waiting for is
    /// on screen.
    pub(super) fn scroll_to_bottom(&mut self) {
        self.follow = true;
        self.scrolled_up = false;
        self.new_below = false;
    }
}
