//! `/statusline`: which readouts the first status row shows, and in what
//! order. Saved in config.json as `statusline` (a list of segment ids); unset
//! or empty means `DEFAULT`, the row as it always was. Everything here is
//! cheap to draw: no segment runs a process per frame (git is the cached,
//! throttled probe the output title already uses).

/// Every segment, in the picker's order, with what it shows.
pub const SEGMENTS: &[(&str, &str)] = &[
    ("model", "model name"),
    ("effort", "reasoning effort, when not auto"),
    ("cost", "session cost"),
    ("tokens", "session tokens"),
    ("ctx", "context window bar"),
    ("balance", "provider account balance"),
    ("git", "git branch, * when dirty"),
    ("cwd", "working directory"),
    ("time", "clock, HH:MM"),
    ("version", "Haiku Code version"),
];

/// The row before `/statusline` existed.
pub const DEFAULT: &[&str] = &["model", "effort", "cost", "tokens", "ctx", "balance"];

/// Segments that join the one before them with a joiner instead of the bar
/// separator when that one is shown: `model effort:high`, `$0.01 · 12k tok`.
pub fn glue(id: &str, prev: &str) -> Option<&'static str> {
    match (prev, id) {
        ("model", "effort") => Some(" "),
        ("cost", "tokens") => Some(" · "),
        _ => None,
    }
}

pub fn is_segment(id: &str) -> bool {
    SEGMENTS.iter().any(|(s, _)| *s == id)
}

/// The segments to draw for a saved setting (unknown ids from a newer or
/// hand-edited config are skipped; nothing usable left means the default).
pub fn effective(saved: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in saved {
        if is_segment(s) && !out.contains(s) {
            out.push(s.clone());
        }
    }
    if out.is_empty() {
        DEFAULT.iter().map(|s| s.to_string()).collect()
    } else {
        out
    }
}

/// Parse `/statusline <ids…>` (spaces or commas). Errors name the bad id.
pub fn parse(arg: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for w in arg.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()) {
        let w = w.to_ascii_lowercase();
        if !is_segment(&w) {
            let names: Vec<&str> = SEGMENTS.iter().map(|(s, _)| *s).collect();
            return Err(format!("unknown segment '{w}' (choose from: {})", names.join(" ")));
        }
        if !out.contains(&w) {
            out.push(w);
        }
    }
    if out.is_empty() {
        return Err("name at least one segment".into());
    }
    Ok(out)
}

/// The picker's rows: the shown segments in their order, then the hidden
/// ones in catalog order.
pub fn picker_items(shown: &[String]) -> Vec<(String, bool)> {
    let mut items: Vec<(String, bool)> = shown.iter().map(|s| (s.clone(), true)).collect();
    for (s, _) in SEGMENTS {
        if !shown.iter().any(|x| x == s) {
            items.push((s.to_string(), false));
        }
    }
    items
}

pub fn describe(id: &str) -> &'static str {
    SEGMENTS.iter().find(|(s, _)| *s == id).map(|(_, d)| *d).unwrap_or("")
}

/// Local wall-clock time as `HH:MM`, from libc (no date crate).
pub fn local_hhmm() -> String {
    let (h, m) = local_hm();
    format!("{h:02}:{m:02}")
}

/// Local (hour, minute). Also used to redraw the clock when the minute turns.
pub fn local_hm() -> (i32, i32) {
    // SAFETY: time(NULL) and localtime_r into a zeroed, owned tm.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return (0, 0);
        }
        (tm.tm_hour, tm.tm_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_unusable_settings_fall_back_to_the_old_row() {
        let def: Vec<String> = DEFAULT.iter().map(|s| s.to_string()).collect();
        assert_eq!(effective(&[]), def);
        assert_eq!(effective(&["nope".into()]), def);
        assert_eq!(effective(&["git".into(), "bogus".into(), "git".into(), "ctx".into()]), ["git", "ctx"]);
    }

    #[test]
    fn typed_segments_parse_in_order_and_bad_ones_are_named() {
        assert_eq!(parse("model, git  CTX").unwrap(), ["model", "git", "ctx"]);
        assert_eq!(parse("time time").unwrap(), ["time"]);
        let e = parse("model gti").unwrap_err();
        assert!(e.contains("'gti'") && e.contains("git"), "{e}");
        assert!(parse("  ").is_err());
    }

    #[test]
    fn the_picker_lists_shown_segments_first_then_the_rest() {
        let items = picker_items(&["git".into(), "model".into()]);
        assert_eq!(items[0], ("git".into(), true));
        assert_eq!(items[1], ("model".into(), true));
        assert_eq!(items.len(), SEGMENTS.len());
        assert!(items[2..].iter().all(|(_, on)| !on));
    }

    #[test]
    fn the_clock_reads_as_hh_mm() {
        let t = local_hhmm();
        assert_eq!(t.len(), 5);
        assert_eq!(&t[2..3], ":");
    }
}
