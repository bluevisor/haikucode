use super::*;

impl App {
    pub(super) fn render_inputline(&mut self, f: &mut Frame, area: Rect) {
        // Whatever the mode, forget where the last frame's composer was: only
        // a composer drawn this frame may be dragged in.
        self.composer_area = Rect::new(0, 0, 0, 0);
        self.composer_first = 0;
        match &self.mode {
            Mode::Idle => self.render_composer(f, area),
            Mode::Busy => {
                // The status clips the action to leave room for its timer and controls.
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{} ", self.spin_frame()), Style::default().fg(self.palette.accent)),
                    Span::styled(self.working_status(area.width.saturating_sub(2) as usize), Style::default().fg(self.dim_text())),
                ])];
                let mark = if self.ascii { "->" } else { "↳" };
                for q in &self.queued {
                    // One row per message: a multi-line one shows its first line.
                    let mut ql = q.lines();
                    let first = ql.next().unwrap_or("").replace('\t', "    ");
                    let more = ql.count();
                    let more = if more > 0 { format!(" (+{more} lines)") } else { String::new() };
                    lines.push(Line::from(Span::styled(
                        format!("  {mark} queued: {first}{more}"),
                        Style::default().fg(self.dim_text()),
                    )));
                }
                let head_h = (lines.len() as u16).min(area.height);
                f.render_widget(Paragraph::new(lines), Rect { height: head_h, ..area });
                if area.height > head_h {
                    let comp = Rect {
                        y: area.y + head_h,
                        height: area.height - head_h,
                        ..area
                    };
                    self.render_composer(f, comp);
                }
            }
            Mode::Approval(desc) => {
                let desc_line = Line::from(vec![
                    Span::styled("approve ", Style::default().fg(Color::Yellow)),
                    Span::styled(desc.clone(), Style::default().add_modifier(Modifier::BOLD)),
                ]);
                let mut opts = vec![
                    Span::styled("(Y)", Style::default().fg(Color::Green)),
                    Span::raw("es  "),
                    Span::styled("(N)", Style::default().fg(Color::Red)),
                    Span::raw("o  "),
                ];
                if let Some(rule) = &self.appr_rule {
                    opts.push(Span::styled("(P)", Style::default().fg(self.palette.accent)));
                    opts.push(Span::raw(format!("attern: don't ask again for {rule}  ")));
                }
                opts.push(Span::styled("(A)", Style::default().fg(self.palette.accent)));
                opts.push(Span::raw("lways (YOLO)"));
                let opts_line = Line::from(opts);
                f.render_widget(Paragraph::new(vec![desc_line, opts_line]), area);
            }
            Mode::ThemeSelect { cursor, .. } => {
                let mut lines: Vec<Line> = Vec::new();
                lines.push(Line::from(vec![
                    Span::styled("select theme ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        "(up/down to preview, Enter to keep, Esc to cancel)",
                        Style::default().fg(self.dim_text()),
                    ),
                ]));
                let marker = if self.ascii { ">" } else { "▸" };
                for (i, t) in THEMES.iter().enumerate() {
                    let sel = i == *cursor;
                    let lead = if sel { marker } else { " " };
                    let style = if sel {
                        Style::default()
                            .fg(self.palette.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(self.dim_text())
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{lead}{t}"), style),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::StatuslineSelect { cursor, items, .. } => {
                let mut lines: Vec<Line> = vec![Line::from(vec![
                    Span::styled("status bar ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        "(Space show/hide, Shift+up/down or [ ] to move, Enter to keep, Esc to cancel)",
                        Style::default().fg(self.dim_text()),
                    ),
                ])];
                let marker = if self.ascii { ">" } else { "▸" };
                let (on_box, off_box) = if self.ascii { ("[x]", "[ ]") } else { ("[✓]", "[ ]") };
                for (i, (id, on)) in items.iter().enumerate() {
                    let sel = i == *cursor;
                    let style = if sel {
                        Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD)
                    } else if *on {
                        Style::default().fg(self.palette.assistant)
                    } else {
                        Style::default().fg(self.dim_text())
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{}{} {:<9}", if sel { marker } else { " " }, if *on { on_box } else { off_box }, id),
                            style,
                        ),
                        Span::styled(format!(" {}", statusline::describe(id)), Style::default().fg(self.dim_text())),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::Settings { cursor, edit } => {
                let mut lines: Vec<Line> = Vec::new();
                let hint = if edit.is_some() {
                    "(type the new value, Enter to save, Esc to cancel)"
                } else {
                    "(up/down to browse, Enter or left/right to change, Esc to close)"
                };
                lines.push(Line::from(vec![
                    Span::styled("settings ", Style::default().fg(Color::Yellow)),
                    Span::styled(hint, Style::default().fg(self.dim_text())),
                ]));
                let marker = if self.ascii { ">" } else { "▸" };
                for (i, &row) in SETTING_ROWS.iter().enumerate() {
                    let label = SETTING_LABELS[row];
                    let sel = i == *cursor;
                    let lead = if sel { marker } else { " " };
                    let label_style = if sel {
                        Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(self.dim_text())
                    };
                    // The row being edited shows what's typed (the key masked);
                    // it used to keep showing the old value, so typing seemed
                    // to do nothing and a stray Enter saved an empty field.
                    let val = match edit {
                        Some(buf) if sel => {
                            let shown = if row == 3 { "•".repeat(buf.chars().count()) } else { buf.clone() };
                            format!("{shown}{}", if self.ascii { "_" } else { "▏" })
                        }
                        _ => self.setting_value(row),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{lead}{label:<16} "), label_style),
                        Span::styled(val, Style::default().fg(self.palette.accent)),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::Select => {
                let mut lines: Vec<Line> = Vec::new();
                if let Some(ref picker) = self.picker {
                    let mut header =
                        vec![Span::styled(picker.title.to_string(), Style::default().fg(Color::Yellow))];
                    if picker.filter.is_empty() {
                        header.push(Span::styled(
                            " (type to filter, Enter to select, Esc to cancel)",
                            Style::default().fg(self.dim_text()),
                        ));
                    } else {
                        // What's been typed, so the filter isn't invisible.
                        let caret = if self.ascii { "_" } else { "▏" };
                        header.push(Span::styled("  filter: ", Style::default().fg(self.dim_text())));
                        header.push(Span::styled(
                            format!("{}{caret}", picker.filter),
                            Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD),
                        ));
                        header.push(Span::styled(
                            "  (Backspace to edit, Enter to select, Esc to cancel)",
                            Style::default().fg(self.dim_text()),
                        ));
                    }
                    lines.push(Line::from(header));
                    if let Some(row) = self.picker_hint(picker) {
                        lines.push(row);
                    }
                    let filtered = picker.filtered();
                    if filtered.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "no matches",
                            Style::default().fg(self.dim_text()),
                        )));
                    } else {
                        let marker = if self.ascii { ">" } else { "▸" };
                        let end = (picker.scroll + PICKER_VISIBLE).min(filtered.len());
                        for (fi, &idx) in filtered.iter().enumerate().take(end).skip(picker.scroll) {
                            let sel = fi == picker.cursor;
                            let lead = if sel { marker } else { " " };
                            let mark = if Some(idx) == picker.current { " (*)" } else { "" };
                            let style = if sel {
                                Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(self.dim_text())
                            };
                            lines.push(Line::from(vec![
                                Span::styled(format!("{lead}{}{mark}", picker.items[idx]), style),
                            ]));
                        }
                    }
                }
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::ProviderInput { step } => {
                let (title, hint) = self.wizard_prompt(*step);
                let error = self.wizard.as_ref().and_then(|d| d.error.clone());
                let shown: String = if *step == ProviderStep::ApiKey {
                    self.wiz_input.chars().map(|_| '*').collect()
                } else {
                    self.wiz_input.clone()
                };
                // Keep the insertion point visible when editing a long URL in
                // a narrow terminal. Mask secrets before calculating the view.
                let available = area.width.saturating_sub(3).max(1) as usize;
                let mut start = self.wiz_cursor;
                let mut used = 0;
                for c in shown.chars().take(self.wiz_cursor).collect::<Vec<_>>().into_iter().rev() {
                    let width = if self.single_width { 1 } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) };
                    if used + width >= available { break; }
                    used += width;
                    start -= 1;
                }
                let before: String = shown.chars().skip(start).take(self.wiz_cursor - start).collect();
                let cursor_char = shown.chars().nth(self.wiz_cursor).unwrap_or(' ').to_string();
                let after: String = shown.chars().skip(self.wiz_cursor + 1).collect();
                let w = area.width.max(1) as usize;
                let mut lines: Vec<Line> = helpers::question_lines(&title, w, self.single_width).into_iter()
                    .map(|s| Line::from(Span::styled(s, Style::default().fg(self.palette.accent)))).collect();
                let hint_color = if error.is_some() { Color::Red } else { self.dim_text() };
                for s in helpers::question_lines(error.as_deref().unwrap_or(&hint), w, self.single_width) {
                    lines.push(Line::from(Span::styled(s, Style::default().fg(hint_color))));
                }
                lines.push(Line::from(vec![
                        Span::styled("› ", Style::default().fg(Color::Yellow)),
                        Span::styled(before, Style::default().fg(self.palette.accent)),
                        Span::styled(cursor_char, Style::default().fg(self.palette.accent).add_modifier(Modifier::REVERSED)),
                        Span::styled(after, Style::default().fg(self.palette.accent)),
                    ]));
                for s in helpers::question_lines(Self::WIZARD_CONTROLS, w, self.single_width) {
                    lines.push(Line::from(Span::styled(s, Style::default().fg(self.dim_text()))));
                }
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::Password { prompt } => {
                let mut lines: Vec<Line> = Vec::new();
                let masked: String = self.pw_input.chars().map(|_| '*').collect();
                lines.push(Line::from(vec![
                    Span::styled(prompt.clone(), Style::default().fg(self.palette.accent)),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(if masked.is_empty() { " " } else { &masked }, Style::default().fg(self.palette.accent)),
                    Span::styled("█", Style::default().add_modifier(Modifier::REVERSED)),
                ]));
                f.render_widget(Paragraph::new(lines), area);
            }
            Mode::Question { prompt } => {
                // The question is model text that can be long, multi-line, or
                // even carry an escape sequence, so it is sanitized and wrapped
                // (never handed to the frame as one row, which used to clip it
                // at the panel edge and let a raw newline break the frame).
                let wrap_w = self.question_wrap_w(area.width);
                let lead_w = helpers::QUESTION_LEAD.chars().count();
                let mut lines: Vec<Line> = Vec::new();
                for (i, row) in helpers::question_lines(prompt, wrap_w, self.single_width)
                    .into_iter()
                    .enumerate()
                {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if i == 0 { helpers::QUESTION_LEAD.to_string() } else { " ".repeat(lead_w) },
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::styled(row, Style::default().fg(self.palette.accent)),
                    ]));
                }
                let mut line_spans = vec![Span::styled(" ".repeat(lead_w), Style::default())];
                if !self.q_input.is_empty() {
                    line_spans.push(Span::styled(
                        self.q_input.clone(),
                        Style::default().fg(self.palette.accent),
                    ));
                }
                line_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
                lines.push(Line::from(line_spans));
                f.render_widget(Paragraph::new(lines), area);
            }
        }
    }

    fn render_composer(&mut self, f: &mut Frame, inner: Rect) {
        // Remembered for mouse selections: a drag in the composer is a range
        // of its cells, mapped back to chars by the same layout.
        self.composer_area = inner;
        let pw = self.prompt_w();
        let w = (inner.width as usize).saturating_sub(pw).max(1);
        let empty = self.input.is_empty();
        let ComposerLayout { rows, cur_row, cur_col, cur_idx, .. } = self.layout_input(w);
        let total_rows = rows.len();
        let max_rows = inner.height as usize;
        let start_row = if cur_row >= max_rows { cur_row - max_rows + 1 } else { 0 };
        self.composer_first = start_row;

        let prompt = self.prompt_str();
        let cursor = self.palette.cursor;
        let block = if self.ascii { "#" } else { "▒" };
        let accent = Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD);
        let cont = " ".repeat(pw);

        // Tabs live in the buffer verbatim; on screen each is TAB_W spaces.
        let tabs = |t: String| if t.contains('\t') { t.replace('\t', &" ".repeat(TAB_W)) } else { t };
        let mut lines: Vec<Line> = Vec::new();
        for r in start_row..(start_row + max_rows).min(total_rows.max(1)) {
            let prefix = if r == 0 { Span::styled(prompt.clone(), accent) } else { Span::raw(cont.clone()) };
            let row: &str = rows.get(r).map(String::as_str).unwrap_or("");
            let mut spans = vec![prefix];
            let text_style = Style::default().fg(self.palette.accent);
            if r == cur_row && cursor != CursorKind::Caret {
                let before: String = row.chars().take(cur_idx).collect();
                let after: String = row.chars().skip(cur_idx + 1).collect();
                spans.push(Span::styled(tabs(before), text_style));
                let at_char = row.chars().nth(cur_idx);
                match cursor {
                    CursorKind::Block => spans.push(Span::styled(block, accent)),
                    _ => {
                        let at = match at_char {
                            Some('\t') | None => " ".to_string(),
                            Some(c) => c.to_string(),
                        };
                        spans.push(Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)));
                    }
                }
                // A tab under the cursor still spans TAB_W cells.
                if at_char == Some('\t') {
                    spans.push(Span::styled(" ".repeat(TAB_W - 1), text_style));
                }
                spans.push(Span::styled(tabs(after), text_style));
            } else {
                spans.push(Span::styled(tabs(row.to_string()), text_style));
            }
            if empty && r == 0 {
                let hint = if matches!(self.mode, Mode::Busy) {
                    "  type to queue the next message".to_string()
                } else if let Some(ref s) = self.suggestion {
                    if self.ascii {
                        format!("  [{}] (Tab)", s)
                    } else {
                        format!("  {}  (Tab to accept)", s)
                    }
                } else {
                    "  describe a task · @file to attach · /help".to_string()
                };
                spans.push(Span::styled(hint, Style::default().fg(self.dim_text())));
            }
            lines.push(Line::from(spans));
        }
        // The `/` command palette, under the input.
        let sugg = self.slash_suggestions();
        if !sugg.is_empty() {
            let idx = self.suggest_idx.min(sugg.len() - 1);
            let marker = if self.ascii { ">" } else { "▸" };
            // Line the descriptions up past the longest entry (options run longer
            // than bare commands).
            let col = sugg.iter().map(|(n, _)| n.trim_end().chars().count()).max().unwrap_or(0).max(9);
            for (i, (name, desc)) in sugg.iter().enumerate() {
                let sel = i == idx;
                let name_style = if sel {
                    Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.dim_text())
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} {:<col$} ", if sel { marker } else { " " }, name.trim_end()),
                        name_style,
                    ),
                    Span::styled(desc.to_string(), Style::default().fg(self.dim_text())),
                ]));
            }
        }
        f.render_widget(Paragraph::new(lines), inner);
        if cursor == CursorKind::Caret {
            let cx = inner.x + pw as u16 + cur_col as u16;
            let cy = inner.y + (cur_row - start_row) as u16;
            f.set_cursor_position(Position::new(cx.min(inner.x + inner.width - 1), cy));
        }
    }

    /// Status line 1: the `/statusline` segments, in order (by default model ·
    /// effort · session cost/tokens · context bar · balance). A segment with
    /// nothing to say (no tokens yet, effort auto, no balance, not in a repo)
    /// is left out along with its separator.
    pub(super) fn render_status1(&self, f: &mut Frame, area: Rect) {
        let gray = Style::default().fg(self.dim_text());
        let sep = Span::styled(if self.ascii { "  |  " } else { "  │  " }, Style::default().fg(self.palette.chrome));
        let mut spans: Vec<Span> = Vec::new();
        let mut prev: Option<&str> = None;
        for id in &self.statusline {
            let seg = self.status_segment(id, gray);
            if seg.is_empty() {
                continue;
            }
            match prev.and_then(|p| statusline::glue(id, p)) {
                Some(j) => spans.push(Span::styled(j, gray)),
                None if prev.is_some() => spans.push(sep.clone()),
                None => {}
            }
            spans.extend(seg);
            prev = Some(id);
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// One status segment's spans; empty when it has nothing to show.
    fn status_segment(&self, id: &str, gray: Style) -> Vec<Span<'static>> {
        // The cost is quoted in the configured price currency; the balance is
        // whatever the provider bills the account in. They can legitimately
        // differ (a CNY account using the USD price list), so when they do both
        // figures get their ISO code and the line never implies they're
        // comparable.
        let aligned = self
            .balance
            .as_ref()
            .map(|b| b.currency == self.price_currency)
            .unwrap_or(true);
        let tokens = self.sess_prompt + self.sess_completion;
        match id {
            "model" => vec![Span::styled(
                self.model_info.clone(),
                Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD),
            )],
            "effort" if self.settings.reasoning != "auto" => {
                vec![Span::styled(format!("effort:{}", self.settings.reasoning), gray)]
            }
            "cost" if tokens > 0 => {
                // A custom server without a price list has nothing to show.
                let Some(cost) = self.session_cost() else {
                    return Vec::new();
                };
                let cost = if aligned {
                    money::fmt_cost(cost, &self.price_currency)
                } else {
                    money::fmt_cost_tagged(cost, &self.price_currency)
                };
                vec![Span::styled(cost, gray)]
            }
            "tokens" if tokens > 0 => vec![Span::styled(format!("{} tok", humanize(tokens)), gray)],
            "ctx" => {
                let frac = self.last_prompt_tokens as f64 / self.ctx_limit as f64;
                let mut v = vec![Span::styled("ctx ", gray)];
                v.extend(bar(frac, 8, self.palette.accent));
                v.push(Span::styled(format!(" {}%", (frac.clamp(0.0, 1.0) * 100.0).round() as u32), gray));
                v
            }
            "balance" => match &self.balance {
                Some(b) => {
                    let bal = if aligned { b.render() } else { b.render_tagged() };
                    vec![Span::styled(format!("bal {bal}"), gray)]
                }
                None => Vec::new(),
            },
            "git" => match &self.git_head {
                Some((branch, dirty)) => {
                    vec![Span::styled(format!("{branch}{}", if *dirty { "*" } else { "" }), gray)]
                }
                None => Vec::new(),
            },
            "cwd" => vec![Span::styled(self.cwd_label.clone(), gray)],
            "time" => vec![Span::styled(statusline::local_hhmm(), gray)],
            "version" => vec![Span::styled(format!("v{}", env!("CARGO_PKG_VERSION")), gray)],
            _ => Vec::new(),
        }
    }

    /// True when the `time` segment is shown and the minute has turned since
    /// it was last drawn: the idle loop only redraws on events otherwise.
    pub fn clock_due(&mut self) -> bool {
        if !self.statusline.iter().any(|s| s == "time")
            || self.clock_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return false;
        }
        self.clock_checked = Some(Instant::now());
        let m = statusline::local_hm().1;
        if m == self.clock_min {
            return false;
        }
        self.clock_min = m;
        true
    }

    /// Status line 2: permission mode + hint.
    pub(super) fn render_status2(&self, f: &mut Frame, area: Rect) {
        let (glyph, text, color) = match self.perm() {
            crate::agent::PERM_AUTO => (
                if self.ascii { ">>" } else { "▶▶" },
                "YOLO mode · no approvals",
                Color::Red,
            ),
            crate::agent::PERM_PLAN => (
                if self.ascii { "#" } else { "◆" },
                "plan mode · read-only",
                Color::Cyan,
            ),
            _ => (if self.ascii { "*" } else { "●" }, "ask before edits", Color::Green),
        };
        let down = if self.ascii { "| v" } else { "↓" };
        // "new" only when something actually arrived; a manual scroll-up just
        // hints that the live end is below. Kept near the front of the line
        // (before the shift+tab hint and version) so a narrow terminal can't
        // clip it away.
        let hint = if !self.scrolled_up {
            None
        } else if self.new_below {
            Some(Span::styled(
                format!("  {down} new"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Some(Span::styled(format!("  {down} end"), Style::default().fg(self.dim_text())))
        };
        let mut spans = vec![
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::styled(text, Style::default().fg(color)),
        ];
        // A flash ("copied 214 chars") takes the hint's place for a moment, so
        // the line never grows a second one.
        match &self.flash {
            Some((note, _)) => spans.push(Span::styled(
                format!("  {note}"),
                Style::default()
                    .fg(self.palette.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            None => spans.extend(hint),
        }
        spans.push(Span::styled("  (shift+tab/ctrl+p to cycle)", Style::default().fg(self.dim_text())));
        spans.push(Span::styled(format!("   Haiku Code v{}", env!("CARGO_PKG_VERSION")), Style::default().fg(self.dim_text())));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}
