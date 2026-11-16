use super::*;

impl App {
    pub fn on_paste(&mut self, s: String) {
        // A paste never submits. The composer keeps its newlines (CRLF and lone
        // CR become \n) and tabs verbatim; the single-line prompts drop both,
        // and no other control character enters any buffer.
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let clean: String = s.chars().filter(|c| !c.is_control() || *c == '\t' || *c == '\n').collect();
        let one_line = || clean.chars().filter(|c| *c != '\t' && *c != '\n');
        // Paste goes to whichever prompt owns the keyboard. A password pasted
        // from a manager must land in the masked prompt — not in the hidden
        // composer, where it would later show in clear and go to the model.
        match &mut self.mode {
            Mode::Password { .. } => self.pw_input.extend(one_line()),
            Mode::Question { .. } => self.q_input.extend(one_line()),
            Mode::ProviderInput { .. } => {
                let pos = self.wiz_input.char_indices().nth(self.wiz_cursor).map_or(self.wiz_input.len(), |(i, _)| i);
                let pasted: String = one_line().collect();
                self.wiz_input.insert_str(pos, &pasted);
                self.wiz_cursor += pasted.chars().count();
            }
            Mode::Select => {
                if let Some(p) = self.picker.as_mut() {
                    p.filter.extend(one_line());
                    p.cursor = 0;
                    p.scroll = 0;
                }
            }
            Mode::Settings { edit: Some(buf), .. } => buf.extend(one_line()),
            Mode::Settings { edit: None, .. }
            | Mode::Approval(_)
            | Mode::ThemeSelect { .. }
            | Mode::StatuslineSelect { .. } => {}
            Mode::Idle | Mode::Busy => {
                // An empty paste is what some terminals send for Cmd+V when
                // the clipboard holds only an image: fetch the image instead.
                if clean.trim().is_empty() {
                    if let Ok(path) = clipboard_image() {
                        self.insert_attachment(&format!("@{path}"));
                    }
                    return;
                }
                // Files dragged into the terminal paste as their paths.
                if let Some(att) = pasted_image_attachments(&clean) {
                    self.insert_attachment(&att);
                    return;
                }
                for c in clean.chars() {
                    self.insert_char(c);
                }
            }
        }
    }

    /// Ctrl+V: attach the clipboard's image (terminals only paste text).
    pub(super) fn paste_clipboard_image(&mut self) {
        match clipboard_image() {
            Ok(path) => self.insert_attachment(&format!("@{path}")),
            Err(e) => self.push(Kind::Notice, format!("paste image: {e}")),
        }
    }

    /// Insert `@path` attachment text at the cursor, space-separated from
    /// its neighbours so the token stays whole.
    fn insert_attachment(&mut self, att: &str) {
        let before = self.input[..self.byte_at(self.cursor)].chars().next_back();
        if before.is_some_and(|c| !c.is_whitespace()) {
            self.insert_char(' ');
        }
        for c in att.chars() {
            self.insert_char(c);
        }
        self.insert_char(' ');
    }

    pub(super) fn insert_char(&mut self, c: char) {
        // Never let a control character into the composer buffer. A pasted
        // snippet copied from a colored terminal (or a log file) carries raw
        // ESC sequences; drawn into the frame they desync the terminal from
        // ratatui's cell grid, so the rest of the screen — transcript included —
        // comes out in the wrong colors until a full repaint.
        // Newlines (Shift/Alt+Enter, Ctrl+J, a paste) and pasted tabs are text;
        // every other control character is dropped.
        let c = if c == '\n' || c == '\t' {
            c
        } else if c.is_control() {
            return;
        } else if self.single_width && unicode_width::UnicodeWidthChar::width(c) != Some(1) {
            // The framebuffer console draws every glyph in one cell; a wide
            // char here would desync the grid exactly like it would in the
            // transcript (see clean_text), so it gets the same placeholder.
            '?'
        } else {
            c
        };
        let pos = self.byte_at(self.cursor);
        self.input.insert(pos, c);
        self.cursor += 1;
    }

    /// Terminal cells a char occupies in the composer. Zero-width marks ride
    /// on the previous cell; CJK and most emoji take two.
    pub(super) fn cell_w(&self, c: char) -> usize {
        match c {
            '\t' => TAB_W,
            '\n' => 0,
            _ if self.single_width => 1,
            _ => unicode_width::UnicodeWidthChar::width(c).unwrap_or(0),
        }
    }

    pub(super) fn str_w(&self, s: &str) -> usize {
        s.chars().map(|c| self.cell_w(c)).sum()
    }

    /// Lay the composer text out in rows of at most `w` cells, wrapping at a
    /// word boundary whenever there is one: a word is never cut in half just
    /// because it didn't fit — the whole word moves down and the spaces before
    /// it stay on the row above (so the rows still cover the text with no
    /// chars skipped, which keeps `starts` a simple offset table). A single
    /// word longer than a row has no boundary to use and is split, as before.
    ///
    /// Locates the cursor (a char index) as a (row, cell column,
    /// chars-into-row) triple. `input_rows` and `render_composer` both come
    /// from here, so the caret can never sit N cells left of the glyph it
    /// belongs to on a line with wide characters.
    pub(super) fn layout_input(&self, w: usize) -> ComposerLayout {
        let w = w.max(1);
        let chars: Vec<char> = self.input.chars().collect();
        let n = chars.len();
        let mut rows: Vec<String> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        let mut i = 0usize;
        loop {
            starts.push(i);
            // The row runs from `i` to `j`: as many chars as fit, stopping at
            // the end of the line or the end of the text.
            let mut j = i;
            let mut col = 0usize;
            while j < n && chars[j] != '\n' {
                let cw = self.cell_w(chars[j]);
                if col + cw > w && j > i {
                    break;
                }
                col += cw;
                j += 1;
            }
            // Word wrap: the char that didn't fit is part of a word that has no
            // room on this row, so break at the last space instead and let the
            // whole word start the next row. Without a space to break at (one
            // long word, or leading indentation) the char split above stands.
            if j < n && chars[j] != '\n' && !chars[j].is_whitespace() {
                if let Some(k) = (i..j).rfind(|&k| chars[k].is_whitespace()) {
                    j = k + 1;
                }
            }
            rows.push(chars[i..j].iter().collect());
            if j < n && chars[j] == '\n' {
                // A hard break: the row ends here and the newline isn't drawn,
                // so the next row starts past it (and may be empty).
                i = (j + 1).min(n);
                continue;
            }
            if j >= n {
                break;
            }
            i = j;
        }
        // The cursor sits on the row that holds it; at a soft wrap the caret
        // belongs to the row the char it precedes starts (the text reads on
        // from there), and after the last char it is past the end of the last
        // row.
        let cur = self.cursor.min(n);
        let mut cur_row = 0usize;
        let mut cur_idx = 0usize;
        for r in 0..rows.len() {
            let start = starts[r];
            let end = start + rows[r].chars().count();
            // A row that ends at a newline doesn't hold it, but the cursor on
            // that newline still belongs to the row (at the end of it).
            let on_its_newline = cur == end && chars.get(cur) == Some(&'\n');
            if cur >= start && (cur < end || on_its_newline || r + 1 == rows.len()) {
                cur_row = r;
                cur_idx = cur - start;
                break;
            }
        }
        let last = rows.len() - 1;
        if cur_row == last && cur_idx >= rows[last].chars().count() && self.str_w(&rows[last]) >= w {
            // Cursor after the last char of a full last row: it spills onto a
            // fresh row, so the caret isn't drawn past the terminal edge.
            rows.push(String::new());
            starts.push(n);
            cur_row = rows.len() - 1;
            cur_idx = 0;
        }
        let cur_col = rows[cur_row].chars().take(cur_idx).map(|c| self.cell_w(c)).sum();
        ComposerLayout { rows, cur_row, cur_col, cur_idx, starts }
    }

    /// Text width of the composer at the last rendered terminal size (the
    /// same arithmetic as `input_rows`).
    fn input_w(&self) -> usize {
        (self.term_w as usize).saturating_sub(2 + self.prompt_w()).max(1)
    }

    /// Most rows the composer grows to before it scrolls: a third of the
    /// terminal, between 5 and 12.
    pub(super) fn composer_cap(&self) -> usize {
        (self.term_h as usize / 3).clamp(5, 12)
    }

    /// Move the cursor to the wrapped row above/below, keeping its column as
    /// near as the row allows. False at the first/last row, so the caller can
    /// give the key its single-line meaning (scrolling the transcript).
    pub(super) fn move_row(&mut self, down: bool) -> bool {
        let lay = self.layout_input(self.input_w());
        let target = if down {
            lay.cur_row + 1
        } else {
            match lay.cur_row.checked_sub(1) {
                Some(r) => r,
                None => return false,
            }
        };
        if target >= lay.rows.len() {
            return false;
        }
        let start = lay.starts[target];
        let row = &lay.rows[target];
        let len = row.chars().count();
        // A soft-wrapped row's end is the next row's start: stop one short so
        // the cursor stays on the row it was moved to.
        let soft = lay.starts.get(target + 1).is_some_and(|&next| next == start + len);
        let max = if soft && len > 0 { len - 1 } else { len };
        let (mut col, mut off) = (0usize, 0usize);
        for c in row.chars().take(max) {
            let cw = self.cell_w(c);
            if col + cw > lay.cur_col {
                break;
            }
            col += cw;
            off += 1;
        }
        self.cursor = start + off;
        true
    }

    /// Char index of the start / end of the cursor's line (between newlines).
    pub(super) fn line_bounds(&self) -> (usize, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let cur = self.cursor.min(chars.len());
        let start = chars[..cur].iter().rposition(|&c| c == '\n').map(|i| i + 1).unwrap_or(0);
        let end = chars[cur..].iter().position(|&c| c == '\n').map(|i| cur + i).unwrap_or(chars.len());
        (start, end)
    }

    /// Enter with a newline intent: Shift/Alt+Enter, or Enter right after a
    /// trailing backslash (which the newline replaces) — the spelling that
    /// works on the Pi's framebuffer console, where Shift+Enter is plain Enter.
    pub(super) fn enter_inserts_newline(&mut self, key: &KeyEvent) -> bool {
        if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) {
            // The key asked for a row; a word is never split in two, so inside
            // one it is a no-op (still handled — Enter must not send).
            self.insert_newline();
            return true;
        }
        if self.cursor > 0 && self.input.chars().nth(self.cursor - 1) == Some('\\') {
            // The backslash is replaced by the break, so the word test is the
            // one at the backslash: mid-word nothing happens at all (and the
            // backslash stays — it isn't swallowed for nothing).
            if self.splits_word_over(self.cursor - 1) {
                return true;
            }
            self.cursor -= 1;
            let pos = self.byte_at(self.cursor);
            self.input.remove(pos);
            self.insert_char('\n');
            return true;
        }
        false
    }

    /// Break the line at the cursor — unless that would cut a word in half
    /// (`fo|o`, `src/ma|in.rs`), which is how a prompt ends up as two lines
    /// with half a word on each.
    pub(super) fn insert_newline(&mut self) {
        if !self.splits_word(self.cursor) {
            self.insert_char('\n');
        }
    }

    /// True when a break at char index `at` would land inside a word: a word
    /// char on each side of it.
    fn splits_word(&self, at: usize) -> bool {
        let chars: Vec<char> = self.input.chars().collect();
        splits_word_between(at.checked_sub(1).and_then(|i| chars.get(i).copied()), chars.get(at).copied())
    }

    /// The same test for a break at char index `at` where the char there is a
    /// trailing backslash about to be replaced by the newline: the word on
    /// either side of it is what a break would cut, not the backslash.
    fn splits_word_over(&self, at: usize) -> bool {
        let chars: Vec<char> = self.input.chars().collect();
        splits_word_between(at.checked_sub(1).and_then(|i| chars.get(i).copied()), chars.get(at + 1).copied())
    }

    pub(super) fn slash_suggestions(&self) -> Vec<(&'static str, &'static str)> {
        // `/cmd ` + a partial option: that command's options.
        if self.mode == Mode::Idle && self.input.starts_with('/') && !self.input.contains('\n') {
            if let Some((cmd, rest)) = self.input.split_once(' ') {
                if rest.contains(char::is_whitespace) {
                    return Vec::new();
                }
                return types::options_of(cmd).filter(|(o, _)| o.starts_with(&self.input)).collect();
            }
        }
        if self.mode == Mode::Idle && self.input.starts_with('/') && !self.input.contains(char::is_whitespace) {
            let mut scored: Vec<_> = SLASH_COMMANDS
                .iter()
                .chain(self.custom_palette.iter())
                .chain(self.mcp_palette.iter())
                .filter(|(cmd, _)| cmd.starts_with(&self.input))
                .map(|&(cmd, desc)| {
                    let count = self.cmd_uses.get(cmd).copied().unwrap_or(0);
                    (cmd, desc, count)
                })
                .collect();
            scored.sort_by_key(|(cmd, _, count)| {
                // Exact match first, then prefix matches sorted by usage (desc),
                // then alphabetically.
                (
                    if *cmd == self.input { 0 } else { 1 },
                    std::cmp::Reverse(*count),
                    *cmd,
                )
            });
            scored.truncate(MAX_SUGGEST);
            scored.into_iter().map(|(c, d, _)| (c, d)).collect()
        } else {
            Vec::new()
        }
    }

    pub(super) fn rebuild_cmd_uses(&mut self) {
        self.cmd_uses.clear();
        for entry in &self.history {
            if let Some(cmd) = entry.split_whitespace().next() {
                if cmd.starts_with('/') {
                    *self.cmd_uses.entry(cmd.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    pub(super) fn byte_at(&self, char_idx: usize) -> usize {
        self.input
            .chars()
            .take(char_idx)
            .map(|c| c.len_utf8())
            .sum()
    }

    pub(super) fn char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Readline / Emacs line editing in the composer: the shortcuts anyone who
    /// has spent time in a shell or Emacs reaches for. Returns true when the
    /// key was consumed.
    ///
    /// Ctrl+A/E jump to the start/end of the line, Ctrl+B/F move a char,
    /// Ctrl+K/U kill to the end/start of the line, Ctrl+W (and Alt+Backspace)
    /// kill the word before the cursor, Alt+D the word after it, Ctrl+Y yanks
    /// the last kill back, Ctrl+H is a backspace, Alt+B/F move by word, and
    /// Ctrl+D deletes the char under the cursor when the line isn't empty
    /// (an empty line still arms/quits, as a bare shell prompt does).
    ///
    /// The app's own Ctrl bindings are matched before this and win: Ctrl+C/D
    /// (quit — handled by the caller), Ctrl+J (newline), Ctrl+L (repaint),
    /// Ctrl+P (permissions), Ctrl+T (brief) and Ctrl+V (paste image).
    ///
    /// Where the keys go on the way in — the audit behind picking them, checked
    /// against Haiku's Terminal (src/apps/terminal in the Haiku tree), macOS and
    /// Linux:
    /// - Ctrl+letter is a plain 0x01..0x1a byte, and Haiku's Terminal writes
    ///   every key it doesn't specially handle straight through
    ///   (TermViewStates::DefaultState::KeyDown), so it works there too. The
    ///   exceptions are Ctrl+H (0x08), Ctrl+I (0x09) and Ctrl+M (0x0d), which
    ///   *are* Backspace, Tab and Enter on the wire — crossterm reports 0x08 as
    ///   Ctrl+H, so it is a usable alias here, but it is never the only binding
    ///   for an action. Ctrl+S/Ctrl+Q are avoided: XOFF/XON on a Linux console
    ///   freeze the output. Ctrl+J reaches us as 0x0a, which crossterm maps to
    ///   Ctrl+J in raw mode (it is only Enter outside raw mode).
    /// - Alt+letter is the fragile class. On Haiku it is B_COMMAND_KEY, i.e.
    ///   Terminal's *menu* accelerator — Alt+A/C/F/G/L/P/Q/T/V/W are Select all,
    ///   Copy, Find, Find next, Clear all, Print, Quit, New tab, Paste, Close —
    ///   and the menu wins over the view, so Alt+F (next word) can never reach
    ///   us there. It also arrives as ESC+letter only when Terminal's
    ///   Settings → "Use Option as Meta key" is on (macOS needs its Option-as-Meta
    ///   equivalent); otherwise it is not reported as a modified key at all.
    ///   Hence no action is Alt-only: Ctrl+W/Alt+Backspace (back a word),
    ///   Alt+B/F and Ctrl+←/→ (by word, and Haiku's Terminal encodes
    ///   modifiers on the arrows as xterm `ESC[1;<mod><letter>`), Ctrl+Y.
    /// - Haiku's Terminal strips modifiers from Backspace and Delete
    ///   (B_BACKSPACE/B_DELETE are rewritten without `mod`), so Ctrl+Backspace
    ///   and Ctrl+Delete degrade to a plain char delete there (checked live in
    ///   the UTM VM). Alt+Backspace still deletes a word with "Use Option as
    ///   Meta key" off; Alt+D needs the meta option. Ctrl+W, Alt+Backspace and
    ///   Ctrl+←/→ are the Haiku-safe spellings. It also keeps Ctrl+Shift+↑/↓ and
    ///   Shift+PgUp/PgDn for scrolling itself.
    /// - A multiplexer eats one of them: tmux's default prefix is Ctrl+B and
    ///   GNU screen's is Ctrl+A (a popular tmux setting too). Those two reach
    ///   the app only as the prefix; Ctrl+A-in-tmux users get Ctrl+E/Home and
    ///   the arrows (which is why the arrow motions stay bound).
    /// - Ctrl+P is *not* readline's history-back here: it cycles permission
    ///   modes because the Pi's console can't report Shift+Tab. Ctrl+N is left
    ///   alone so history isn't half-bound; ↑/↓ do that.
    pub(super) fn on_key_readline(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Ctrl+<letter> arrives as Char with CONTROL under both legacy and the
        // Kitty protocol; Alt+<letter> as Char with ALT, which used to fall
        // through and type the bare letter.
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = self.line_bounds().0,
            KeyCode::Char('e') if ctrl => self.cursor = self.line_bounds().1,
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(self.char_len()),
            KeyCode::Char('b') if alt => self.cursor = self.prev_word(),
            KeyCode::Char('f') if alt => self.cursor = self.next_word(),
            KeyCode::Char('h') if ctrl => self.on_key_edit(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Char('d') if alt => {
                let end = self.next_word();
                self.kill(self.cursor, end);
            }
            KeyCode::Char('k') if ctrl => {
                let end = self.line_bounds().1;
                self.kill(self.cursor, end);
            }
            KeyCode::Char('u') if ctrl => {
                let start = self.line_bounds().0;
                self.kill(start, self.cursor);
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.prev_word();
                self.kill(start, self.cursor);
            }
            KeyCode::Char('y') if ctrl => {
                let ring = self.kill_ring.clone();
                for c in ring.chars() {
                    self.insert_char(c);
                }
            }
            _ => return false,
        }
        // Any edit invalidates the `/` palette highlight (as typing does).
        self.suggest_idx = 0;
        true
    }

    /// Cut chars `from..to` out of the composer into the kill ring.
    fn kill(&mut self, from: usize, to: usize) {
        if to <= from {
            return;
        }
        let (start, end) = (self.byte_at(from), self.byte_at(to));
        self.kill_ring = self.input[start..end].to_string();
        self.input.drain(start..end);
        self.cursor = from;
    }

    /// Delete the char under the cursor (Ctrl+D, readline's delete-char), or
    /// the newline that ends the line so two lines join.
    pub(super) fn delete_forward(&mut self) {
        if self.cursor >= self.char_len() {
            return;
        }
        let pos = self.byte_at(self.cursor);
        self.input.remove(pos);
    }

    /// Backspace / Delete, with the word-deleting modifiers: Alt+Backspace and
    /// Alt+Delete (what macOS and most terminals send), plus Ctrl+Backspace
    /// and Ctrl+Delete for the platforms where Alt is not the app's to take —
    /// on Haiku Alt is the menu accelerator key, so Ctrl is the reliable
    /// modifier there (though its Terminal strips modifiers from Backspace and
    /// Delete, so Ctrl+W / Alt+Backspace are the Haiku spellings). All of them go through the kill ring, so a kill that was
    /// one word too many comes back with Ctrl+Y.
    pub(super) fn on_key_edit(&mut self, key: KeyEvent) {
        let word = key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Backspace => {
                if word {
                    let start = self.prev_word();
                    self.kill(start, self.cursor);
                } else if self.cursor > 0 {
                    self.cursor -= 1;
                    let pos = self.byte_at(self.cursor);
                    self.input.remove(pos);
                }
            }
            KeyCode::Delete => {
                if word {
                    let end = self.next_word();
                    self.kill(self.cursor, end);
                } else {
                    let pos = self.byte_at(self.cursor);
                    if pos < self.input.len() {
                        self.input.remove(pos);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn prev_word(&self) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    pub(super) fn next_word(&self) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Tab-complete the `@path` word ending at the cursor, wherever it sits in
    /// the line (`fix @src/ma` → `fix @src/main.rs`), not only a line that
    /// starts with `@`.
    pub(super) fn complete(&mut self) {
        let end = self.byte_at(self.cursor);
        let start = self.input[..end].rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
        let word = &self.input[start..end];
        let Some(prefix) = word.strip_prefix('@') else { return };
        let opts = complete_path(prefix);
        let fill = match opts.len() {
            0 => return,
            1 => opts[0].clone(),
            _ => {
                let lcp = longest_common_prefix(&opts);
                if lcp.len() <= prefix.len() {
                    return;
                }
                lcp
            }
        };
        let added = fill.chars().count() - prefix.chars().count();
        self.input.replace_range(start + 1..end, &fill);
        self.cursor += added;
    }

    pub(super) fn history_prev(&mut self) {
        if self.hist_idx > 0 {
            if self.hist_idx == self.history.len() {
                self.pending = std::mem::take(&mut self.input);
            }
            self.hist_idx -= 1;
            self.input = self.history[self.hist_idx].clone();
            self.cursor = self.char_len();
        }
    }

    pub(super) fn history_next(&mut self) {
        if self.hist_idx < self.history.len() {
            self.hist_idx += 1;
            if self.hist_idx == self.history.len() {
                self.input = std::mem::take(&mut self.pending);
            } else {
                self.input = self.history[self.hist_idx].clone();
            }
            self.cursor = self.char_len();
        }
    }
}
