//! Banner art: the HAIKU CODE logo rendered as block art (Unicode or ASCII),
//! assembled into lines with roles so the TUI and `--banner` flag can
//! render the same layout identically.

use super::palette::{self};
use super::types::{BannerColor, Kind, TLine};

pub const TAGLINE: &str = "an agentic coding CLI for Haiku";

/// 3x5 block glyphs for "HAIKU CODE" (the blank is the word gap). Small enough
/// that the heavy, double-width logo still fits Haiku Terminal's default 80
/// columns; adding a letter is a one-line change.
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

/// The glyph table as a grid of cells, one column between glyphs.
fn art_cells() -> Vec<Vec<bool>> {
    (0..5)
        .map(|r| {
            let row: Vec<&str> = ART_GLYPHS.iter().map(|g| g[r]).collect();
            row.join(" ").chars().map(|c| c == '#').collect()
        })
        .collect()
}

/// Heavy Unicode logo: every cell two columns wide, with a `░` drop shadow one
/// cell down and to the right of each filled cell.
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

/// Pick the widest HAIKU CODE art that fits in `w` columns.
fn banner_art(w: usize, ascii: bool) -> Vec<String> {
    let heavy = art_heavy();
    let heavy_w = heavy.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let narrow_w = art_cells()[0].len();
    if !ascii && w >= heavy_w {
        heavy
    } else if w > narrow_w {
        let mut art = art_glyphs(if ascii { '#' } else { '█' });
        // ASCII gets a drop-shadow row; Unicode non-heavy gets none (too narrow).
        if ascii {
            art.push(ascii_shadow_row());
        }
        art
    } else {
        vec!["H A I K U   C O D E".to_string()]
    }
}

/// Role of a banner line, so the TUI and the ANSI `--banner` preview color the
/// same layout identically.
#[derive(Clone, Copy)]
pub enum BRole {
    Art(usize), // rainbow/mono block-art row
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
    let art = banner_art(w, ascii);
    let artw = art.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let art_pad = " ".repeat(w.saturating_sub(artw) / 2);

    let mut out = Vec::new();
    // Breathing room between the box title (working path) and the logo.
    out.push(BLine { text: String::new(), role: BRole::Frame });
    for (i, l) in art.iter().enumerate() {
        out.push(BLine { text: format!("{art_pad}{l}"), role: BRole::Art(i) });
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
            // Same cells-not-glyphs logo as the TUI (see helpers::art_spans),
            // drawn as reverse-video spaces.
            BRole::Art(i) => {
                let c = palette::banner_row_color(&p, &rainbow, i, sixteen);
                let (fg, shadow) = (palette::ansi_fg(c), palette::ansi_fg(palette::shadow_color(c, p.bg, sixteen)));
                bl.text.replace('█', "\x1b[7m \x1b[27m").replace('░', &format!("{shadow}\x1b[7m \x1b[27m{fg}"))
            }
            _ => bl.text,
        };
        let prefix = match bl.role {
            BRole::Art(i) => palette::ansi_fg(palette::banner_row_color(&p, &rainbow, i, sixteen)),
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
            BRole::Art(i) => Some(BannerColor::Rainbow(i)),
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

    /// Haiku Terminal opens at 80 columns; the TUI leaves 76 for the banner.
    #[test]
    fn the_heavy_logo_fits_an_80_column_terminal() {
        let art = banner_art(76, false);
        assert!(art[0].contains('█') && art.iter().any(|l| l.contains('░')), "{art:?}");
        assert!(art.iter().all(|l| l.chars().count() <= 76), "{art:?}");
        assert_eq!(art.len(), 6, "five glyph rows plus the shadow row");
    }

    #[test]
    fn narrow_terminals_fall_back_to_smaller_art() {
        let small = banner_art(50, false);
        assert!(small.iter().all(|l| !l.contains('░')) && small[0].contains('█'), "{small:?}");
        let ascii = banner_art(76, true);
        assert!(ascii.iter().all(|l| l.is_ascii()) && ascii.last().unwrap().contains('.'), "{ascii:?}");
        assert_eq!(banner_art(20, false), ["H A I K U   C O D E"]);
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
