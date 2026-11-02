//! Self-update from GitHub Releases: one check at startup on a short-lived
//! thread, install only after the user agrees. Uses the inbox `curl.exe` and
//! `tar.exe` (Windows 10 1803+), so no HTTP/zip code ends up in the exe.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "https://github.com/Jess4da/FPS-Overlay";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Latest release tag (e.g. `v0.3.0`) if it is newer than this build.
/// `releases/latest` redirects to `/releases/tag/<tag>` and skips pre-releases.
pub fn check() -> Option<String> {
    let url = format!("{REPO}/releases/latest");
    let loc = run(Command::new("curl.exe").args(["-sfI", "-m", "15", &url])).ok()?;
    let tag = loc
        .lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or(l.strip_prefix("Location: "))
        })?
        .trim()
        .rsplit('/')
        .next()?
        .to_string();
    newer(&tag, env!("CARGO_PKG_VERSION")).then_some(tag)
}

fn triple(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse().ok());
    let t = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(t)
}

/// True when `tag` is a strictly higher `X.Y.Z` than `current`; junk is never newer.
fn newer(tag: &str, current: &str) -> bool {
    matches!((triple(tag), triple(current)), (Some(t), Some(c)) if t > c)
}

/// The exe left behind by the last update; deleted at startup.
fn old_exe(exe: &std::path::Path) -> PathBuf {
    exe.with_extension("exe.old")
}

pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe(&exe));
    }
}

/// Downloads the release zip and swaps it in for the running exe (Windows allows
/// renaming a running image). The caller restarts with `restart()` after exit.
/// ponytail: trusts GitHub over HTTPS; the `.sha256` asset is not checked, since it
/// comes from the same host. Add signature checks if releases move elsewhere.
pub fn install(tag: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join("fps-overlay-update");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join("update.zip");
    let url = format!("{REPO}/releases/download/{tag}/fps-overlay-{tag}-x86_64-windows.zip");
    run(Command::new("curl.exe")
        .args(["-sfL", "-m", "120", "-o"])
        .arg(&zip)
        .arg(&url))
    .map_err(|e| format!("download failed: {e}"))?;
    run(Command::new("tar.exe")
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&dir))
    .map_err(|e| format!("unzip failed: {e}"))?;
    let new = dir.join("fps-overlay.exe");
    if !new.is_file() {
        return Err("fps-overlay.exe missing from the release zip".into());
    }
    let old = old_exe(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::copy(&new, &exe) {
        // Put the running exe back so the install is never left without one.
        let _ = std::fs::rename(&old, &exe);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Starts the (updated) exe. We are elevated, so the child is too: no UAC prompt.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = Command::new(&exe);
        if let Some(d) = exe.parent() {
            cmd.current_dir(d);
        }
        let _ = cmd.spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_numeric_semver() {
        assert!(newer("v0.2.10", "0.2.9"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.2.1", "0.2.1"));
        assert!(!newer("v0.2.0", "0.2.1"));
        // A pre-release suffix with the same core is not an upgrade.
        assert!(!newer("v0.2.1-rc1", "0.2.1"));
        assert!(!newer("latest", "0.2.1"));
        assert!(!newer("v0.3", "0.2.1"));
    }
}
