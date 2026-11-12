//! Updates. `wispshot.exe --update check|install` runs in its own short-lived process, so the
//! tray never loads WinHTTP. The newest version is where github.com/.../releases/latest
//! redirects to (no API, no rate limit). The setup is checked against the release's
//! SHA256SUMS.txt before it runs; it installs silently and starts WispShot again. A portable copy
//! gets the portable zip (checked the same way), unpacked with Windows' own tar.exe; the running
//! exe is renamed to wispshot.exe.old (Windows allows that), the new one takes its place and starts.

use crate::i18n::{t, tf};
use crate::upload::get;
use std::os::windows::process::CommandExt;
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

/// Release file `name` of version `v`, checked against the release's SHA256SUMS.txt.
fn download(v: &str, name: &str) -> Result<Vec<u8>, String> {
    let base = format!("{REPO}/releases/download/v{v}/");
    let sums = get(&format!("{base}SHA256SUMS.txt"), true, 1 << 16)?;
    if sums.code != 200 {
        return Err(format!("SHA256SUMS.txt: HTTP {}", sums.code));
    }
    let want = listed_hash(&String::from_utf8_lossy(&sums.body), name).ok_or("no checksum for the download")?;
    let file = get(&format!("{base}{name}"), true, 64 << 20)?;
    if file.code != 200 {
        return Err(format!("{name}: HTTP {}", file.code));
    }
    if sha256_hex(&file.body).as_deref() != Some(want.as_str()) {
        return Err("the download does not match its checksum".into());
    }
    Ok(file.body)
}

/// A new folder each time (nothing can be waiting in it); old ones go with clean_temp().
fn temp_folder() -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("WispShot-update-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Whether this program's folder takes new files (a portable copy under Program Files does not).
fn folder_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!("wispshot-write-test-{}.tmp", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Whether the tray (any WispShot resident) is running.
fn tray_running() -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::FindWindowW(windows::core::w!("WispApp"), None).is_ok() }
}

/// Whether a WispShot still holds the single-instance lock (a tray whose window is gone keeps it
/// until its process ends, and a new copy started before that would just leave).
fn instance_lock_held() -> bool {
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, windows::core::w!(r"Local\WispShot.SingleInstance")) {
            Ok(h) => {
                let _ = windows::Win32::Foundation::CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

fn wait_until(mut done: impl FnMut() -> bool, secs: u64) {
    let t0 = std::time::Instant::now();
    while !done() && t0.elapsed().as_secs() < secs {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Replace this portable copy with version `v` and start it. Ok(false) = sent to the release page.
fn install_portable(v: &str) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no folder")?.to_path_buf();
    let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let tar = sys.join("System32").join("tar.exe");
    if !folder_writable(&dir) || !tar.exists() {
        open_url(&format!("{REPO}/releases/tag/v{v}"));
        return Ok(false);
    }
    crate::notify(&tf("update.downloading_title", &[v]), t("update.downloading"));
    let name = format!("WispShot-{v}-portable-x64.zip");
    let zip = download(v, &name)?;
    let tmp = temp_folder()?;
    let zip_path = tmp.join(&name);
    std::fs::write(&zip_path, &zip).map_err(|e| e.to_string())?;
    let st = std::process::Command::new(&tar)
        .arg("-xf")
        .arg(&zip_path)
        .arg("-C")
        .arg(&tmp)
        .arg("wispshot.exe")
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .map_err(|e| e.to_string())?;
    let new = tmp.join("wispshot.exe");
    if !st.success() || !new.is_file() {
        return Err("the download could not be unpacked".into());
    }
    // close the tray (and Settings with it), then swap the exe
    let was_running = tray_running();
    if was_running {
        let _ = std::process::Command::new(&exe).arg("--exit").status();
        wait_until(|| !tray_running() && !instance_lock_held(), 15);
    }
    let mut old = dir.join("wispshot.exe.old");
    let _ = std::fs::remove_file(&old);
    if old.exists() {
        old = dir.join(format!("wispshot-{}.exe.old", std::process::id()));
    }
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::copy(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        if was_running {
            let _ = std::process::Command::new(&exe).spawn();
        }
        return Err(e.to_string());
    }
    std::process::Command::new(&exe).spawn().map_err(|e| e.to_string())?;
    // say it worked once the new tray is up
    wait_until(tray_running, 10);
    std::thread::sleep(std::time::Duration::from_millis(500));
    crate::notify(&tf("update.done_title", &[v]), t("update.done"));
    Ok(true)
}

/// Download, check and start the setup of version `v` (or replace a portable copy).
fn install(v: &str) -> Result<bool, String> {
    if !installed() {
        return install_portable(v);
    }
    crate::notify(&tf("update.downloading_title", &[v]), t("update.downloading"));
    let name = format!("WispShot-{v}-setup-x64.exe");
    let setup = download(v, &name)?;
    let dir = temp_folder()?;
    let path: PathBuf = dir.join(&name);
    std::fs::write(&path, &setup).map_err(|e| e.to_string())?;
    // the setup closes the running WispShot, replaces it and starts it again (/restartapp);
    // this process ends right away so the exe is free to be replaced
    std::process::Command::new(&path)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/restartapp=1"])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Delete the setups earlier updates downloaded (a running one stays until next time), and
/// the exe a portable update replaced.
pub fn clean_temp() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n.starts_with("wispshot") && n.ends_with(".exe.old") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
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
