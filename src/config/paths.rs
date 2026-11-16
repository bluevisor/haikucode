//! Where things live: the config dir (and picoder adoption), atomic writes,
//! settings layers, history and session paths.

use super::*;

/// Haiku keeps per-user settings in `~/config/settings` (B_USER_SETTINGS_DIRECTORY);
/// other hosts (the dev Mac, CI) use the XDG-style `~/.config`.
#[cfg(target_os = "haiku")]
const CONFIG_SUBDIR: &str = "config/settings/haikucode";
#[cfg(not(target_os = "haiku"))]
const CONFIG_SUBDIR: &str = ".config/haikucode";

pub fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(CONFIG_SUBDIR)
}

/// On first run on a host that had picoder (which Haiku Code replaces), copy
/// its settings, memory, history, sessions, skills and commands from
/// `~/.config/picoder` so nothing has to be set up again. Leaves picoder's
/// directory as it was; does nothing once `config_dir()` exists. Returns a
/// line saying what happened.
pub fn adopt_picoder() -> Option<String> {
    if cfg!(target_os = "haiku") || cfg!(test) {
        return None;
    }
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    adopt_picoder_from(&home.join(".config/picoder"), &config_dir())
}

pub(super) fn adopt_picoder_from(old: &Path, new: &Path) -> Option<String> {
    if new.exists() || !old.join("config.json").is_file() {
        return None;
    }
    fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let (src, dst) = (e.path(), to.join(e.file_name()));
            let ty = e.file_type()?;
            if ty.is_dir() {
                // `run/` holds a live picoder's sockets and locks.
                if e.file_name() != "run" {
                    copy_tree(&src, &dst)?;
                }
            } else if ty.is_file() {
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }
    match copy_tree(old, new) {
        Ok(()) => Some(format!("imported your picoder settings, memory and sessions from {}", old.display())),
        Err(e) => {
            // Half a copy would hide the rest next launch: start clean instead.
            let _ = std::fs::remove_dir_all(new);
            Some(format!("couldn't import picoder's settings from {} ({e})", old.display()))
        }
    }
}

/// `config_dir()` as shown to people and the model, e.g. `~/config/settings/haikucode/`.
pub fn config_dir_label() -> String {
    format!("~/{CONFIG_SUBDIR}/")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn memory_path() -> PathBuf {
    config_dir().join("memory.md")
}

/// Write `data` to `path` atomically: write a sibling temp file, fsync it, then
/// rename over the target. A power loss mid-write (common on a Pi running off an
/// SD card) then leaves either the intact old file or the complete new one —
/// never the truncated mix a bare `fs::write` can produce. The temp name is
/// per-process so two haikucode instances don't clobber each other's temp file.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        // Keep the target's mode (config.json is 0600 — it holds API keys):
        // a fresh temp file would otherwise come up 0644 and the rename would
        // silently widen the permissions.
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = f.set_permissions(meta.permissions());
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Every settings document that can carry `permissions` / `hooks`, lowest
/// priority first: the user's config.json, then the project's
/// `.haikucode/settings.json` and `.haikucode/settings.local.json` (the latter is
/// where "don't ask again" rules land, and belongs in .gitignore). Missing or
/// malformed files are skipped. Each entry is (path, parsed JSON object).
pub fn settings_layers() -> Vec<(PathBuf, serde_json::Value)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let paths = [
        config_path(),
        cwd.join(".haikucode").join("settings.json"),
        cwd.join(".haikucode").join("settings.local.json"),
    ];
    paths
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            v.is_object().then_some((p, v))
        })
        .collect()
}

/// The value of `key` in the most specific settings layer that sets it
/// (project local > project > config.json).
pub fn setting(key: &str) -> Option<serde_json::Value> {
    settings_layers().into_iter().rev().find_map(|(_, v)| v.get(key).cloned())
}

pub fn history_path() -> PathBuf {
    config_dir().join("history")
}

pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}
