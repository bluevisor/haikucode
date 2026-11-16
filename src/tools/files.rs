//! File tools: read_file, list_files, write_file, edit/multi_edit and their previews.

use super::search::git_ignored_children;
use super::{deny_symlink, expand};
use crate::api::{truncate, MAX_TOOL_OUTPUT};
use std::io::Read;
use std::path::Path;

// ------------------------------------------------------------ read/list -----

pub fn read_file(path: &str, start: Option<u64>, end: Option<u64>) -> String {
    let p = expand(path);
    // Open with O_NOFOLLOW so symlinks are refused atomically — no TOCTOU
    // window between the check and the open.
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&p) {
            Ok(f) => f,
            Err(e) => {
                if let Ok(meta) = std::fs::symlink_metadata(&p) {
                    if meta.file_type().is_symlink() {
                        return format!("DENIED: {path} is a symlink; read the real path instead.");
                    }
                }
                return format!("ERROR: {e}");
            }
        }
    };
    #[cfg(not(unix))]
    let mut file = {
        if let Some(err) = deny_symlink(&p, path, "read") {
            return err;
        }
        match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(e) => return format!("ERROR: {e}"),
        }
    };
    // Open and read only up to READ_FILE_LIMIT bytes to avoid OOM on huge files.
    const READ_FILE_LIMIT: usize = 1_000_000; // 1 MB
    // Read a small probe first: if the file is binary, hex-dump it without
    // allocating the full 1 MB.
    let mut probe = [0u8; 4096];
    let probe_n = match std::io::Read::read(&mut file, &mut probe) {
        Ok(0) => return "(empty file)".into(),
        Ok(n) => n,
        Err(e) => return format!("ERROR: read failed: {e}"),
    };
    let is_binary = probe[..probe_n].contains(&0);
    let mut raw = probe[..probe_n].to_vec();
    if !is_binary {
        if let Err(e) = std::io::Read::by_ref(&mut file)
            .take((READ_FILE_LIMIT - probe_n) as u64)
            .read_to_end(&mut raw)
        {
            return format!("ERROR: read failed: {e}");
        }
    }
    let content = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => {
            // A file read right up to READ_FILE_LIMIT can be cut mid-codepoint;
            // that lone trailing partial char isn't "binary". If the only invalid
            // bytes are at the very end of a limit-sized read, keep the valid
            // prefix as text instead of hex-dumping a legit UTF-8 file.
            let valid_up_to = e.utf8_error().valid_up_to();
            let raw = e.into_bytes();
            if raw.len() >= READ_FILE_LIMIT && valid_up_to >= raw.len().saturating_sub(3) {
                // Safe: valid_up_to is by definition a char boundary.
                String::from_utf8_lossy(&raw[..valid_up_to]).into_owned()
            } else {
            // Binary: return a hex dump of the first 512 bytes.
            let preview = &raw[..raw.len().min(512)];
            let hex: String = preview
                .chunks(16)
                .enumerate()
                .map(|(i, chunk)| {
                    let off = format!("{:08x}", i * 16);
                    let bytes: String = chunk.iter().map(|b| format!(" {b:02x}")).collect();
                    let ascii: String = chunk
                        .iter()
                        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
                        .collect();
                    format!("{off} {bytes: <49} |{ascii}|\n")
                })
                .collect();
            return format!(
                "Binary file ({} bytes total, hex dump of first {} bytes):\n{hex}",
                raw.len(),
                preview.len()
            );
            }
        }
    };
    let truncated_read = content.len() >= READ_FILE_LIMIT;
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let (slice, off) = match (start, end) {
        (None, None) => (&lines[..], 1usize),
        (s, e) => {
            let s = s.unwrap_or(1).max(1) as usize;
            let e = e.map(|x| x as usize).unwrap_or(lines.len());
            // An out-of-range or inverted range is an error, not "(empty file)"
            // — the model must not conclude the file is blank and overwrite it.
            if s > lines.len() && !lines.is_empty() {
                return format!("ERROR: start_line {s} is beyond the end of the file ({} lines).", lines.len());
            }
            if e < s {
                return format!("ERROR: end_line {e} is before start_line {s}.");
            }
            let lo = (s - 1).min(lines.len());
            let hi = e.min(lines.len()).max(lo);
            (&lines[lo..hi], s)
        }
    };
    if slice.is_empty() {
        return "(empty file)".into();
    }
    let mut out = String::new();
    for (i, ln) in slice.iter().enumerate() {
        // A tab, like `cat -n`: with spaces the model mistook the separator
        // for indentation and sent edit_file old_text indented 2 too deep.
        out.push_str(&format!("{:>5}\t{}", off + i, ln));
        if !ln.ends_with('\n') {
            out.push('\n');
        }
    }
    if truncated_read {
        out.push_str("... (file truncated at 1 MB)\n");
    }
    truncate(&out, MAX_TOOL_OUTPUT)
}

pub fn list_files(path: &str) -> String {
    let p = expand(if path.is_empty() { "." } else { path });
    if let Some(err) = deny_symlink(&p, path, "list") {
        return err;
    }
    let entries = match std::fs::read_dir(&p) {
        Ok(e) => e,
        Err(e) => return format!("ERROR: {e}"),
    };
    const MAX_ENTRIES: usize = 10_000;
    let mut names: Vec<String> = Vec::new();
    let mut unreadable = 0u32;
    for entry in entries {
        match entry {
            Ok(e) => {
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                names.push(if is_dir { format!("{name}/") } else { name });
                if names.len() >= MAX_ENTRIES {
                    names.push("... (directory listing truncated at 10 000 entries)".into());
                    break;
                }
            }
            Err(_) => {
                unreadable += 1;
            }
        }
    }
    names.sort();
    // Leave out what .gitignore excludes, but say what was left out, so the
    // model doesn't conclude a build dir is missing.
    if let Some(ignored) = git_ignored_children(&p) {
        let hidden: Vec<String> = names.iter().filter(|n| ignored.contains(*n)).cloned().collect();
        if !hidden.is_empty() {
            names.retain(|n| !ignored.contains(n));
            let shown: Vec<&str> = hidden.iter().take(10).map(String::as_str).collect();
            let more = if hidden.len() > 10 { ", …" } else { "" };
            names.push(format!("({} git-ignored not shown: {}{more})", hidden.len(), shown.join(", ")));
        }
    }
    if unreadable > 0 {
        let s = if unreadable == 1 { "" } else { "s" };
        names.push(format!("({unreadable} unreadable entry{s})"));
    }
    // Pin the notes (truncation, ignored, unreadable) to the very bottom.
    let note = |s: &str| s.starts_with("...") || s.starts_with('(');
    names.sort_by(|a, b| {
        match (note(a), note(b)) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => a.cmp(b),
        }
    });
    if names.is_empty() {
        "(empty)".into()
    } else {
        truncate(&names.join("\n"), MAX_TOOL_OUTPUT)
    }
}

// --------------------------------------------------------- write / edit -----

/// Compute a preview diff for an upcoming write (existing content vs new).
pub fn write_preview(path: &str, content: &str) -> (String, bool) {
    let p = expand(path);
    let existed = p.exists();
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    (crate::diff::unified(&old, content, 300), existed)
}

pub fn write_file(path: &str, content: &str) -> String {
    let p = expand(path);
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return format!("ERROR: {e}");
            }
        }
    }
    // O_NOFOLLOW: refuse to write through symlinks atomically.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .write(true).create(true).truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&p)
        {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(content.as_bytes()) {
                    return format!("ERROR: {e}");
                }
            }
            Err(e) => {
                // Distinguish symlink denial from other errors.
                if let Ok(meta) = std::fs::symlink_metadata(&p) {
                    if meta.file_type().is_symlink() {
                        return format!("DENIED: {path} is a symlink; write to the real path instead.");
                    }
                }
                return format!("ERROR: {e}");
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(err) = deny_symlink(&p, path, "write to") {
            return err;
        }
        match std::fs::write(&p, content) {
            Err(e) => return format!("ERROR: {e}"),
            _ => {}
        }
    }
    format!("OK wrote {} ({} bytes)", p.display(), content.len())
}

pub enum EditPreview {
    Ok { diff: String, new_content: String },
    Err(String),
}

/// One requested edit (a unique-substring replacement in a file).
#[derive(Debug)]
pub struct EditReq {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
}

/// A validated multi-file edit, ready to apply.
#[derive(Debug)]
pub struct MultiEditPlan {
    /// Combined diff across all touched files, for the approval preview.
    pub diff: String,
    /// Final content per file, in first-touched order.
    pub files: Vec<(String, String)>,
}

/// Validate a batch of edits without writing. Edits are applied to in-memory
/// copies in order, so several edits to the same file compose correctly. Any
/// failure (file missing, old_text absent or ambiguous) aborts the whole
/// batch — nothing is half-applied.
pub fn multi_edit_plan(edits: &[EditReq]) -> std::result::Result<MultiEditPlan, String> {
    if edits.is_empty() {
        return Err("ERROR: no edits provided".into());
    }
    let mut originals: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut current: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    // Key every map by the normalized path: `src/a.rs` and `./src/a.rs` are one
    // file, and two spellings used to load it twice and let the later write
    // clobber the earlier edit.
    let mut shown: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // An edit that changes nothing is skipped, not fatal: rejecting a whole
    // all-or-nothing batch over one no-op only makes the model resend it.
    if edits.iter().all(|e| e.old_text == e.new_text) {
        return Err("ERROR: every edit has identical old_text and new_text (no change).".into());
    }
    for (i, e) in edits.iter().enumerate() {
        if e.old_text == e.new_text {
            continue;
        }
        let key = expand(&e.path).to_string_lossy().into_owned();
        if !current.contains_key(&key) {
            let disk = read_for_edit(&expand(&e.path), &e.path)
                .map_err(|err| format!("{} (edit {}: {})", err.trim_end_matches('.'), i + 1, e.path))?;
            originals.insert(key.clone(), disk.clone());
            current.insert(key.clone(), disk);
            shown.insert(key.clone(), e.path.clone());
            order.push(key.clone());
        }
        let cur = current.get(&key).unwrap();
        let updated = splice_edit(cur, &e.old_text, &e.new_text, &format!("edit {} ({}): ", i + 1, e.path))?;
        current.insert(key, updated);
    }
    let mut diff = String::new();
    let mut files = Vec::new();
    for key in &order {
        let old = &originals[key];
        let new = &current[key];
        let path = &shown[key];
        diff.push_str(&format!("--- {path}\n"));
        diff.push_str(&crate::diff::unified(old, new, 300));
        diff.push('\n');
        files.push((path.clone(), new.clone()));
    }
    Ok(MultiEditPlan { diff, files })
}
/// Locate `old_text` in `data` and splice in `new_text`. Matches the raw bytes
/// first so a file is never rewritten wholesale; only when that fails does it
/// fall back to NFC-normalized matching (the model may emit composed
/// codepoints for a decomposed file), in which case the whole file comes back
/// normalized — the diff shows it. Errors are the model-facing messages.
fn splice_edit(data: &str, old_text: &str, new_text: &str, where_: &str) -> std::result::Result<String, String> {
    if old_text == new_text {
        return Err(format!("ERROR: {where_}old_text and new_text are identical (no change)."));
    }
    let count = |hay: &str, needle: &str| if needle.is_empty() { 0 } else { hay.matches(needle).count() };
    match count(data, old_text) {
        1 => return Ok(data.replacen(old_text, new_text, 1)),
        n if n > 1 => return Err(format!("ERROR: {where_}old_text matches {n} times; make it unique.")),
        _ => {}
    }
    let data_nfc: String = unicode_normalization::UnicodeNormalization::nfc(data.chars()).collect();
    let old_nfc: String = unicode_normalization::UnicodeNormalization::nfc(old_text.chars()).collect();
    match count(&data_nfc, &old_nfc) {
        0 => {}
        1 => return Ok(data_nfc.replacen(&old_nfc, new_text, 1)),
        n => return Err(format!("ERROR: {where_}old_text matches {n} times; make it unique.")),
    }
    // Models get whitespace wrong far more often than content: indentation off
    // by a level, blank lines with stray spaces, read_file's line numbers
    // pasted in. Match whole lines ignoring that, then re-indent the new text.
    let (old_t, new_t) = match strip_line_numbers(old_text) {
        Some(o) => (o, strip_line_numbers(new_text).unwrap_or_else(|| new_text.to_string())),
        None => (old_text.to_string(), new_text.to_string()),
    };
    if let Some(r) = splice_loose(data, &old_t, &new_t, where_) {
        return r;
    }
    Err(format!("ERROR: {where_}old_text not found.{}", closest_hint(data, &old_t)))
}

/// `text` without read_file's line-number prefixes (`   12<TAB>`, or the old
/// `   12  `), when every non-blank line carries one. None otherwise.
fn strip_line_numbers(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"^\s*\d+(\t| {2})").ok()?;
    let lines: Vec<&str> = text.split('\n').collect();
    let mut nonblank = lines.iter().filter(|l| !l.trim().is_empty()).peekable();
    nonblank.peek()?;
    if !nonblank.all(|l| re.is_match(l)) {
        return None;
    }
    Some(lines.iter().map(|l| re.replace(l, "").into_owned()).collect::<Vec<_>>().join("\n"))
}

/// Leading spaces and tabs of a line.
fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Replace the one run of whole lines in `data` that equals `old_text` line
/// for line once each line's surrounding whitespace is ignored (blank lines
/// match blank lines) — or, failing that, all of its whitespace (a space the
/// model dropped mid-line) — re-indenting `new_text` by the indentation the
/// model got wrong. None when nothing matches either way.
fn splice_loose(data: &str, old_text: &str, new_text: &str, where_: &str) -> Option<std::result::Result<String, String>> {
    let old: Vec<&str> = old_text.trim_end_matches(['\n', '\r']).split('\n').collect();
    let first = old.iter().position(|l| !l.trim().is_empty())?;
    let lines: Vec<&str> = data.split_inclusive('\n').collect();
    let k = old.len();
    if k > lines.len() {
        return None;
    }
    let trim = |l: &str| l.trim().to_string();
    let squash = |l: &str| l.split_whitespace().collect::<String>();
    let mut found = None;
    for key in [&trim as &dyn Fn(&str) -> String, &squash] {
        let want: Vec<String> = old.iter().map(|l| key(l)).collect();
        let have: Vec<String> = lines.iter().map(|l| key(l)).collect();
        let hits: Vec<usize> = (0..=lines.len() - k).filter(|&i| have[i..i + k] == want[..]).collect();
        match hits.as_slice() {
            [] => continue,
            [i] => {
                found = Some(*i);
                break;
            }
            many => {
                return Some(Err(format!(
                    "ERROR: {where_}old_text matches {} places when whitespace is ignored; include more \
                     surrounding lines to make it unique.",
                    many.len()
                )))
            }
        }
    }
    let i = found?;
    let start: usize = lines[..i].iter().map(|l| l.len()).sum();
    let end = start + lines[i..i + k].iter().map(|l| l.len()).sum::<usize>();
    let last = lines[i + k - 1];
    let nl = if last.ends_with("\r\n") || data.contains("\r\n") { "\r\n" } else { "\n" };
    let eol = if last.ends_with('\n') { nl } else { "" };
    let (from, to) = (indent_of(old[first]), indent_of(lines[i + first]));
    let shift = to.len() as isize - from.len() as isize;
    let reindent = |line: &str| -> String {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            return String::new();
        }
        if let Some(rest) = line.strip_prefix(from) {
            return format!("{to}{rest}");
        }
        // Shallower than old_text's first line (a closing brace, say): move it
        // by the same amount, as far as its own indentation allows.
        let lead = indent_of(line).len();
        if shift < 0 {
            line[(-shift as usize).min(lead)..].to_string()
        } else {
            format!("{}{line}", &to[..(shift as usize).min(to.len())])
        }
    };
    let body = if new_text.is_empty() {
        String::new()
    } else {
        let new: Vec<String> = new_text.trim_end_matches(['\n', '\r']).split('\n').map(reindent).collect();
        format!("{}{eol}", new.join(nl))
    };
    Some(Ok(format!("{}{body}{}", &data[..start], &data[end..])))
}

/// For a failed edit: the stretch of `data` that best lines up with
/// `old_text` (most lines equal ignoring whitespace), numbered like read_file,
/// so the model can copy the real text instead of guessing again.
fn closest_hint(data: &str, old_text: &str) -> String {
    let old: Vec<&str> = old_text.trim_end_matches('\n').split('\n').map(str::trim).collect();
    let lines: Vec<&str> = data.lines().collect();
    let span = old.len().min(lines.len());
    if span == 0 || old.len() > 400 || lines.len() > 50_000 {
        return String::new();
    }
    let trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    let (mut score, mut at) = (0, 0);
    for i in 0..=lines.len() - span {
        let s = (0..span).filter(|&j| !old[j].is_empty() && trimmed[i + j] == old[j]).count();
        if s > score {
            (score, at) = (s, i);
        }
    }
    if score == 0 {
        return " No line of it appears in the file; re-read the file before editing.".into();
    }
    let shown = &lines[at..(at + span).min(at + 40)];
    let body: Vec<String> = shown.iter().enumerate().map(|(j, l)| format!("{:>5}\t{l}", at + j + 1)).collect();
    format!(
        " Closest match, lines {}-{} ({score} of {} lines agree ignoring whitespace) — copy old_text \
         from here exactly (the text after each tab):\n{}",
        at + 1,
        at + shown.len(),
        old.len(),
        body.join("\n")
    )
}

/// Read a file for editing, refusing symlinks atomically (O_NOFOLLOW).
fn read_for_edit(p: &Path, shown: &str) -> std::result::Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(p) {
            Ok(mut f) => {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut f, &mut s).map_err(|e| format!("ERROR: {e}"))?;
                Ok(s)
            }
            Err(e) => {
                if let Ok(meta) = std::fs::symlink_metadata(p) {
                    if meta.file_type().is_symlink() {
                        return Err(format!("DENIED: {shown} is a symlink; edit the real path instead."));
                    }
                }
                Err(format!("ERROR: {e}"))
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(err) = deny_symlink(p, shown, "edit") {
            return Err(err);
        }
        std::fs::read_to_string(p).map_err(|e| format!("ERROR: {e}"))
    }
}

pub fn edit_preview(path: &str, old_text: &str, new_text: &str) -> EditPreview {
    let p = expand(path);
    let data = match read_for_edit(&p, path) {
        Ok(d) => d,
        Err(e) => return EditPreview::Err(e),
    };
    match splice_edit(&data, old_text, new_text, "") {
        Ok(new_content) => {
            let diff = crate::diff::unified(&data, &new_content, 300);
            EditPreview::Ok { diff, new_content }
        }
        Err(e) => EditPreview::Err(e),
    }
}

pub fn apply_write(path: &str, content: &str) -> String {
    let p = expand(path);
    // O_NOFOLLOW: the preview refused symlinks, the write must too (a link
    // could be swapped in between, and multi_edit has no other guard).
    #[cfg(unix)]
    let res = {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&p)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    };
    #[cfg(not(unix))]
    let res = match deny_symlink(&p, path, "edit") {
        Some(err) => return err,
        None => std::fs::write(&p, content),
    };
    match res {
        Ok(()) => format!("OK edited {}", p.display()),
        Err(e) if std::fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(false) => {
            format!("DENIED: {path} is a symlink; edit the real path instead. ({e})")
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_edit_composes_same_file() {
        let dir = std::env::temp_dir().join(format!("haikucode_multi_edit_test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "one two three\n").unwrap();
        std::fs::write(&b, "hello world\n").unwrap();
        let edits = vec![
            EditReq { path: a.to_string_lossy().into(), old_text: "one".into(), new_text: "1".into() },
            // Second edit to the same file sees the result of the first.
            EditReq { path: a.to_string_lossy().into(), old_text: "1 two".into(), new_text: "1 2".into() },
            EditReq { path: b.to_string_lossy().into(), old_text: "world".into(), new_text: "there".into() },
        ];
        let plan = multi_edit_plan(&edits).expect("plan ok");
        assert_eq!(plan.files.len(), 2);
        let a_final = &plan.files.iter().find(|(p, _)| p.contains("a.txt")).unwrap().1;
        assert_eq!(a_final, "1 2 three\n");
        // Apply and confirm disk matches.
        for (p, c) in &plan.files {
            apply_write(p, c);
        }
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "1 2 three\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "hello there\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn multi_edit_aborts_on_missing_match() {
        let dir = std::env::temp_dir().join(format!("haikucode_multi_edit_abort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, "keep me\n").unwrap();
        let edits = vec![
            EditReq { path: a.to_string_lossy().into(), old_text: "keep".into(), new_text: "KEEP".into() },
            EditReq { path: a.to_string_lossy().into(), old_text: "absent".into(), new_text: "x".into() },
        ];
        assert!(multi_edit_plan(&edits).is_err());
        // Nothing applied — file unchanged.
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "keep me\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edits_match_raw_text_first_and_only_normalize_as_a_fallback() {
        // Decomposed "é" (e + U+0301) elsewhere in the file must survive an
        // unrelated edit untouched.
        let data = "caf\u{65}\u{301} au lait\nfoo = 1\n";
        let out = splice_edit(data, "foo = 1", "foo = 2", "").unwrap();
        assert_eq!(out, "caf\u{65}\u{301} au lait\nfoo = 2\n");
        // Composed old_text against a decomposed file still matches (fallback).
        let out = splice_edit(data, "caf\u{e9}", "tea", "").unwrap();
        assert!(out.starts_with("tea au lait"));
        assert!(splice_edit(data, "nope", "x", "").unwrap_err().contains("not found"));
        assert!(splice_edit("a a", "a", "b", "").unwrap_err().contains("2 times"));
        assert!(splice_edit(data, "foo", "foo", "").unwrap_err().contains("identical"));
    }

    #[test]
    fn multi_edit_treats_two_spellings_of_one_file_as_one_file() {
        let dir = std::env::temp_dir().join(format!("haikucode-me-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one two three\n").unwrap();
        let spelled = format!("{}/./a.txt", dir.display());
        let plan = multi_edit_plan(&[
            EditReq { path: f.to_string_lossy().into_owned(), old_text: "one".into(), new_text: "1".into() },
            EditReq { path: spelled, old_text: "three".into(), new_text: "3".into() },
        ])
        .unwrap();
        assert_eq!(plan.files.len(), 1, "one file, both edits");
        assert_eq!(plan.files[0].1, "1 two 3\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn multi_edit_and_apply_write_refuse_symlinks() {
        let dir = std::env::temp_dir().join(format!("haikucode-me-ln-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.txt");
        std::fs::write(&real, "hello\n").unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = multi_edit_plan(&[EditReq { path: link.to_string_lossy().into_owned(), old_text: "hello".into(), new_text: "bye".into() }])
            .unwrap_err();
        assert!(err.starts_with("DENIED"), "{err}");
        let r = apply_write(link.to_str().unwrap(), "bye\n");
        assert!(r.starts_with("DENIED"), "{r}");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "hello\n", "the target is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_reports_bad_ranges_instead_of_an_empty_file() {
        let dir = std::env::temp_dir().join(format!("haikucode-rf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("r.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let p = f.to_str().unwrap();
        assert!(read_file(p, Some(10), None).starts_with("ERROR: start_line 10 is beyond"));
        assert!(read_file(p, Some(3), Some(2)).starts_with("ERROR: end_line 2 is before"));
        assert!(read_file(p, Some(2), Some(2)).contains("    2\tb"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failures seen with a local Qwen (2026-10-06): old_text indented two
    /// spaces too deep, with a whitespace-only blank line, from mistaking
    /// read_file's separator for indentation.
    #[test]
    fn edit_tolerates_wrong_indentation_and_reindents_the_new_text() {
        let data = "function frame(now) {\n    const dt = 1;\n    if (x) y();\n\n    gl.draw();\n  }\n";
        let old = "      const dt = 1;\n      if (x) y();\n  \n      gl.draw();\n    }";
        let new = "      const dt = 1;\n      if (x) y();\n\n      const d = 2;\n      gl.draw();\n    }";
        assert_eq!(
            splice_edit(data, old, new, "").unwrap(),
            "function frame(now) {\n    const dt = 1;\n    if (x) y();\n\n    const d = 2;\n    gl.draw();\n  }\n",
            "matched despite the indentation, and the new lines use the file's"
        );
    }

    /// The other slip seen live: a space dropped inside a line.
    /// From a live session: one no-op edit in a batch of six used to reject
    /// the whole batch.
    #[test]
    fn multi_edit_skips_a_no_op_edit() {
        let dir = std::env::temp_dir().join(format!("haikucode_noop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one\ntwo\n").unwrap();
        let path = f.to_string_lossy().into_owned();
        let req = |o: &str, n: &str| EditReq { path: path.clone(), old_text: o.into(), new_text: n.into() };
        let plan = multi_edit_plan(&[req("one", "ONE"), req("two", "two")]).unwrap();
        assert_eq!(plan.files[0].1, "ONE\ntwo\n");
        assert!(multi_edit_plan(&[req("two", "two")]).unwrap_err().contains("every edit"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_tolerates_a_space_dropped_mid_line() {
        let data = "  if (u_scene == 1) {\n    // ----- Menger: subdivision with continuous min -----\n    vec3 q = p;\n  }\n";
        let old = "  if (u_scene == 1) {\n    // ----- Menger: subdivision with continuous min-----\n    vec3 q = p;";
        let new = "  if (u_scene == 1) {\n    // ----- Menger: subdivision with continuous min-----\n    vec3 q = p * 2.0;";
        let out = splice_edit(data, old, new, "").unwrap();
        assert!(out.contains("vec3 q = p * 2.0;") && out.ends_with("  }\n"), "{out}");
    }

    #[test]
    fn edit_strips_line_numbers_copied_from_read_file() {
        let data = "a\n    const dt = 1;\n    go();\nz\n";
        let old = "    2\t    const dt = 1;\n    3\t    go();";
        let new = "    2\t    const dt = 2;\n    3\t    go();";
        assert_eq!(splice_edit(data, old, new, "").unwrap(), "a\n    const dt = 2;\n    go();\nz\n");
    }

    #[test]
    fn loose_matching_keeps_crlf_and_refuses_ambiguity() {
        let crlf = "if (a) {\r\n  x();\r\n}\r\n";
        assert_eq!(splice_edit(crlf, "    x();", "    y();", "").unwrap(), "if (a) {\r\n  y();\r\n}\r\n");
        // Two blocks that differ only by indentation: no guessing which.
        let twice = "  call();\n    call();\n";
        let err = splice_edit(twice, "\tcall();", "done();", "").unwrap_err();
        assert!(err.contains("2 places"), "{err}");
    }

    #[test]
    fn a_failed_edit_shows_the_closest_real_lines() {
        let data = "one\n  let a = 1;\n  let b = 2;\n  let c = 3;\nfour\n";
        let err = splice_edit(data, "  let a = 1;\n  let b = 20;\n  let c = 3;", "x", "").unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("Closest match, lines 2-4 (2 of 3"), "{err}");
        assert!(err.contains("    3\t  let b = 2;"), "real text, numbered like read_file: {err}");
        let none = splice_edit(data, "nothing like it", "x", "").unwrap_err();
        assert!(none.contains("re-read the file"), "{none}");
    }

    #[test]
    #[cfg(unix)]
    fn file_tools_refuse_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("haikucode_symlink_test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("real.txt");
        std::fs::write(&target, "secret\n").unwrap();
        let link = dir.join("link.txt");
        symlink(&target, &link).unwrap();
        let lp = link.to_string_lossy();
        // Reads, writes, and edits through the symlink are all refused.
        assert!(read_file(&lp, None, None).starts_with("DENIED"));
        assert!(write_file(&lp, "x").starts_with("DENIED"));
        assert!(matches!(
            edit_preview(&lp, "secret", "x"),
            EditPreview::Err(e) if e.starts_with("DENIED")
        ));
        assert!(list_files(&dir.to_string_lossy()).contains("link.txt"));
        // The real path still works.
        assert!(read_file(&target.to_string_lossy(), None, None).contains("secret"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
