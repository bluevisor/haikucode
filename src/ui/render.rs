use super::*;

impl App {
    /// Add the launch banner. Only a placeholder goes into the transcript; the
    /// display cache expands it at the pane's current width, so the logo and
    /// SYSTEM box reflow live on resize.
    pub fn banner(&mut self, status: Vec<String>) {
        self.banner_status = status;
        self.transcript.push(TLine { kind: Kind::BannerBlock, text: String::new(), lead: false, color: None });
        self.push_dim(String::new());
        self.after_push();
    }

    /// Render the run of work entries starting at `start` as a brief summary;
    /// returns the index just past the run. Blank separators inside the run are
    /// absorbed, trailing ones are left to render normally. While an approval
    /// is pending, the last tool call and its diff stay in full so the change
    /// being approved is visible.
    fn render_brief_run(&self, out: &mut Vec<Line<'static>>, start: usize, width: usize, approving: bool) -> usize {
        let mut end = start;
        while end < self.transcript.len() && (brief::is_work(self.transcript[end].kind) || self.transcript[end].kind == Kind::Blank) {
            end += 1;
        }
        while end > start && self.transcript[end - 1].kind == Kind::Blank {
            end -= 1;
        }
        let mut cut = end;
        if approving && end == self.transcript.len() {
            if let Some(p) = self.transcript[start..end].iter().rposition(|t| t.kind == Kind::Tool) {
                cut = start + p;
            }
        }
        let run: Vec<TLine> = self.transcript[start..cut]
            .iter()
            .filter(|t| t.kind != Kind::Blank)
            .map(|t| TLine { kind: t.kind, text: t.text.clone(), lead: t.lead, color: None })
            .collect();
        let rows = brief::summarize(&run);
        for (label, text) in &rows {
            out.push(self.brief_row(label, text, width, false));
        }
        for t in &self.transcript[cut..end] {
            render_tline(out, t.kind, &t.text, t.lead, t.color, width, self.glyphs, &self.palette, self.single_width);
        }
        end
    }

    pub(super) fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.settings.show_thinking = self.show_thinking;
        Config::persist_show_thinking(self.show_thinking);
        let what = if self.show_thinking { "full thinking and tool output" } else { "thinking summaries and live activity (ctrl+t for details)" };
        self.push(Kind::Notice, format!("showing {what}"));
    }

    fn render_banner(&self, out: &mut Vec<Line<'static>>, width: usize) {
        // The pane is 2 columns narrower than the terminal the banner used to be
        // sized from (term width − 4), so keep the same 2-column margin.
        let w = width.saturating_sub(2).max(8);
        for bl in banner_lines(w, self.ascii, &self.banner_status) {
            let (kind, color) = match bl.role {
                BRole::Art(i, half) => (Kind::Banner, Some(BannerColor::Rainbow(i, half))),
                BRole::Version => (Kind::Banner, None),
                BRole::Tagline => (Kind::BannerDim, None),
                BRole::Frame | BRole::Data => (Kind::BannerDim, Some(BannerColor::Accent)),
            };
            render_tline(out, kind, &bl.text, false, color, width, self.glyphs, &self.palette, self.single_width);
        }
    }

    fn push_dim(&mut self, text: String) {
        self.transcript.push(TLine { kind: Kind::BannerDim, text, lead: false, color: None });
        self.dirty();
    }

    pub fn welcome(&mut self) {
        let model = self.model_info.clone();
        self.push(Kind::Notice, format!("Haiku Code ({model}) — theme: {} · type a task, or /help for commands", self.palette.name));
    }

    pub fn note(&mut self, s: String) {
        self.push(Kind::Notice, s);
    }

    /// Show a resumed session's recent exchanges ((role, text) pairs).
    pub fn replay(&mut self, items: Vec<(String, String)>) {
        for (role, text) in items {
            let kind = if role == "user" { Kind::User } else { Kind::Assistant };
            self.push(kind, text);
        }
    }

    /// Ask the worker for this directory's sessions; the list opens a picker.
    pub fn open_session_picker(&mut self, h: &Handles) {
        let _ = h.cmd_tx.send(WorkerCmd::ListSessions);
        self.mode = Mode::Select;
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn busy(&self) -> bool {
        self.mode == Mode::Busy
    }

    pub fn tick_spinner(&mut self) -> bool {
        self.spin_counter += 1;
        if self.spin_counter.is_multiple_of(6) {
            self.spinner = (self.spinner + 1) % 10;
            true
        } else {
            false
        }
    }

    pub(super) fn spin_frame(&self) -> &'static str {
        if self.ascii {
            SPIN_A[self.spinner % SPIN_A.len()]
        } else {
            SPIN_U[self.spinner]
        }
    }

    pub(super) fn working_status(&self, width: usize) -> String {
        let elapsed = match self.busy_since {
            Some(t) => {
                let secs = t.elapsed().as_secs();
                if secs < 60 {
                    format!(" ({secs}s)")
                } else {
                    let m = secs / 60;
                    let s = secs % 60;
                    format!(" ({m}:{s:02})")
                }
            }
            None => String::new(),
        };
        let elapsed = if self.str_w(&elapsed) < width { elapsed } else { String::new() };
        let action = self.current_activity.as_deref().unwrap_or({
            if self.live.is_empty() { "Thinking…" } else { "Writing response…" }
        });
        let action = helpers::clean_text(action, self.single_width);
        let action = action.split_whitespace().collect::<Vec<_>>().join(" ");
        let hint = if width >= 70 { " · Esc to interrupt · Enter queues" } else { "" };
        let room = width.saturating_sub(self.str_w(&elapsed) + self.str_w(hint));
        let action = if room == 0 {
            String::new()
        } else if self.str_w(&action) > room {
            let mut used = 0;
            let clipped: String = action.chars().take_while(|&c| {
                used += self.cell_w(c);
                used <= room.saturating_sub(1)
            }).collect();
            format!("{clipped}…")
        } else {
            action
        };
        format!("{action}{elapsed}{hint}")
    }

    fn border_type(&self) -> BorderType {
        if self.glyphs.rounded {
            BorderType::Rounded
        } else {
            BorderType::Plain
        }
    }

    /// A comfortable mid-gray for secondary text. True RGB over SSH (where ANSI
    /// gray can render as pure white); plain gray on the 16-color console.
    pub(super) fn dim_text(&self) -> Color {
        self.palette.secondary
    }

    // ----------------------------------------------------------- render -----

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        self.term_w = area.width;
        self.term_h = area.height;
        // Paint the themed background first; spans drawn on top keep their own
        // fg and inherit this bg (Color::Reset on the default theme = no-op,
        // so the terminal's own background shows through).
        if self.palette.bg != Color::Reset {
            f.render_widget(
                Block::default().style(Style::default().bg(self.palette.bg)),
                area,
            );
        }
        let ch = self.composer_rows(area.width);
        let chunks = Layout::vertical([
            Constraint::Min(3),    // output
            Constraint::Length(1), // rule
            Constraint::Length(ch),// composer / busy / approval
            Constraint::Length(1), // rule
            Constraint::Length(1), // status: model · usage · ctx
            Constraint::Length(1), // status: permission mode
        ])
        .split(area);
        self.render_output(f, chunks[0]);
        self.render_rule(f, chunks[1]);
        self.render_inputline(f, pad1(chunks[2]));
        self.render_rule(f, chunks[3]);
        self.render_status1(f, pad1(chunks[4]));
        self.render_status2(f, pad1(chunks[5]));
        self.render_selection(f);
    }

    /// Tint the cells a drag covers, whatever they hold — prose, a diff, a
    /// wrapped long line. Reversing them keeps each cell's own color readable
    /// on any theme, and it needs nothing from the renderer but the box.
    fn render_selection(&self, f: &mut Frame) {
        let Some(select) = self.select else {
            return;
        };
        let (start, end) = select.corners();
        let right = select.area.right().saturating_sub(1);
        let buf = f.buffer_mut();
        for row in start.1..=end.1 {
            let from = if row == start.1 { start.0 } else { select.left };
            let to = if row == end.1 { end.0 } else { right };
            for col in from.min(right)..=to.min(right) {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    let style = cell.style().add_modifier(Modifier::REVERSED);
                    cell.set_style(style);
                }
            }
        }
    }

    /// The line under a picker's title: the wizard's last error (red), else
    /// the picker's own hint.
    pub(super) fn picker_hint(&self, picker: &Picker) -> Option<Line<'static>> {
        let error = self.wizard.as_ref().and_then(|d| d.error.clone()).filter(|_| matches!(picker.action, PickAction::Auth));
        match (error, &picker.hint) {
            (Some(e), _) => Some(Line::from(Span::styled(e, Style::default().fg(Color::Red)))),
            (None, Some(h)) => Some(Line::from(Span::styled(h.clone(), Style::default().fg(self.dim_text())))),
            (None, None) => None,
        }
    }

    /// What the agent is doing, for the prompt's marker.
    pub(super) fn activity(&self) -> Activity {
        match &self.mode {
            Mode::Approval(_) | Mode::Question { .. } | Mode::Password { .. } => Activity::Asking,
            Mode::Busy => match &self.tool_running {
                Some(name) => Activity::of_tool(name),
                None if !self.live.is_empty() => Activity::Writing,
                None => Activity::Thinking,
            },
            _ => Activity::Idle,
        }
    }

    /// The theme's prompt with its marker showing the agent's activity.
    pub(super) fn prompt_str(&self) -> String {
        let base = self.palette.prompt.unwrap_or(self.glyphs.prompt);
        activity::prompt_for(base, self.activity(), !self.ascii && !self.single_width)
    }

    /// Width of the prompt. Taken from the theme's own prompt: the activity
    /// marker is always one cell, so the composer never reflows.
    pub(super) fn prompt_w(&self) -> usize {
        self.str_w(self.palette.prompt.unwrap_or(self.glyphs.prompt))
    }

    /// Wrap width for an ask_user question: `inner_width` is the panel's own
    /// width (already inset by one column each side), less the `? ` lead that
    /// hangs on the first row. The row height and the render both come from
    /// here, so the panel can't reserve fewer rows than the question needs.
    pub(super) fn question_wrap_w(&self, inner_width: u16) -> usize {
        (inner_width as usize)
            .saturating_sub(helpers::QUESTION_LEAD.chars().count())
            .max(1)
    }

    /// Rows the composer text needs at this width (same in Idle and Busy).
    pub(super) fn input_rows(&self, width: u16) -> u16 {
        let w = (width as usize).saturating_sub(2 + self.prompt_w()).max(1);
        self.layout_input(w).rows.len().clamp(1, self.composer_cap()) as u16
    }

    fn composer_rows(&self, width: u16) -> u16 {
        match &self.mode {
            Mode::Busy => {
                1 + self.queued.len() as u16 + self.input_rows(width)
                    + self.slash_suggestions().len() as u16
            }
            Mode::Approval(_) => 2,
            Mode::ThemeSelect { .. } => THEMES.len() as u16 + 1,
            Mode::StatuslineSelect { items, .. } => items.len() as u16 + 1,
            Mode::Settings { .. } => SETTING_ROWS.len() as u16 + 1,
            Mode::Select => {
                let n = self.picker.as_ref().map(|p| p.filtered().len()).unwrap_or(0);
                let hint = self.picker.as_ref().and_then(|p| self.picker_hint(p)).is_some() as u16;
                n.clamp(1, PICKER_VISIBLE) as u16 + 1 + hint
            }
            Mode::Password { .. } => 2,
            Mode::ProviderInput { step } => {
                let (title, hint) = self.wizard_prompt(*step);
                let hint = self.wizard.as_ref().and_then(|d| d.error.as_deref()).unwrap_or(&hint);
                let w = width.saturating_sub(2).max(1) as usize;
                [title.as_str(), hint, Self::WIZARD_CONTROLS].iter()
                    .map(|text| helpers::question_lines(text, w, self.single_width).len() as u16)
                    .sum::<u16>() + 1
            }
            Mode::Question { prompt } => {
                let rows = helpers::question_lines(
                    prompt,
                    self.question_wrap_w(width.saturating_sub(2)),
                    self.single_width,
                )
                .len();
                rows as u16 + 1
            }
            Mode::Idle => self.input_rows(width) + self.slash_suggestions().len() as u16,
        }
    }

    fn render_rule(&self, f: &mut Frame, area: Rect) {
        let ch = if self.ascii { "-" } else { "─" };
        let line = ch.repeat(area.width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(line, Style::default().fg(self.palette.chrome)))),
            area,
        );
    }

    fn render_output(&mut self, f: &mut Frame, area: Rect) {
        self.refresh_git_head();
        let mut title_spans = vec![
            Span::styled(format!(" {} ", self.cwd_label), Style::default().fg(self.palette.accent).add_modifier(Modifier::BOLD)),
        ];
        if let Some((branch, dirty)) = self.git_head.clone() {
            let dot = if self.ascii || self.single_width { "*" } else { "●" };
            let dot_color = if dirty { self.palette.code } else { self.palette.diff_add };
            title_spans.push(Span::styled(
                format!("{branch} "),
                Style::default().fg(self.palette.secondary),
            ));
            title_spans.push(Span::styled(
                format!("{dot} "),
                Style::default().fg(dot_color),
            ));
        }
        let title = Line::from(title_spans);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.border_type())
            .border_style(Style::default().fg(self.palette.chrome))
            .title(title);
        let inner = block.inner(area);
        f.render_widget(block, area);
        // Remembered for drag-selections: a selection is a range of screen
        // cells in this box, and nothing outside it.
        self.output_area = inner;

        let width = inner.width as usize;
        self.ensure_display_cache(width);
        let live = self.build_live_lines(width);
        let total = self.disp_cache.len() + live.len();
        self.view_h = inner.height as usize;
        self.max_top = total.saturating_sub(self.view_h);
        let top = if self.follow { self.max_top } else { self.scroll.min(self.max_top) };
        self.scroll = top;
        let mut visible: Vec<Line> = Vec::with_capacity(self.view_h);
        for ln in self.disp_cache.iter().skip(top).take(self.view_h) {
            visible.push(ln.clone());
        }
        let remaining = self.view_h.saturating_sub(visible.len());
        if remaining > 0 {
            let live_skip = top.saturating_sub(self.disp_cache.len());
            for ln in live.into_iter().skip(live_skip).take(remaining) {
                visible.push(ln);
            }
        }
        f.render_widget(Paragraph::new(visible), inner);
    }

    pub(super) fn ensure_display_cache(&mut self, width: usize) {
        let view = (self.show_thinking, matches!(self.mode, Mode::Approval(_)));
        if self.disp_cache_width == width && self.disp_cache_tver == self.tver && self.disp_cache_view == view {
            return;
        }
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut i = 0;
        while i < self.transcript.len() {
            let t = &self.transcript[i];
            if !self.show_thinking && brief::is_work(t.kind) {
                i = self.render_brief_run(&mut out, i, width, view.1);
                continue;
            }
            i += 1;
            if t.kind == Kind::BannerBlock {
                self.render_banner(&mut out, width);
            } else if t.kind == Kind::Assistant {
                render_message(&mut out, &t.text, t.lead, width, self.glyphs, &self.palette, self.single_width);
            } else {
                render_tline(&mut out, t.kind, &t.text, t.lead, t.color, width, self.glyphs, &self.palette, self.single_width);
            }
        }
        self.disp_cache = out;
        self.disp_cache_width = width;
        self.disp_cache_tver = self.tver;
        self.disp_cache_view = view;
    }

    fn build_live_lines(&self, width: usize) -> Vec<Line<'static>> {
        // Mirror exactly what `flush_live` will commit (reasoning first, then
        // the answer, with the same separators), so nothing on screen moves
        // the moment the live text becomes transcript.
        let mut out: Vec<Line<'static>> = Vec::new();
        if self.awaiting_response && self.mode == Mode::Busy {
            // Fixed-width ASCII pulse: a short trail glides through the track
            // without shifting the label or growing a line of punctuation.
            const PULSE: [&str; 10] = [
                ".      ", "..     ", "...    ", " ...   ", "  ...  ",
                "   ... ", "    ...", "     ..", "      .", "       ",
            ];
            let pulse = PULSE[(self.spin_counter / 6) % PULSE.len()];
            let text = brief::clip(&format!("Waiting for model  [{pulse}]"), width);
            out.push(Line::from(Span::styled(text, Style::default().fg(self.dim_text()))));
        }
        let mut prev = self.transcript.last().map(|t| t.kind);
        if !self.live_reasoning.trim().is_empty() {
            // In brief view a blank between work entries is absorbed into the
            // run's summary, so don't reserve one here either.
            let absorbed = !self.show_thinking && prev.is_some_and(brief::is_work);
            if prev.is_some_and(|p| needs_sep(p, Kind::Reasoning)) && !absorbed {
                out.push(Line::default());
            }
            if self.show_thinking {
                render_tline(&mut out, Kind::Reasoning, &self.live_reasoning, true, None, width, self.glyphs, &self.palette, self.single_width);
            } else {
                out.push(self.brief_row("", &self.live_reasoning, width, true));
            }
            prev = Some(Kind::Reasoning);
        }
        if !self.live.is_empty() {
            if prev.is_some_and(|p| needs_sep(p, Kind::Assistant)) {
                out.push(Line::default());
            }
            render_message(&mut out, &self.live, true, width, self.glyphs, &self.palette, self.single_width);
        }
        out
    }

    /// One brief-view list entry, clipped to one screen line. Thoughts have
    /// no label; failures carry an explicit error label.
    fn brief_row(&self, label: &str, text: &str, width: usize, gist: bool) -> Line<'static> {
        let prefix = if label.is_empty() { String::new() } else { format!("{label}  ") };
        let room = width.saturating_sub(2 + prefix.len());
        let text = if gist { brief::gist(text, room) } else { brief::clip(text, room) };
        let mut out = Vec::new();
        render_tline(&mut out, Kind::Reasoning, &format!("{prefix}{text}"), true, None, width, self.glyphs, &self.palette, self.single_width);
        out.truncate(1);
        out.pop().unwrap_or_default()
    }
}
