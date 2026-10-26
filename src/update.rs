//! Over-the-air updates from GitHub releases. Each release carries the Haiku
//! packages (`haikucode-<version>-<rev>-<arch>.hpkg`) and a tarball per macOS
//! and Linux build (`haikucode-<version>-<platform>.tar.gz`). Updating
//! downloads the one for this build and checks its SHA-256 against the digest
//! GitHub publishes. On Haiku the package is swapped into `~/config/packages`,
//! where the package daemon activates it a moment later; elsewhere the binary
//! replaces the running one in place. The running copy keeps going; the next
//! launch is the new version.

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
#[cfg(target_os = "haiku")]
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "x86_gcc2",
        a => a,
    }
}

/// The macOS/Linux build this is (`macos-arm64`, `linux-x86_64`, …), which
/// names its release tarball. The release workflow stamps it in at build time
/// (`HAIKUCODE_ASSET`), since the 32-bit ARM builds (`linux-armv6`/`-armv7`)
/// share a Rust arch; a local build of a 64-bit target works it out.
#[cfg(not(target_os = "haiku"))]
fn platform() -> Option<&'static str> {
    if let Some(p) = option_env!("HAIKUCODE_ASSET") {
        return Some(p);
    }
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => return None,
    })
}

/// The end of this build's release asset name.
fn asset_suffix() -> Option<String> {
    #[cfg(target_os = "haiku")]
    return Some(format!("-{}.hpkg", arch()));
    #[cfg(not(target_os = "haiku"))]
    return platform().map(|p| format!("-{p}.tar.gz"));
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
    let suffix = asset_suffix().ok_or_else(|| {
        anyhow!("no release builds for {}-{}; build from source", std::env::consts::OS, std::env::consts::ARCH)
    })?;
    let asset = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.starts_with("haikucode-") && n.ends_with(&suffix)))
        .ok_or_else(|| anyhow!("release {tag} has no haikucode-…{suffix} yet"))?;
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

/// The launch check: at most once a day, and only for a copy `/update` could
/// replace (not a build tree's). Returns a notice when a newer version is out.
pub fn check_on_launch(http: &ureq::Agent) -> Option<String> {
    asset_suffix()?;
    if replaceable_exe().is_err() {
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

/// The running binary, when it's a copy an update may replace: the
/// `~/config` package on Haiku; elsewhere any installed copy outside a cargo
/// build tree or a package manager's prefix.
fn replaceable_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("can't locate the running haikucode")?;
    let s = exe.to_string_lossy();
    if cfg!(target_os = "haiku") {
        // A copy installed system-wide (pkgman/HaikuDepot) isn't ours to swap.
        if !exe.starts_with(home().join("config")) {
            bail!("this haikucode ({s}) isn't the ~/config package; update it with pkgman");
        }
    } else if s.contains("/target/debug/") || (s.contains("/target/") && s.contains("/release/")) {
        bail!("this haikucode ({s}) is a cargo build; rebuild it from source instead");
    } else if s.contains("/Cellar/") || s.starts_with("/nix/") || s.starts_with("/usr/bin/") {
        bail!("this haikucode ({s}) belongs to a package manager; update it there");
    }
    Ok(exe)
}

/// Download, verify and install `rel`. `note` reports progress.
pub fn install(http: &ureq::Agent, rel: &Release, note: &dyn Fn(String)) -> Result<String> {
    let exe = replaceable_exe()?;
    note(format!("downloading {} ({} KB)…", rel.asset_name, rel.size / 1024));
    let bytes = download(http, &rel.asset_url, rel.size)?;
    if let Some(want) = &rel.sha256 {
        let got = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        if &got != want {
            bail!("checksum mismatch for {} (got {got}, release says {want}); not installed", rel.asset_name);
        }
    }
    if cfg!(target_os = "haiku") {
        install_package(rel, &bytes)
    } else {
        install_binary(rel, &bytes, &exe)
    }
}

/// Unpack the release tarball's `haikucode`, check that it runs here, and
/// rename it over `exe` (same directory, so the swap is atomic and the running
/// process keeps its old inode).
fn install_binary(rel: &Release, tarball: &[u8], exe: &Path) -> Result<String> {
    let dir = exe.parent().ok_or_else(|| anyhow!("{} has no directory", exe.display()))?;
    let work = std::env::temp_dir().join(format!("haikucode-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).context("create temp dir")?;
    let result = (|| {
        let tgz = work.join(&rel.asset_name);
        std::fs::write(&tgz, tarball).context("write download")?;
        let out = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&tgz)
            .arg("-C")
            .arg(&work)
            .arg("haikucode")
            .output()
            .context("run tar")?;
        if !out.status.success() {
            bail!("unpacking {} failed: {}", rel.asset_name, String::from_utf8_lossy(&out.stderr).trim());
        }
        // Stage beside the target: a rename across filesystems would fail.
        let staged = dir.join(format!(".haikucode.new.{}", std::process::id()));
        std::fs::copy(work.join("haikucode"), &staged).map_err(|e| {
            anyhow!("can't write to {} ({e}); reinstall haikucode somewhere you own, such as ~/.local/bin", dir.display())
        })?;
        let swap = (|| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).context("chmod")?;
            let ran = std::process::Command::new(&staged).arg("--version").output();
            match ran {
                Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&rel.version) => {}
                Ok(o) => bail!(
                    "the downloaded build didn't run here ({}); not installed",
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                Err(e) => bail!("the downloaded build didn't run here ({e}); not installed"),
            }
            std::fs::rename(&staged, exe).with_context(|| format!("replace {}", exe.display()))
        })();
        if swap.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        swap
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    Ok(format!("installed Haiku Code {} — restart haikucode to use it", rel.version))
}

/// Swap the package into `~/config/packages` and wait for the daemon to
/// activate it.
fn install_package(rel: &Release, bytes: &[u8]) -> Result<String> {
    let packages = home().join("config/packages");
    let bin = home().join("config/bin/haikucode");

    // Stage beside ~/config/packages (same volume, so the final step is an
    // atomic rename the package daemon never sees half-written).
    let stage_dir = home().join("config/cache/haikucode-update");
    std::fs::create_dir_all(&stage_dir).context("create cache dir")?;
    let staged = stage_dir.join(&rel.asset_name);
    std::fs::write(&staged, bytes).context("write package")?;

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

    /// Each build looks for its own asset, and no suffix is a tail of another
    /// (`-arm64.hpkg` vs `-macos-arm64.tar.gz`).
    #[test]
    fn this_build_has_a_distinct_asset_suffix() {
        #[cfg(any(target_os = "macos", target_os = "haiku", all(target_os = "linux", target_pointer_width = "64")))]
        {
            let s = asset_suffix().expect("a released platform");
            assert!(s.ends_with(".hpkg") || s.ends_with(".tar.gz"), "{s}");
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(asset_suffix().as_deref(), Some("-macos-arm64.tar.gz"));
    }

    #[test]
    fn digest_hex_matches_a_known_sha256() {
        let d = ring::digest::digest(&ring::digest::SHA256, b"abc");
        assert_eq!(hex(d.as_ref()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
