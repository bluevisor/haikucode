//! Markdown rendering for assistant replies.
//!
//! Models answer in GitHub-flavored Markdown, so the transcript renders the
//! parts they actually use: headings, nested bullet / numbered / task lists,
//! block quotes, rules, fenced code, tables, and inline code, bold, italic,
//! strikethrough and links. It works line by line (a table looks ahead to the
//! end of its rows), so it can redraw a reply that's still streaming: an
//! unterminated fence or a half-sent table simply renders as far as it got.
//!
//! Indentation is structure here, not decoration. A nested list item or an
//! indented code line keeps its leading columns as a prefix, and its wrapped
//! continuations hang under the text, never under the bullet.

use super::helpers::{clean_text, layout};
use super::palette::{self, Palette};
use super::types::{Glyphs, Kind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A run of text in one style.
type Seg = (String, Style);

/// What every renderer below needs from the caller.
struct Ctx<'a> {
    width: usize,
    g: Glyphs,
    p: &'a Palette,
    single_width: bool,
}

impl Ctx<'_> {
    /// Columns taken by the assistant glyph (or its alignment padding).
    fn gutter(&self) -> usize {
        let (indent, glyph) = layout(Kind::Assistant, self.g);
        indent + glyph.width()
    }

    fn base(&self) -> Style {
        palette::colors(Kind::Assistant, self.p).0
    }

    fn dim(&self) -> Style {
        Style::default().fg(self.p.secondary)
    }

    fn colors(&self) -> InlineColors {
        InlineColors { code: self.p.code, dim: self.p.secondary }
    }

    /// Box-drawing characters, or ASCII on terminals without them.
    fn boxes(&self) -> &'static Boxes {
        if self.g.rounded { &BOX_U } else { &BOX_A }
    }
}

/// Render a full assistant message. `lead` puts the assistant glyph on the
/// first row (false when this continues a message already on screen).
pub fn render_message(
    out: &mut Vec<Line<'static>>,
    text: &str,
    lead: bool,
    width: usize,
    g: Glyphs,
    p: &Palette,
    single_width: bool,
) {
    let cx = Ctx { width, g, p, single_width };
    let mut lead = lead;
    // Models habitually start and end a reply with a newline. Rendered literally
    // those become stray empty rows above the glyph and an extra blank on top
    // of the block separator, so both ends are trimmed.
    let text = text.trim_matches(['\n', '\r']);
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut fence: Option<Fence> = None;
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        if let Some(f) = &fence {
            if f.closed_by(raw) {
                fence = None;
            } else {
                code_line(out, &cx, &mut lead, raw, f.indent);
            }
            i += 1;
            continue;
        }
        if let Some(f) = Fence::open(raw) {
            fence = Some(f); // the fence marker itself isn't shown
            i += 1;
            continue;
        }
        if let Some(end) = table_end(&lines, i) {
            if !table(out, &cx, &mut lead, &lines[i..end]) {
                for raw in &lines[i..end] {
                    md_line(out, &cx, &mut lead, raw);
                }
            }
            i = end;
            continue;
        }
        md_line(out, &cx, &mut lead, raw);
        i += 1;
    }
}

// ---------------------------------------------------------------- blocks

/// An open fenced code block: its marker (``` or ~~~, three or more) and the
/// indentation of the opening fence, which its lines are shown without (a
/// fence inside a list item is indented with the item).
struct Fence {
    ch: char,
    len: usize,
    indent: usize,
}

impl Fence {
    fn open(line: &str) -> Option<Fence> {
        let t = line.trim_start();
        let ch = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let len = t.chars().take_while(|c| *c == ch).count();
        // A backtick fence's info string can't hold a backtick, which keeps an
        // inline span like ```x``` on a line of prose from opening a block.
        if len < 3 || (ch == '`' && t[len..].contains('`')) {
            return None;
        }
        Some(Fence { ch, len, indent: line.len() - t.len() })
    }

    fn closed_by(&self, line: &str) -> bool {
        let t = line.trim();
        let n = t.chars().take_while(|c| *c == self.ch).count();
        n >= self.len && n == t.chars().count()
    }
}

/// One line inside a fenced block: code color, indentation kept, and wrapped
/// at the panel edge character by character, so nothing is lost or reflowed.
fn code_line(out: &mut Vec<Line<'static>>, cx: &Ctx, lead: &mut bool, raw: &str, fence_indent: usize) {
    let text = clean_text(raw, cx.single_width);
    // Drop up to the fence's own indentation; anything deeper is the code's.
    let strip = text.chars().take(fence_indent).take_while(|c| *c == ' ').count();
    let text = &text[strip..];
    let indent = text.chars().take_while(|c| *c == ' ').count();
    let style = cx.base().fg(cx.p.code);
    let avail = cx.width.saturating_sub(cx.gutter()).max(1);
    // An indent wider than half the panel would leave no room for the code.
    let indent = if indent * 2 > avail { 0 } else { indent };
    let rows = if text.trim().is_empty() {
        vec![Vec::new()]
    } else {
        hard_split(&text[indent..], avail - indent).into_iter().map(|c| vec![(c, style)]).collect()
    };
    let pad = vec![(" ".repeat(indent), Style::default())];
    emit(out, cx, lead, pad.clone(), pad, rows);
}

/// One line outside code and tables.
fn md_line(out: &mut Vec<Line<'static>>, cx: &Ctx, lead: &mut bool, raw: &str) {
    let text = clean_text(raw, cx.single_width);
    let text: &str = &text;
    let base = cx.base();
    let avail = cx.width.saturating_sub(cx.gutter()).max(1);

    if text.trim().is_empty() {
        return emit(out, cx, lead, Vec::new(), Vec::new(), vec![Vec::new()]);
    }
    if is_rule(text) {
        let rule = cx.boxes().h.repeat(avail);
        return emit(out, cx, lead, Vec::new(), Vec::new(), vec![vec![(rule, Style::default().fg(cx.p.chrome))]]);
    }
    if let Some((level, body)) = heading(text.trim_start()) {
        // Top-level headings in the heading color; deeper ones stay in the
        // text color, so the levels read as a hierarchy.
        let style = if level <= 2 { Style::default().fg(cx.p.heading) } else { base };
        let segs = inline(body, style.add_modifier(Modifier::BOLD), cx.colors());
        return emit(out, cx, lead, Vec::new(), Vec::new(), wrap(segs, avail));
    }
    if let Some((depth, body)) = quote(text) {
        let bar = format!("{} ", cx.boxes().v).repeat(depth);
        let w = bar.width();
        let prefix = vec![(bar, Style::default().fg(cx.p.chrome))];
        let segs = inline(body, cx.dim().add_modifier(Modifier::ITALIC), cx.colors());
        return emit(out, cx, lead, prefix.clone(), prefix, wrap(segs, avail.saturating_sub(w).max(1)));
    }

    // Leading indentation (a nested item, or a paragraph continuing an item)
    // becomes a prefix that wrapped rows repeat.
    let ws = text.len() - text.trim_start().len();
    let indent = ws.min(avail / 2);
    let rest = &text[ws..];
    let (marker, body, body_style) = match list_item(rest) {
        Some(item) => {
            let (marker, style) = match item.kind {
                ItemKind::Bullet => (format!("{} ", bullet(cx.g, ws)), base),
                ItemKind::Number(n) => (format!("{n} "), base),
            };
            match item.task {
                Some(done) => {
                    let boxed = match (cx.g.rounded, done) {
                        (true, false) => "☐ ",
                        (true, true) => "☑ ",
                        (false, false) => "[ ] ",
                        (false, true) => "[x] ",
                    };
                    // A finished task fades so the open ones stand out.
                    let style = if done { cx.dim() } else { base };
                    (format!("{marker}{boxed}"), item.body, style)
                }
                None => (marker, item.body, style),
            }
        }
        None => (String::new(), rest, base),
    };
    let hang = indent + marker.width();
    let first = vec![(format!("{}{marker}", " ".repeat(indent)), base)];
    let cont = vec![(" ".repeat(hang), Style::default())];
    let segs = inline(body, body_style, cx.colors());
    emit(out, cx, lead, first, cont, wrap(segs, avail.saturating_sub(hang).max(1)));
}

/// A thematic break: three or more `-`, `*` or `_`, optionally spaced.
fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && ["-", "*", "_"].iter().any(|m| t.chars().all(|c| c.to_string() == *m))
}

/// An ATX heading (`#`..`######` then a space): its level and text, with any
/// closing `#`s dropped.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let body = line[level + 1..].trim();
    let body = body.trim_end_matches('#').trim_end();
    Some((level, if body.is_empty() { line[level + 1..].trim() } else { body }))
}

/// A block quote (`>`, possibly nested as `> >`): its depth and text.
fn quote(line: &str) -> Option<(usize, &str)> {
    let mut rest = line.trim_start();
    let mut depth = 0;
    while let Some(r) = rest.strip_prefix('>') {
        depth += 1;
        rest = r.strip_prefix(' ').unwrap_or(r).trim_start();
    }
    (depth > 0).then_some((depth, rest))
}

enum ItemKind {
    Bullet,
    /// The item's number with its delimiter, as written (`3.` or `3)`).
    Number(String),
}

struct Item<'a> {
    kind: ItemKind,
    /// `Some(done)` for a task-list item (`- [ ]` / `- [x]`).
    task: Option<bool>,
    body: &'a str,
}

/// A list item (already left-trimmed): `-`/`*`/`+` or `1.`/`1)` then a space.
fn list_item(line: &str) -> Option<Item<'_>> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let (kind, after) = if (1..=9).contains(&digits) && matches!(line.as_bytes().get(digits), Some(b'.' | b')')) {
        (ItemKind::Number(line[..=digits].to_string()), &line[digits + 1..])
    } else if matches!(line.as_bytes().first(), Some(b'-' | b'*' | b'+')) {
        (ItemKind::Bullet, &line[1..])
    } else {
        return None;
    };
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let body = after.trim_start();
    for (mark, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        if let Some(rest) = body.strip_prefix(mark) {
            return Some(Item { kind, task: Some(done), body: rest.trim_start() });
        }
    }
    Some(Item { kind, task: None, body })
}

/// The bullet for a list item indented `ws` columns: one shape per level, so
/// nesting reads even where the indentation is slight.
fn bullet(g: Glyphs, ws: usize) -> &'static str {
    if !g.rounded {
        return "-";
    }
    ["•", "◦", "▪"][(ws / 2) % 3]
}

// ---------------------------------------------------------------- tables

struct Boxes {
    h: &'static str,
    v: &'static str,
    /// Corners and joints, top row then middle then bottom: left, cross, right.
    top: [&'static str; 3],
    mid: [&'static str; 3],
    bot: [&'static str; 3],
}

const BOX_U: Boxes =
    Boxes { h: "─", v: "│", top: ["┌", "┬", "┐"], mid: ["├", "┼", "┤"], bot: ["└", "┴", "┘"] };
const BOX_A: Boxes = Boxes { h: "-", v: "|", top: ["+", "+", "+"], mid: ["+", "+", "+"], bot: ["+", "+", "+"] };

#[derive(Clone, Copy, PartialEq, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

/// If a table starts at `lines[i]` (a row with a `|`, then a delimiter row
/// like `|---|:--:|`), the index just past its last row.
fn table_end(lines: &[&str], i: usize) -> Option<usize> {
    let head = lines[i];
    let delim = lines.get(i + 1)?;
    if !head.contains('|') || delimiter(delim).is_none() {
        return None;
    }
    let mut end = i + 2;
    while end < lines.len() && lines[end].contains('|') && !lines[end].trim().is_empty() {
        end += 1;
    }
    Some(end)
}

/// A table's delimiter row: one alignment per column.
fn delimiter(line: &str) -> Option<Vec<Align>> {
    let cells = cells(line);
    let mut out = Vec::new();
    for c in &cells {
        let c = c.trim();
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let dashes = c.trim_matches(':');
        if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
            return None;
        }
        out.push(match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    (!out.is_empty()).then_some(out)
}

/// Split a table row into its cells. Outer pipes are optional; a pipe inside
/// a code span or escaped as `\|` belongs to the cell.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") { &t[..t.len() - 1] } else { t };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// Draw a table in a box, columns sized to their content and shrunk (longest
/// first, cells wrapping) to fit the panel. False when even three columns per
/// cell won't fit; the caller then shows the rows as text.
fn table(out: &mut Vec<Line<'static>>, cx: &Ctx, lead: &mut bool, rows: &[&str]) -> bool {
    let Some(aligns) = delimiter(rows[1]) else { return false };
    let n = aligns.len();
    let base = cx.base();
    let colors = cx.colors();
    let parse = |line: &str, style: Style| -> Vec<Vec<Seg>> {
        let mut cs = cells(&clean_text(line, cx.single_width));
        cs.resize(n, String::new());
        cs.iter().map(|c| inline(c, style, colors)).collect()
    };
    let mut body: Vec<Vec<Vec<Seg>>> = vec![parse(rows[0], base.add_modifier(Modifier::BOLD))];
    body.extend(rows[2..].iter().map(|r| parse(r, base)));

    // Natural widths, then take a column from the widest until it all fits:
    // borders and padding cost 3 per column plus 1.
    let avail = cx.width.saturating_sub(cx.gutter());
    let room = avail.saturating_sub(3 * n + 1);
    if room < 3 * n {
        return false;
    }
    let mut widths: Vec<usize> = (0..n)
        .map(|c| body.iter().map(|r| r[c].iter().map(|(t, _)| t.width()).sum::<usize>()).max().unwrap_or(0).max(1))
        .collect();
    while widths.iter().sum::<usize>() > room {
        let (widest, _) = widths.iter().enumerate().max_by_key(|(i, w)| (**w, usize::MAX - i)).unwrap();
        widths[widest] -= 1;
    }

    let b = cx.boxes();
    let frame = Style::default().fg(cx.p.chrome);
    let border = |ends: [&str; 3]| -> Vec<Seg> {
        let mut s = ends[0].to_string();
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&b.h.repeat(w + 2));
            s.push_str(if i + 1 == n { ends[2] } else { ends[1] });
        }
        vec![(s, frame)]
    };
    let mut lines: Vec<Vec<Seg>> = vec![border(b.top)];
    for (r, row) in body.iter().enumerate() {
        let wrapped: Vec<Vec<Vec<Seg>>> = row.iter().zip(&widths).map(|(c, w)| wrap(c.clone(), *w)).collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..height {
            let mut line: Vec<Seg> = vec![(b.v.to_string(), frame)];
            for (c, cell) in wrapped.iter().enumerate() {
                let segs = cell.get(k).cloned().unwrap_or_default();
                let used: usize = segs.iter().map(|(t, _)| t.width()).sum();
                let gap = widths[c].saturating_sub(used);
                let (l, rt) = match aligns[c] {
                    Align::Left => (0, gap),
                    Align::Right => (gap, 0),
                    Align::Center => (gap / 2, gap - gap / 2),
                };
                line.push((" ".repeat(l + 1), Style::default()));
                line.extend(segs);
                line.push((" ".repeat(rt + 1), Style::default()));
                line.push((b.v.to_string(), frame));
            }
            lines.push(line);
        }
        if r == 0 {
            lines.push(border(b.mid));
        }
    }
    lines.push(border(b.bot));
    emit(out, cx, lead, Vec::new(), Vec::new(), lines);
    true
}

// ---------------------------------------------------------------- inline

#[derive(Clone, Copy)]
struct InlineColors {
    code: Color,
    dim: Color,
}

/// Parse inline Markdown into styled runs: `` `code` ``, `**bold**`,
/// `*italic*`, `***both***`, `~~strike~~`, `[links](url)`, `<autolinks>` and
/// backslash escapes. Emphasis nests (`**bold with `code`**`). Markers count
/// only when they enclose non-blank text on the same line, so `2 * 3` and a
/// lone backtick stay literal; underscores are left alone so snake_case
/// identifiers survive.
fn inline(text: &str, base: Style, c: InlineColors) -> Vec<Seg> {
    let cs: Vec<char> = text.chars().collect();
    let n = cs.len();
    let mut segs: Vec<Seg> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, segs: &mut Vec<Seg>| {
        if !buf.is_empty() {
            segs.push((std::mem::take(buf), base));
        }
    };
    let find = |from: usize, pat: &[char]| (from..n.saturating_sub(pat.len() - 1)).find(|&j| cs[j..j + pat.len()] == *pat);
    let mut i = 0;
    while i < n {
        let ch = cs[i];
        // A backslash before punctuation shows the punctuation itself.
        if ch == '\\' && i + 1 < n && cs[i + 1].is_ascii_punctuation() {
            buf.push(cs[i + 1]);
            i += 2;
            continue;
        }
        // Code: a run of k backticks up to the next run of exactly k; the
        // inside is literal, one padding space trimmed from each end.
        if ch == '`' {
            let k = cs[i..].iter().take_while(|&&x| x == '`').count();
            let mut j = i + k;
            let close = loop {
                match (j..n).find(|&x| cs[x] == '`') {
                    None => break None,
                    Some(s) => {
                        let run = cs[s..].iter().take_while(|&&x| x == '`').count();
                        if run == k {
                            break Some(s);
                        }
                        j = s + run;
                    }
                }
            };
            if let Some(close) = close.filter(|&s| s > i + k) {
                let inner: String = cs[i + k..close].iter().collect();
                let inner = if inner.len() > 2 && inner.starts_with(' ') && inner.ends_with(' ') {
                    inner[1..inner.len() - 1].to_string()
                } else {
                    inner
                };
                flush(&mut buf, &mut segs);
                segs.push((inner, base.fg(c.code)));
                i = close + k;
                continue;
            }
            buf.extend(&cs[i..i + k]);
            i += k;
            continue;
        }
        // Emphasis and strikethrough: the longest marker that closes wins.
        if ch == '*' || ch == '~' {
            let run = cs[i..].iter().take_while(|&&x| x == ch).count();
            let options: &[usize] = if ch == '*' { &[3, 2, 1] } else { &[2] };
            let mut done = false;
            for &m in options.iter().filter(|&&m| m <= run) {
                let start = i + m;
                if start >= n || cs[start] == ' ' {
                    continue;
                }
                let pat = vec![ch; m];
                if let Some(close) = find(start, &pat) {
                    if close > start && cs[close - 1] != ' ' {
                        let modi = match (ch, m) {
                            ('~', _) => Modifier::CROSSED_OUT,
                            (_, 3) => Modifier::BOLD | Modifier::ITALIC,
                            (_, 2) => Modifier::BOLD,
                            _ => Modifier::ITALIC,
                        };
                        flush(&mut buf, &mut segs);
                        let inner: String = cs[start..close].iter().collect();
                        segs.extend(inline(&inner, base.add_modifier(modi), c));
                        i = close + m;
                        done = true;
                        break;
                    }
                }
            }
            if done {
                continue;
            }
            buf.extend(&cs[i..i + run]);
            i += run;
            continue;
        }
        // [text](url) and ![alt](url): the text underlined, the address
        // dimmed after it (a terminal can't hide a link behind its text).
        if ch == '[' || (ch == '!' && cs.get(i + 1) == Some(&'[')) {
            let open = if ch == '!' { i + 1 } else { i };
            if let Some(mid) = find(open + 1, &[']', '(']) {
                if let Some(end) = find(mid + 2, &[')']) {
                    let label: String = cs[open + 1..mid].iter().collect();
                    let url: String = cs[mid + 2..end].iter().collect();
                    if !label.contains('\n') && !url.contains(' ') {
                        flush(&mut buf, &mut segs);
                        let label = if label.is_empty() { url.clone() } else { label };
                        segs.extend(inline(&label, base.add_modifier(Modifier::UNDERLINED), c));
                        if !url.is_empty() && url != label {
                            segs.push((format!(" ({url})"), base.fg(c.dim)));
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        // <https://…> autolinks lose their brackets.
        if ch == '<' {
            if let Some(end) = find(i + 1, &['>']) {
                let url: String = cs[i + 1..end].iter().collect();
                if (url.starts_with("http://") || url.starts_with("https://")) && !url.contains(' ') {
                    flush(&mut buf, &mut segs);
                    segs.push((url, base.add_modifier(Modifier::UNDERLINED)));
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(ch);
        i += 1;
    }
    flush(&mut buf, &mut segs);
    segs
}

// ---------------------------------------------------------------- layout

/// Greedy word-wrap styled runs to `wrap_w` display columns, keeping each
/// run's style. Spaces at a break are dropped; a word wider than the line is
/// split.
fn wrap(segs: Vec<Seg>, wrap_w: usize) -> Vec<Vec<Seg>> {
    let wrap_w = wrap_w.max(1);
    // Split each run into space / non-space tokens, keeping its style.
    let mut toks: Vec<(String, Style, bool)> = Vec::new();
    for (text, st) in segs {
        let mut cur = String::new();
        let mut cur_space = false;
        for ch in text.chars() {
            let sp = ch == ' ';
            if !cur.is_empty() && sp != cur_space {
                toks.push((std::mem::take(&mut cur), st, cur_space));
            }
            cur.push(ch);
            cur_space = sp;
        }
        if !cur.is_empty() {
            toks.push((cur, st, cur_space));
        }
    }

    let mut lines: Vec<Vec<Seg>> = Vec::new();
    let mut line: Vec<Seg> = Vec::new();
    let mut w = 0usize;
    for (text, st, is_space) in toks {
        let tw = text.width();
        if is_space {
            if w == 0 {
                continue; // a row never starts with the space it broke at
            }
            if w + tw > wrap_w {
                lines.push(std::mem::take(&mut line));
                w = 0;
            } else {
                line.push((text, st));
                w += tw;
            }
            continue;
        }
        let chunks = if tw > wrap_w { hard_split(&text, wrap_w) } else { vec![text] };
        for chunk in chunks {
            let cw = chunk.width();
            if w + cw > wrap_w && w > 0 {
                // Don't leave the space that preceded this word dangling.
                if line.last().is_some_and(|(t, _)| t.starts_with(' ')) {
                    line.pop();
                }
                lines.push(std::mem::take(&mut line));
                w = 0;
            }
            line.push((chunk, st));
            w += cw;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Split text into chunks no wider than `w` display columns.
fn hard_split(word: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cw = 0usize;
    for ch in word.chars() {
        let chw = ch.width().unwrap_or(0);
        if cw + chw > w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cw = 0;
        }
        cur.push(ch);
        cw += chw;
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// Emit rows under the assistant glyph: the glyph on the message's first row
/// (then `lead` is spent), its alignment padding on every other. The block's
/// first row gets `first` after that, its wrapped rows `cont`, which is how
/// list items hang and quotes keep their bar.
fn emit(out: &mut Vec<Line<'static>>, cx: &Ctx, lead: &mut bool, first: Vec<Seg>, cont: Vec<Seg>, rows: Vec<Vec<Seg>>) {
    let (indent, glyph) = layout(Kind::Assistant, cx.g);
    let (_, glyph_style) = palette::colors(Kind::Assistant, cx.p);
    for (j, segs) in rows.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if *lead {
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            if !glyph.is_empty() {
                spans.push(Span::styled(glyph.to_string(), glyph_style));
            }
            *lead = false;
        } else if !segs.is_empty() || !first.is_empty() {
            // A blank row stays empty rather than a run of padding.
            spans.push(Span::raw(" ".repeat(cx.gutter())));
        }
        let prefix = if j == 0 { &first } else { &cont };
        spans.extend(prefix.iter().filter(|(t, _)| !t.is_empty()).map(|(t, s)| Span::styled(t.clone(), *s)));
        spans.extend(segs.into_iter().map(|(t, s)| Span::styled(t, s)));
        out.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::types::{GLYPHS_A, GLYPHS_U};

    fn colors() -> InlineColors {
        InlineColors { code: Color::Cyan, dim: Color::DarkGray }
    }

    fn parse(text: &str) -> Vec<Seg> {
        inline(text, Style::default(), colors())
    }

    fn texts(segs: &[Seg]) -> Vec<&str> {
        segs.iter().map(|(t, _)| t.as_str()).collect()
    }

    /// Render with the ASCII glyphs (`* ` lead) and flatten each row.
    fn render(md: &str, width: usize) -> Vec<String> {
        render_with(md, width, GLYPHS_A)
    }

    fn render_with(md: &str, width: usize, g: Glyphs) -> Vec<String> {
        let p = palette::palette_by_name("Rainbow");
        let mut out = Vec::new();
        render_message(&mut out, md, true, width, g, &p, false);
        out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn inline_bold_italic_code() {
        let segs = parse("a **b** c *d* `e`");
        assert_eq!(texts(&segs), vec!["a ", "b", " c ", "d", " ", "e"]);
        assert!(segs[1].1.add_modifier.contains(Modifier::BOLD));
        assert!(segs[3].1.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(segs[5].1.fg, Some(Color::Cyan));
    }

    #[test]
    fn inline_leaves_literals_and_identifiers_alone() {
        // Lone or spaced asterisks and snake_case underscores are not emphasis.
        assert_eq!(texts(&parse("2 * 3")).concat(), "2 * 3");
        assert_eq!(texts(&parse("a * b * c")).concat(), "a * b * c");
        assert_eq!(parse("snake_case_name").len(), 1);
        // Unterminated markers stay literal.
        assert_eq!(texts(&parse("`unterminated and **also")).concat(), "`unterminated and **also");
    }

    #[test]
    fn emphasis_nests_and_strikes() {
        let segs = parse("**bold `code`** ~~gone~~ ***both***");
        assert_eq!(texts(&segs), vec!["bold ", "code", " ", "gone", " ", "both"]);
        // Code inside bold is both.
        assert_eq!(segs[1].1.fg, Some(Color::Cyan));
        assert!(segs[1].1.add_modifier.contains(Modifier::BOLD));
        assert!(segs[3].1.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(segs[5].1.add_modifier.contains(Modifier::BOLD | Modifier::ITALIC));
    }

    #[test]
    fn double_backtick_code_can_hold_a_backtick() {
        assert_eq!(texts(&parse("``a ` b``")), vec!["a ` b"]);
        assert_eq!(texts(&parse("`` `x` ``")), vec!["`x`"]);
    }

    #[test]
    fn links_show_their_text_then_the_address() {
        let segs = parse("see [the docs](https://x.dev/a) and <https://y.dev>");
        assert_eq!(texts(&segs), vec!["see ", "the docs", " (https://x.dev/a)", " and ", "https://y.dev"]);
        assert!(segs[1].1.add_modifier.contains(Modifier::UNDERLINED));
        // A link whose text is its address isn't printed twice.
        assert_eq!(texts(&parse("[https://z.dev](https://z.dev)")), vec!["https://z.dev"]);
        // Brackets that aren't a link stay as typed.
        assert_eq!(texts(&parse("arr[0] (x)")).concat(), "arr[0] (x)");
    }

    #[test]
    fn backslash_escapes_show_the_character() {
        assert_eq!(texts(&parse(r"\*not italic\*")).concat(), "*not italic*");
    }

    #[test]
    fn message_splits_lines_and_hides_code_fence() {
        let rows = render("para one\n\n```\ncode\n```\ndone", 40);
        assert_eq!(rows, vec!["* para one", "", "  code", "  done"]);
    }

    /// The bug the screenshot showed: nested items and code lost their
    /// indentation, flattening structure the model wrote on purpose.
    #[test]
    fn nested_lists_and_code_keep_their_indentation() {
        let rows = render("- top\n  - nested\n    - deeper", 40);
        assert_eq!(rows, vec!["* - top", "    - nested", "      - deeper"]);
        let rows = render("```rust\nfn main() {\n    let x = 1;\n}\n```", 40);
        assert_eq!(rows, vec!["* fn main() {", "      let x = 1;", "  }"]);
    }

    #[test]
    fn nested_bullets_change_shape_with_unicode_glyphs() {
        let rows = render_with("- a\n  - b\n    - c", 40, GLYPHS_U);
        assert_eq!(rows, vec!["● • a", "    ◦ b", "      ▪ c"]);
    }

    #[test]
    fn a_fence_inside_a_list_item_drops_only_the_items_indent() {
        let rows = render("1. step\n   ```\n   cargo build\n     --release\n   ```", 40);
        assert_eq!(rows, vec!["* 1. step", "  cargo build", "    --release"]);
    }

    #[test]
    fn long_code_lines_wrap_without_losing_characters() {
        let rows = render("```\n    abcdefghijkl\n```", 10);
        // 8 columns after the glyph: 4 of indent, 4 of code per row.
        assert_eq!(rows, vec!["*     abcd", "      efgh", "      ijkl"]);
    }

    #[test]
    fn wrapped_list_items_hang_under_the_text() {
        let rows = render("- alpha beta gamma delta epsilon", 24);
        assert_eq!(rows, vec!["* - alpha beta gamma", "    delta epsilon"]);
        let rows = render("10. alpha beta gamma delta", 20);
        assert_eq!(rows, vec!["* 10. alpha beta", "      gamma delta"]);
    }

    #[test]
    fn wrapped_prose_aligns_under_the_glyph() {
        let rows = render("alpha beta gamma delta epsilon zeta", 24);
        assert_eq!(rows, vec!["* alpha beta gamma delta", "  epsilon zeta"]);
    }

    #[test]
    fn task_lists_quotes_and_rules() {
        let rows = render("- [ ] todo\n- [x] done", 40);
        assert_eq!(rows, vec!["* - [ ] todo", "  - [x] done"]);
        let rows = render_with("- [x] done", 40, GLYPHS_U);
        assert_eq!(rows, vec!["● • ☑ done"]);
        let rows = render("> quoted words here\n> > deeper", 16);
        assert_eq!(rows, vec!["* | quoted words", "  | here", "  | | deeper"]);
        assert_eq!(render("---", 10), vec!["* --------"]);
        // `- item` is a list, not a rule.
        assert_eq!(render("- x", 10), vec!["* - x"]);
    }

    #[test]
    fn headings_drop_their_markers() {
        assert_eq!(render("## Title ##", 40), vec!["* Title"]);
        assert_eq!(render("#nospace", 40), vec!["* #nospace"]);
        assert_eq!(heading("####### too many"), None);
    }

    #[test]
    fn tables_draw_in_a_box_with_aligned_columns() {
        let md = "| Name | Size |\n|:-----|-----:|\n| a | 1 |\n| `bb` | 200 |";
        let rows = render_with(md, 40, GLYPHS_U);
        assert_eq!(
            rows,
            vec![
                "● ┌──────┬──────┐",
                "  │ Name │ Size │",
                "  ├──────┼──────┤",
                "  │ a    │    1 │",
                "  │ bb   │  200 │",
                "  └──────┴──────┘",
            ]
        );
    }

    #[test]
    fn a_wide_table_shrinks_its_widest_column_and_wraps() {
        let md = "| k | v |\n|---|---|\n| x | one two three four |";
        let rows = render(md, 20);
        assert_eq!(
            rows,
            vec![
                "* +---+------------+",
                "  | k | v          |",
                "  +---+------------+",
                "  | x | one two    |",
                "  |   | three four |",
                "  +---+------------+",
            ]
        );
    }

    #[test]
    fn a_table_too_narrow_for_its_columns_stays_text() {
        let md = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |";
        let rows = render(md, 12);
        assert_eq!(rows[..2], ["* | a | b |", "  c |"]);
    }

    #[test]
    fn a_table_header_without_its_delimiter_yet_is_text() {
        // Mid-stream: the delimiter row hasn't arrived.
        assert_eq!(render("| a | b |", 40), vec!["* | a | b |"]);
    }

    #[test]
    fn cells_respect_code_spans_and_escaped_pipes() {
        assert_eq!(cells("| `a|b` | c\\|d |"), vec!["`a|b`", "c|d"]);
        assert_eq!(cells("x | y"), vec!["x", "y"]);
    }

    #[test]
    fn an_unterminated_fence_renders_what_streamed_so_far() {
        assert_eq!(render("```\nlet a = 1;", 40), vec!["* let a = 1;"]);
    }

    #[test]
    fn wrap_keeps_styles_and_hard_splits_long_words() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let lines = wrap(vec![("hello ".into(), Style::default()), ("world".into(), bold)], 8);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], vec![("world".to_string(), bold)]);
        let lines = wrap(vec![("abcdefghij".into(), Style::default())], 4);
        let joined: Vec<String> = lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert_eq!(joined, vec!["abcd", "efgh", "ij"]);
    }
}
