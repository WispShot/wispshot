//! Updates. `wispshot.exe --update check|install` runs in its own short-lived process, so the
//! tray never loads WinHTTP. The newest version is where github.com/.../releases/latest
//! redirects to (no API, no rate limit). The setup is checked against the release's
//! SHA256SUMS.txt before it runs; it installs silently and starts WispShot again.

use crate::i18n::{t, tf};
use crate::upload::get;
use std::path::PathBuf;

const REPO: &str = "https://github.com/WispShot/wispshot";

/// "0.4.1" / "v0.4.1" -> (0, 4, 1)
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    let r = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(r)
}

/// Whether `v` is newer than this build.
pub fn newer(v: &str) -> bool {
    match (parse(v), parse(env!("CARGO_PKG_VERSION"))) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The newest published version ("0.4.1").
pub fn latest() -> Result<String, String> {
    let r = get(&format!("{REPO}/releases/latest"), false, 1 << 16)?;
    let loc = r.location.ok_or_else(|| format!("HTTP {}", r.code))?;
    let tag = loc.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    parse(tag).map(|_| tag.trim_start_matches('v').to_string()).ok_or_else(|| "no version".into())
}

/// Installed with the setup (it leaves its uninstaller next to the exe), not a portable copy.
pub fn installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("unins000.exe").exists())).unwrap_or(false)
}

fn sha256_hex(data: &[u8]) -> Option<String> {
    use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out).ok().ok()? };
    Some(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// The hash listed for `name` in a SHA256SUMS.txt ("<hex>  <name>" lines).
fn listed_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, n) = l.trim().split_once(char::is_whitespace)?;
        (n.trim().trim_start_matches('*') == name && h.len() == 64).then(|| h.to_ascii_lowercase())
    })
}

fn open_url(url: &str) {
    let w = crate::output::wide(url);
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(w.as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// Download, check and start the setup of version `v`. Ok(false) = sent to the release page.
fn install(v: &str) -> Result<bool, String> {
    if !installed() {
        open_url(&format!("{REPO}/releases/tag/v{v}"));
        return Ok(false);
    }
    crate::notify(&tf("update.downloading_title", &[v]), t("update.downloading"));
    let name = format!("WispShot-{v}-setup-x64.exe");
    let base = format!("{REPO}/releases/download/v{v}/");
    let sums = get(&format!("{base}SHA256SUMS.txt"), true, 1 << 16)?;
    if sums.code != 200 {
        return Err(format!("SHA256SUMS.txt: HTTP {}", sums.code));
    }
    let want = listed_hash(&String::from_utf8_lossy(&sums.body), &name).ok_or("no checksum for the setup")?;
    let setup = get(&format!("{base}{name}"), true, 64 << 20)?;
    if setup.code != 200 {
        return Err(format!("{name}: HTTP {}", setup.code));
    }
    if sha256_hex(&setup.body).as_deref() != Some(want.as_str()) {
        return Err("the download does not match its checksum".into());
    }
    // a new folder each time (nothing can be waiting in it); old ones go with clean_temp()
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("WispShot-update-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    let path: PathBuf = dir.join(&name);
    std::fs::write(&path, &setup.body).map_err(|e| e.to_string())?;
    // the setup closes the running WispShot, replaces it and starts it again (/restartapp);
    // this process ends right away so the exe is free to be replaced
    std::process::Command::new(&path)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/restartapp=1"])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Delete the setups earlier updates downloaded (a running one stays until next time).
pub fn clean_temp() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with("WispShot-update") {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `--update check`: tell the tray when a newer version is out. `--update install`: install it.
pub fn run(what: &str) -> i32 {
    crate::config::load(); // language for the notices
    match what {
        "check" => {
            if let Ok(v) = latest() {
                if newer(&v) {
                    crate::ipc_send(crate::IPC_UPDATE, &v);
                }
            }
            0
        }
        // from Settings: say what was found either way
        "checknow" => match latest() {
            Ok(v) if newer(&v) => {
                crate::ipc_send(crate::IPC_UPDATE, &v);
                0
            }
            Ok(_) => {
                crate::notify("WispShot", &tf("update.up_to_date", &[env!("CARGO_PKG_VERSION")]));
                0
            }
            Err(e) => {
                crate::notify(t("update.failed_title"), &tf("update.failed", &[&e]));
                1
            }
        },
        "install" => match latest() {
            Ok(v) if newer(&v) => match install(&v) {
                Ok(_) => 0,
                Err(e) => {
                    crate::notify(t("update.failed_title"), &tf("update.failed", &[&e]));
                    1
                }
            },
            Ok(_) => {
                crate::notify("WispShot", &tf("update.up_to_date", &[env!("CARGO_PKG_VERSION")]));
                0
            }
            Err(e) => {
                crate::notify(t("update.failed_title"), &tf("update.failed", &[&e]));
                1
            }
        },
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_sums() {
        assert_eq!(parse("v0.4.1"), Some((0, 4, 1)));
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert!(newer("99.0.0"));
        assert!(!newer(env!("CARGO_PKG_VERSION")));
        assert!(!newer("0.0.1"));
        let h = "a".repeat(64);
        let sums = format!("{h}  WispShot-0.4.1-portable-x64.zip\n{}  WispShot-0.4.1-setup-x64.exe\n", "B".repeat(64));
        assert_eq!(listed_hash(&sums, "WispShot-0.4.1-setup-x64.exe"), Some("b".repeat(64)));
        assert_eq!(listed_hash(&sums, "nope.exe"), None);
        assert_eq!(
            sha256_hex(b"abc").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }
}

#[cfg(test)]
mod live {
    /// Asks github.com for real: `cargo test --release -- --ignored live_latest`
    #[test]
    #[ignore]
    fn live_latest() {
        let v = super::latest().expect("latest");
        assert!(super::parse(&v).is_some(), "{v}");
        println!("latest = {v}");
    }
}
