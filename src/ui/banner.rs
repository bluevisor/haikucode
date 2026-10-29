//! Banner art: the HAIKU CODE logo rendered as block art (Unicode or ASCII),
//! assembled into lines with roles so the TUI and `--banner` flag can
//! render the same layout identically.

use super::palette::{self};
use super::types::{BannerColor, Kind, TLine};

/// Names the OS it's running on.
pub const TAGLINE: &str = if cfg!(target_os = "macos") {
    "an agentic coding CLI for macOS"
} else if cfg!(target_os = "linux") {
    "an agentic coding CLI for Linux"
} else {
    "an agentic coding CLI for Haiku"
};

/// 3x5 block glyphs for "HAIKU CODE" (the blank is the word gap). Drawn with
/// half blocks, two pixel rows per text row, the logo is 37x3 cells; adding a
/// letter is a one-line change.
const ART_GLYPHS: [&[&str; 5]; 10] = [
    &["# #", "# #", "###", "# #", "# #"], // H
    &["###", "# #", "###", "# #", "# #"], // A
    &["###", " # ", " # ", " # ", "###"], // I
    &["# #", "# #", "## ", "# #", "# #"], // K
    &["# #", "# #", "# #", "# #", "###"], // U
    &[" ", " ", " ", " ", " "],           // word gap
    &["###", "#  ", "#  ", "#  ", "###"], // C
    &["###", "# #", "# #", "# #", "###"], // O
    &["## ", "# #", "# #", "# #", "## "], // D
    &["###", "#  ", "## ", "#  ", "###"], // E
];

/// Whether the Unicode logo is drawn with `▀`/`▄` half blocks. Haiku Terminal's
/// default font has no block elements and its fallback glyphs are wider than a
/// cell, so there the logo stays one background-colored cell per pixel.
pub const HALF_BLOCKS: bool = !cfg!(target_os = "haiku");

/// The glyph table as a grid of cells, one column between glyphs.
fn art_cells() -> Vec<Vec<bool>> {
    (0..5)
        .map(|r| {
            let row: Vec<&str> = ART_GLYPHS.iter().map(|g| g[r]).collect();
            row.join(" ").chars().map(|c| c == '#').collect()
        })
        .collect()
}

/// Heavy Unicode logo for very wide terminals: every cell two columns wide,
/// with a `░` drop shadow one cell down and to the right of each filled cell.
fn art_heavy() -> Vec<String> {
    let cells = art_cells();
    let (rows, cols) = (cells.len(), cells[0].len());
    let filled = |r: usize, c: usize| r < rows && c < cols && cells[r][c];
    (0..=rows)
        .map(|r| {
            let line: String = (0..=cols)
                .map(|c| {
                    if filled(r, c) {
                        "██"
                    } else if r > 0 && c > 0 && filled(r - 1, c - 1) {
                        "░░"
                    } else {
                        "  "
                    }
                })
                .collect();
            line.trim_end().to_string()
        })
        .collect()
}

/// Banner width from which the heavy logo replaces the small one.
const HEAVY_MIN_WIDTH: usize = 150;

/// Half-block logo: pixel rows packed in pairs, `▀` top only, `▄` bottom
/// only, `█` both (the renderers color the two halves separately).
fn art_half() -> Vec<String> {
    let cells = art_cells();
    cells
        .chunks(2)
        .map(|pair| {
            let bottom = |c: usize| pair.get(1).is_some_and(|r| r[c]);
            let line: String = (0..pair[0].len())
                .map(|c| match (pair[0][c], bottom(c)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect();
            line.trim_end().to_string()
        })
        .collect()
}

/// Single-width logo, `#`→`fill`.
fn art_glyphs(fill: char) -> Vec<String> {
    art_cells()
        .iter()
        .map(|row| row.iter().map(|&on| if on { fill } else { ' ' }).collect::<String>().trim_end().to_string())
        .collect()
}

/// Drop-shadow row for the ASCII art: the bottom row shifted right by one
/// column, drawn with `.`.
fn ascii_shadow_row() -> String {
    let cells = art_cells();
    let bottom = &cells[cells.len() - 1];
    let mut shadow = vec![' '; bottom.len() + 1];
    for (i, &on) in bottom.iter().enumerate() {
        if on {
            shadow[i + 1] = '.';
        }
    }
    shadow.into_iter().collect::<String>().trim_end().to_string()
}

/// The HAIKU CODE art for `w` columns, or the name spelled out when it won't
/// fit, and whether it's half blocks (two pixel rows per line).
fn banner_art(w: usize, ascii: bool) -> (Vec<String>, bool) {
    if w < art_cells()[0].len() {
        (vec!["H A I K U   C O D E".to_string()], false)
    } else if ascii {
        let mut art = art_glyphs('#');
        art.push(ascii_shadow_row());
        (art, false)
    } else if w >= HEAVY_MIN_WIDTH {
        (art_heavy(), false)
    } else if HALF_BLOCKS {
        (art_half(), true)
    } else {
        (art_glyphs('█'), false)
    }
}

/// Role of a banner line, so the TUI and the ANSI `--banner` preview color the
/// same layout identically.
#[derive(Clone, Copy)]
pub enum BRole {
    Art(usize, bool), // rainbow/mono block-art row by its top pixel row; half blocks?
    Version,    // bold accent
    Tagline,    // dim
    Frame,      // dim panel rule / blank
    Data,       // accent status line
}

pub struct BLine {
    pub text: String,
    pub role: BRole,
}

/// Build the launch banner as structured lines: centered block art, a version
/// + tagline, and a bordered SYSTEM panel wrapping the status lines.
pub fn banner_lines(w: usize, ascii: bool, status: &[String]) -> Vec<BLine> {
    let center = |s: &str| {
        let pad = " ".repeat(w.saturating_sub(s.chars().count()) / 2);
        format!("{pad}{s}")
    };
    let (art, half) = banner_art(w, ascii);
    let artw = art.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let art_pad = " ".repeat(w.saturating_sub(artw) / 2);

    let mut out = Vec::new();
    // Breathing room between the box title (working path) and the logo.
    out.push(BLine { text: String::new(), role: BRole::Frame });
    for (i, l) in art.iter().enumerate() {
        out.push(BLine { text: format!("{art_pad}{l}"), role: BRole::Art(if half { 2 * i } else { i }, half) });
    }
    out.push(BLine { text: String::new(), role: BRole::Frame });
    out.push(BLine { text: center(&format!("Haiku Code v{}", env!("CARGO_PKG_VERSION"))), role: BRole::Version });
    out.push(BLine { text: center(TAGLINE), role: BRole::Tagline });
    out.push(BLine { text: String::new(), role: BRole::Frame });

    let (tl, tr, bl, br, h, vbar) = if ascii {
        ("+", "+", "+", "+", "-", "|")
    } else {
        ("┌", "┐", "└", "┘", "─", "│")
    };
    // Top border: ┌─ SYSTEM ───...──┐
    let head = format!("{tl}{h} SYSTEM ");
    let fill = w.saturating_sub(head.chars().count() + 1); // +1 for right corner
    out.push(BLine { text: format!("{head}{}{tr}", h.repeat(fill)), role: BRole::Frame });
    // Status lines: │ text ... │, wrapped inside the box with continuation rows
    // indented past the 5-column label ("HW   ") so they never poke out of it.
    let inner = w.saturating_sub(4).max(8); // 4 = "│ " + " │"
    let opts = textwrap::Options::new(inner).subsequent_indent("     ");
    for line in status {
        for (i, row) in textwrap::wrap(line, &opts).into_iter().enumerate() {
            // A break before a " · " separator would start the row with it.
            let row = match (i, row.trim_start().strip_prefix("· ").or_else(|| row.trim_start().strip_prefix("- "))) {
                (1.., Some(rest)) => format!("     {rest}"),
                _ => row.into_owned(),
            };
            let pad = w.saturating_sub(row.chars().count() + 3); // 3 = "│ " + "│"
            out.push(BLine { text: format!("{vbar} {row}{}{vbar}", " ".repeat(pad)), role: BRole::Data });
        }
    }
    // Bottom border: └───...──┘
    let bfill = w.saturating_sub(2); // bl + br = 2
    out.push(BLine { text: format!("{bl}{}{br}", h.repeat(bfill)), role: BRole::Frame });
    out
}

/// ANSI-colored banner for the `--banner` flag (a preview of the launch screen).
pub fn banner_ansi(width: u16, ascii: bool, theme: &str, status: &[String]) -> String {
    use super::types::is_16color_terminal;

    let p = palette::palette_by_name(theme);
    let w = (width as usize).saturating_sub(4).max(8);
    let sixteen = is_16color_terminal();
    let rainbow = if sixteen { palette::APPLE_RAINBOW_16 } else { palette::APPLE_RAINBOW };
    let reset = "\x1b[0m";

    let mut out = String::new();
    for bl in banner_lines(w, ascii, status) {
        let text = match bl.role {
            // Same logo as the TUI (see helpers::art_spans): half blocks with
            // the lower pixel row's color, or reverse-video spaces.
            BRole::Art(i, half) => {
                let c = palette::banner_row_color(&p, &rainbow, i, sixteen);
                let below = palette::banner_row_color(&p, &rainbow, i + 1, sixteen);
                let fg = palette::ansi_fg(c);
                if half {
                    let (fg_below, bg_below) = (palette::ansi_fg(below), palette::ansi_bg(below));
                    bl.text.replace('█', &format!("{bg_below}▀\x1b[49m")).replace('▄', &format!("{fg_below}▄{fg}"))
                } else {
                    let shadow = palette::ansi_fg(palette::shadow_color(c, p.bg, sixteen));
                    bl.text.replace('█', "\x1b[7m \x1b[27m").replace('░', &format!("{shadow}\x1b[7m \x1b[27m{fg}"))
                }
            }
            _ => bl.text,
        };
        let prefix = match bl.role {
            BRole::Art(i, _) => palette::ansi_fg(palette::banner_row_color(&p, &rainbow, i, sixteen)),
            // Accent/notice are the theme's own colors; on a 16-color console an
            // `Rgb` prefix is an escape the console drops, so the frame/data
            // text would inherit whatever color was last set.
            BRole::Version => format!("\x1b[1m{}", palette::ansi_fg(snap(p.accent, sixteen))),
            BRole::Tagline => palette::ansi_fg(snap(p.notice, sixteen)),
            BRole::Frame | BRole::Data => palette::ansi_fg(snap(p.accent, sixteen)),
        };
        out.push_str(&format!("{prefix}{text}{reset}\n"));
    }
    out
}

/// `nearest_16` only when this is a 16-color terminal, so a truecolor one keeps
/// the theme's exact shades.
fn snap(c: ratatui::style::Color, sixteen: bool) -> ratatui::style::Color {
    if sixteen {
        palette::nearest_16(c)
    } else {
        c
    }
}

/// Push banner lines into the transcript as `TLine`s.
#[allow(dead_code)]
pub fn push_banner_transcript(
    transcript: &mut Vec<TLine>,
    width: u16,
    ascii: bool,
    theme: &str,
    status: Vec<String>,
) {
    let w = (width as usize).saturating_sub(4).max(8);
    for bl in banner_lines(w, ascii, &status) {
        let color = match bl.role {
            BRole::Art(i, half) => Some(BannerColor::Rainbow(i, half)),
            BRole::Version | BRole::Data => Some(BannerColor::Accent),
            BRole::Tagline | BRole::Frame => Some(BannerColor::Fixed(
                palette::palette_by_name(theme).notice,
            )),
        };
        transcript.push(TLine {
            kind: if matches!(bl.role, BRole::Tagline | BRole::Frame) {
                Kind::BannerDim
            } else {
                Kind::Banner
            },
            text: bl.text,
            lead: true,
            color,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logo is opencode-sized: 37 columns, 3 half-block rows (5 cell rows
    /// on Haiku, where block glyphs smear).
    #[test]
    fn the_logo_is_small() {
        let (art, half) = banner_art(76, false);
        assert!(art.iter().all(|l| l.chars().count() <= 37), "{art:?}");
        assert_eq!((art.len(), half), if HALF_BLOCKS { (3, true) } else { (5, false) }, "{art:?}");
        assert!(art[0].contains('█'), "{art:?}");
    }

    /// Very wide terminals get the big double-width logo with its drop shadow.
    #[test]
    fn very_wide_terminals_get_the_heavy_logo() {
        let (art, half) = banner_art(HEAVY_MIN_WIDTH, false);
        assert!(!half && art.len() == 6 && art.iter().any(|l| l.contains('░')), "{art:?}");
        assert!(banner_art(HEAVY_MIN_WIDTH - 1, false).0.iter().all(|l| !l.contains('░')));
    }

    #[test]
    fn narrow_terminals_fall_back_to_text() {
        let (ascii, _) = banner_art(HEAVY_MIN_WIDTH, true);
        assert!(ascii.iter().all(|l| l.is_ascii()) && ascii.last().unwrap().contains('.'), "{ascii:?}");
        assert_eq!(banner_art(36, false).0, ["H A I K U   C O D E"]);
    }

    /// A long HW row wraps inside the SYSTEM box instead of running past its
    /// right border, and every box row is exactly the banner width.
    #[test]
    fn long_status_rows_wrap_inside_the_box() {
        let hw = "HW   Haiku R1~beta6+development (hrev60295) · aarch64 · 8 cores · MEM 831/16379MB".to_string();
        for w in [40, 60, 76, 120] {
            let lines = banner_lines(w, false, std::slice::from_ref(&hw));
            let boxed: Vec<_> = lines.iter().skip_while(|l| !l.text.contains("SYSTEM")).collect();
            assert!(boxed.iter().all(|l| l.text.chars().count() == w), "w={w}: {:?}", boxed.iter().map(|l| &l.text).collect::<Vec<_>>());
            let data: Vec<_> = boxed.iter().filter(|l| matches!(l.role, BRole::Data)).collect();
            assert_eq!(data.len() > 1, w < 90, "w={w}");
            assert!(data.iter().skip(1).all(|l| l.text.starts_with("│      ") && !l.text.starts_with("│      ·")), "continuations indent past the label, no leading separator");
        }
    }
}
