//! Over-the-air updates from GitHub releases. Each release carries the Haiku
//! package (`haikucode-<version>-<rev>-<arch>.hpkg`); updating downloads it,
//! checks its SHA-256 against the digest GitHub publishes, and swaps it into
//! `~/config/packages`, where the package daemon activates it a moment later.
//! The running copy keeps going; the next launch is the new version.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LATEST_URL: &str = "https://api.github.com/repos/bluevisor/haikucode/releases/latest";
/// How often the launch check may hit GitHub.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// Packages are a few MB; refuse anything absurd.
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;

pub struct Release {
    pub version: String,
    asset_name: String,
    asset_url: String,
    size: u64,
    /// Hex SHA-256 from the asset's `digest` ("sha256:…"), when GitHub has one.
    sha256: Option<String>,
}

/// The package architecture this build's release asset carries: `getarch`'s
/// name, except that the 32-bit build (for the x86 secondary arch of
/// x86_gcc2h Haiku) ships in an x86_gcc2 package, like HaikuPorts' `_x86` ones.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "x86_gcc2",
        a => a,
    }
}

/// `0.1.10` > `0.1.9`; a leading `v` and any `-suffix` are ignored.
fn parse_version(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split(['-', '+']).next().unwrap_or(v);
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

/// The latest release and its package for this architecture.
pub fn latest(http: &ureq::Agent) -> Result<Release> {
    let body = http
        .get(LATEST_URL)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => anyhow!("no Haiku Code release is published yet"),
            e => anyhow!("could not reach GitHub: {e}"),
        })?
        .into_string()
        .context("read release info")?;
    let v: serde_json::Value = serde_json::from_str(&body).context("bad release JSON")?;
    let tag = v["tag_name"].as_str().ok_or_else(|| anyhow!("no release published yet"))?;
    let suffix = format!("-{}.hpkg", arch());
    let asset = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.starts_with("haikucode-") && n.ends_with(&suffix)))
        .ok_or_else(|| anyhow!("release {tag} has no {} package", arch()))?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        asset_name: asset["name"].as_str().unwrap_or_default().to_string(),
        asset_url: asset["browser_download_url"].as_str().unwrap_or_default().to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        sha256: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
    })
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into()))
}

/// The launch check: at most once a day, and only on Haiku (the releases
/// carry Haiku packages). Returns a notice when a newer version is out.
pub fn check_on_launch(http: &ureq::Agent) -> Option<String> {
    if !cfg!(target_os = "haiku") {
        return None;
    }
    let stamp = crate::config::config_dir().join("update-check");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let last = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    if now.saturating_sub(last) < CHECK_EVERY.as_secs() {
        return None;
    }
    let rel = latest(http).ok()?;
    let _ = std::fs::write(&stamp, now.to_string());
    let current = env!("CARGO_PKG_VERSION");
    is_newer(&rel.version, current)
        .then(|| format!("Haiku Code {} is out (you have {current}) — /update to install it", rel.version))
}

/// Download, verify and install `rel`. `note` reports progress.
pub fn install(http: &ureq::Agent, rel: &Release, note: &dyn Fn(String)) -> Result<String> {
    if !cfg!(target_os = "haiku") {
        bail!("OTA updates install Haiku packages; on this system update from source (cargo install --path .)");
    }
    let packages = home().join("config/packages");
    let bin = home().join("config/bin/haikucode");
    // A copy installed system-wide (pkgman/HaikuDepot) or run from a build tree
    // isn't ours to swap.
    let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    if exe.as_deref().is_some_and(|p| !p.starts_with(home().join("config"))) {
        bail!("this haikucode ({}) isn't the ~/config package; update it with pkgman", exe.unwrap().display());
    }

    note(format!("downloading {} ({} KB)…", rel.asset_name, rel.size / 1024));
    let bytes = download(http, &rel.asset_url, rel.size)?;
    if let Some(want) = &rel.sha256 {
        let got = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        if &got != want {
            bail!("checksum mismatch for {} (got {got}, release says {want}); not installed", rel.asset_name);
        }
    }

    // Stage beside ~/config/packages (same volume, so the final step is an
    // atomic rename the package daemon never sees half-written).
    let stage_dir = home().join("config/cache/haikucode-update");
    std::fs::create_dir_all(&stage_dir).context("create cache dir")?;
    let staged = stage_dir.join(&rel.asset_name);
    std::fs::write(&staged, &bytes).context("write package")?;

    // Two haikucode packages in ~/config/packages block activation, and a
    // same-name swap in one go races the daemon: remove, wait for the old
    // binary to go, then add and wait for the new one (as haiku/deploy.sh does).
    for old in old_packages(&packages) {
        std::fs::remove_file(&old).with_context(|| format!("remove {}", old.display()))?;
    }
    wait_for(|| !bin.exists());
    std::fs::rename(&staged, packages.join(&rel.asset_name)).context("install package")?;
    if !wait_for(|| bin.exists()) {
        bail!("installed {} but it hasn't activated yet; check ~/config/packages", rel.asset_name);
    }
    Ok(format!("installed Haiku Code {} — restart haikucode to use it", rel.version))
}

fn old_packages(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("haikucode-") && n.ends_with(".hpkg"))
        })
        .collect()
}

fn wait_for(cond: impl Fn() -> bool) -> bool {
    for _ in 0..40 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    cond()
}

fn download(http: &ureq::Agent, url: &str, size: u64) -> Result<Vec<u8>> {
    let resp = http.get(url).call().map_err(|e| anyhow!("download failed: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader().take(MAX_PACKAGE).read_to_end(&mut bytes).context("download interrupted")?;
    if size > 0 && bytes.len() as u64 != size {
        bail!("download was {} bytes, expected {size}", bytes.len());
    }
    Ok(bytes)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `/update` and `haikucode update`: install the latest release if it's newer
/// (or regardless, with `force`).
pub fn run(http: &ureq::Agent, force: bool, note: &dyn Fn(String)) -> Result<String> {
    let rel = latest(http)?;
    let current = env!("CARGO_PKG_VERSION");
    if !force && !is_newer(&rel.version, current) {
        return Ok(format!("Haiku Code {current} is up to date (latest release {})", rel.version));
    }
    install(http, &rel, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("v0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        assert!(is_newer("1.0.0-rc1", "0.9.0"));
    }

    #[test]
    fn digest_hex_matches_a_known_sha256() {
        let d = ring::digest::digest(&ring::digest::SHA256, b"abc");
        assert_eq!(hex(d.as_ref()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
