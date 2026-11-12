//! Tiny INI config stored next to the executable (portable).

use crate::i18n::t;
use crate::keys::{self, Scope, DEFS, K};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// A global hotkey ready for RegisterHotKey.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// Editor tools that can sit on the toolbar: ini name, i18n key of the name.
pub const BAR_TOOLS: [(&str, &str); 11] = [
    ("rect", "tool.rect"),
    ("ellipse", "tool.ellipse"),
    ("arrow", "tool.arrow"),
    ("line", "tool.line"),
    ("measure", "tool.measure"),
    ("pen", "tool.pen"),
    ("marker", "tool.marker"),
    ("text", "tool.text"),
    ("counter", "tool.counter"),
    ("mosaic", "tool.mosaic"),
    ("blur", "tool.blur"),
];
/// Editor actions that can sit on the toolbar (Copy and Close always do).
pub const BAR_ACTIONS: [(&str, &str); 8] = [
    ("redact", "editor.redact"),
    ("pin", "editor.pin"),
    ("ocr", "editor.ocr"),
    ("record", "editor.record"),
    ("scroll", "editor.scroll"),
    ("upload", "editor.upload"),
    ("save", "editor.save"),
    ("copy_file", "editor.copy_file"),
];
const DEFAULT_BAR_TOOLS: &str = "rect arrow pen text counter blur";
const DEFAULT_BAR_ACTIONS: &str = "pin save";

/// Known names from `v` ("rect, pen arrow"), in the table's order.
fn parse_bar(v: &str, table: &[(&str, &str)]) -> Vec<String> {
    let want: Vec<String> = v.split([',', ' ', ';']).map(|w| w.trim().to_ascii_lowercase()).collect();
    table.iter().filter(|(n, _)| want.iter().any(|w| w == n)).map(|(n, _)| n.to_string()).collect()
}

static BAR: std::sync::RwLock<(Vec<String>, Vec<String>)> = std::sync::RwLock::new((Vec::new(), Vec::new()));

/// The toolbar's tools and actions from the settings, for the editor.
pub fn bar() -> (Vec<String>, Vec<String>) {
    BAR.read().map(|b| b.clone()).unwrap_or_default()
}

/// A program from `[hotkeys] pause_in_apps`.
#[derive(Clone, PartialEq, Debug)]
pub struct PauseApp {
    /// Lowercase exe name, what the foreground program is matched by ("saplogon.exe").
    pub exe: String,
    /// Where the program was found (for its icon and name); may be empty.
    pub path: String,
    /// The system-wide shortcuts released there; empty: all of them.
    pub keys: Vec<K>,
}

impl PauseApp {
    pub fn from_path(path: &str) -> Option<PauseApp> {
        let p = path.trim().trim_matches('"').trim();
        let name = p.rsplit(['\\', '/']).next().unwrap_or(p).trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        let exe = if name.ends_with(".exe") { name } else { name + ".exe" };
        let path = if p.contains(['\\', '/']) { p.to_string() } else { String::new() };
        Some(PauseApp { exe, path, keys: vec![] })
    }

    /// Whether shortcut `k` is released while this program is in front.
    pub fn pauses(&self, k: K) -> bool {
        self.keys.is_empty() || self.keys.contains(&k)
    }

    /// "C:\x\saplogon.exe | capture record" (the part after | only when not all keys pause).
    fn ini(&self) -> String {
        let mut s = if self.path.is_empty() { self.exe.clone() } else { self.path.clone() };
        if !self.keys.is_empty() {
            let names: Vec<&str> = self.keys.iter().map(|k| keys::def(*k).ini).collect();
            s += &format!(" | {}", names.join(" "));
        }
        s
    }
}

#[derive(Clone)]
pub struct Config {
    /// One binding per `keys::DEFS` entry ("" = off).
    pub keys: Vec<String>,
    /// "system" or a language code ("en", "tr").
    pub language: String,
    pub theme: Theme,
    /// Release the hotkeys while a full-screen app (game, video) is in front.
    pub pause_fullscreen: bool,
    /// Release the hotkeys while one of these programs is in front: their own keys such as
    /// F1 or F3 keep working there.
    pub pause_apps: Vec<PauseApp>,
    /// Ask GitHub once a day whether a newer version is out.
    pub check_updates: bool,
    /// Editor toolbar: the tools and the actions shown on the bar (ini names, see BAR_TOOLS /
    /// BAR_ACTIONS); the rest sit behind its "more" buttons.
    pub bar_tools: Vec<String>,
    pub bar_actions: Vec<String>,
    /// WispShot turned off "Print Screen opens screen capture" in Windows; turn it back on
    /// when no shortcut uses Print Screen any more (and on uninstall).
    pub snipping_restore: bool,
    pub save_dir: PathBuf,
    /// true: ask for a location every time; false: save straight into `save_dir`.
    pub save_ask: bool,
    /// Balloon after a screenshot was saved (click opens the folder).
    pub save_notify: bool,
    /// Screenshot file format (the save dialog preselects it).
    pub image_format: crate::output::Fmt,
    pub jpeg_quality: u32,
    /// Share frame around copied / saved screenshots.
    pub frame: crate::frame::Style,
    pub video_dir: PathBuf,
    pub video_ask: bool,
    pub video_notify: bool,
    /// Save recordings as GIF (converted from the MP4 when recording ends).
    pub gif: bool,
    pub gif_fps: u32,
    /// Widest GIF in pixels; larger recordings are scaled down. 0 = keep the size.
    pub gif_width: u32,
    pub fps: u32,
    pub cursor: bool,
    pub magnifier: bool,
    pub detect_elements: bool,
    /// Seconds the delayed capture waits.
    pub capture_delay: u32,
    /// OCR language: "auto" or a language tag such as "tr" or "en-US".
    pub ocr_language: String,
    /// Keep finished captures in the history folder.
    pub history: bool,
    pub history_keep: u32,
    pub history_mb: u32,
    /// Delete history images older than this many days (0: never).
    pub history_days: u32,
    /// Read the text in history images (for searching).
    pub history_ocr: bool,
    /// Where the Upload button sends a capture.
    pub upload_host: crate::upload::Host,
    /// `custom` host: the address to POST to and the form field for the file.
    pub upload_url: String,
    pub upload_field: String,
    /// The user has agreed once that uploads are public.
    pub upload_asked: bool,
    /// Drawings on a recording fade out after a few seconds (steps stay).
    pub ink_fade: bool,
}

impl Config {
    pub fn key(&self, k: K) -> &str {
        &self.keys[k as usize]
    }

    pub fn set_key(&mut self, k: K, text: String) {
        self.keys[k as usize] = text;
    }

    /// A global hotkey as RegisterHotKey wants it.
    pub fn hotkey(&self, k: K) -> Option<Hotkey> {
        keys::parse(self.key(k)).map(|c| Hotkey { mods: c.hotkey_mods(), vk: c.vk as u32 })
    }

    /// Shortcut as shown in menus and tips ("Shift + F1"), "" when off.
    pub fn key_label(&self, k: K) -> String {
        keys::display(self.key(k))
    }
}

/// Section and name of a key binding in the ini.
fn key_place(i: usize) -> (&'static str, &'static str) {
    let d = &DEFS[i];
    if d.scope == Scope::Global {
        ("hotkeys", d.ini)
    } else {
        ("keys", d.ini)
    }
}

/// A fresh wispshot.ini with comments in the current language.
fn default_ini() -> String {
    let mut s = String::new();
    for l in t("ini.header").lines() {
        s += &format!("; {}\n", l);
    }
    s += "\n[general]\n";
    s += &format!("; {}\nlanguage = system\n", t("ini.language"));
    s += &format!("; {}\ntheme = system\n", t("ini.theme"));
    s += &format!("; {}\ncheck_updates = true\n", t("ini.check_updates"));
    s += "\n[hotkeys]\n";
    for d in DEFS.iter().filter(|d| d.scope == Scope::Global) {
        s += &format!("{} = {}\n", d.ini, d.default);
    }
    s += &format!("; {}\npause_in_fullscreen = true\n", t("ini.pause_fullscreen"));
    s += &format!("; {}\npause_in_apps =\n", t("ini.pause_apps"));
    s += &format!(
        "\n[output]\n; {}\nsave_dir =\n; {}\nsave_mode = ask\n; {}\nnotify = true\n; {}\nformat = png\n; {}\njpeg_quality = 90\n; {}\nframe = off\n",
        t("ini.save_dir"),
        t("ini.save_mode"),
        t("ini.notify"),
        t("ini.format"),
        t("ini.jpeg_quality"),
        t("ini.frame")
    );
    s += &format!("\n[record]\n; {}\nvideo_dir =\n; {}\nsave_mode = folder\nnotify = true\nfps = 30\ncursor = true\n; {}\nink_fade = true\n; {}\nformat = mp4\n; {}\ngif_fps = 15\ngif_width = 800\n", t("ini.video_dir"), t("ini.video_mode"), t("ini.ink_fade"), t("ini.video_format"), t("ini.gif"));
    s += &format!(
        "\n[capture]\nmagnifier = true\n; {}\ndetect_elements = false\n; {}\ndelay = 3\n; {}\nocr_language = auto\n",
        t("ini.detect"),
        t("ini.delay"),
        t("ini.ocr_language")
    );
    s += &format!(
        "\n[history]\n; {}\nenabled = false\n; {}\nkeep = 100\nmax_mb = 500\n; {}\nmax_days = 0\n; {}\nocr = true\n",
        t("ini.history"),
        t("ini.history_limit"),
        t("ini.history_days"),
        t("ini.history_ocr")
    );
    s += &format!(
        "\n[upload]\n; {}\nhost = uguu\n; {}\nurl =\nfield = file\nasked = false\n",
        t("ini.upload_host"),
        t("ini.upload_custom")
    );
    s += &format!(
        "\n[editor]\n; {}\ntoolbar_tools = {}\ntoolbar_actions = {}\n",
        t("ini.toolbar"),
        DEFAULT_BAR_TOOLS,
        DEFAULT_BAR_ACTIONS
    );
    s += &format!("\n[keys]\n; {}\n", t("ini.keys"));
    for d in DEFS.iter().filter(|d| d.scope != Scope::Global) {
        s += &format!("{} = {}\n", d.ini, d.default);
    }
    s
}

pub fn ini_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let p = dir.join("wispshot.ini");
    migrate(&dir.join("wisp.ini"), &p);
    // fall back to %APPDATA%\WispShot when the exe folder is read-only (e.g. Program Files)
    if p.exists() {
        return p;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
        Ok(_) => {
            // this empty file is a first run, not a save in progress: load() fills it
            FRESH.store(true, Ordering::Relaxed);
            return p;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return p,
        Err(_) => {}
    }
    let ad = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join("WispShot");
    let _ = std::fs::create_dir_all(&ad);
    let p = ad.join("wispshot.ini");
    migrate(&ad.parent().unwrap_or(&ad).join("Wisp").join("wisp.ini"), &p);
    p
}

/// The app was called "Wisp" before 0.2: carry an old settings file over once.
fn migrate(old: &std::path::Path, new: &std::path::Path) {
    if !new.exists() && old.exists() {
        let _ = std::fs::copy(old, new);
    }
}

use windows::Win32::UI::Shell::{FOLDERID_Pictures, FOLDERID_Videos};

fn default_dir(id: &windows::core::GUID) -> PathBuf {
    crate::output::known_folder(id).unwrap_or_else(std::env::temp_dir).join("WispShot")
}

pub fn default_save_dir() -> PathBuf {
    default_dir(&FOLDERID_Pictures)
}

pub fn default_video_dir() -> PathBuf {
    default_dir(&FOLDERID_Videos)
}

fn read_ini() -> String {
    match std::fs::read_to_string(ini_path()) {
        Ok(t) if !t.trim().is_empty() => t,
        _ => default_ini(),
    }
}

/// Write `cfg` back to wispshot.ini, keeping comments and unknown keys in place.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = |p: &PathBuf, d: PathBuf| if *p == d { String::new() } else { p.display().to_string() };
    let mode = |ask: bool| if ask { "ask" } else { "folder" }.to_string();
    let tf = |v: bool| if v { "true" } else { "false" }.to_string();
    let theme = match cfg.theme {
        Theme::System => "system",
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    let mut vals: Vec<(&str, &str, String)> = vec![
        ("general", "language", cfg.language.clone()),
        ("general", "theme", theme.to_string()),
        ("general", "check_updates", tf(cfg.check_updates)),
        ("editor", "toolbar_tools", cfg.bar_tools.join(" ")),
        ("editor", "toolbar_actions", cfg.bar_actions.join(" ")),
        ("hotkeys", "pause_in_fullscreen", tf(cfg.pause_fullscreen)),
        ("hotkeys", "pause_in_apps", cfg.pause_apps.iter().map(|a| a.ini()).collect::<Vec<_>>().join(", ")),
        ("hotkeys", "snipping_restore", tf(cfg.snipping_restore)),
        ("output", "save_dir", dir(&cfg.save_dir, default_save_dir())),
        ("output", "save_mode", mode(cfg.save_ask)),
        ("output", "notify", tf(cfg.save_notify)),
        ("output", "format", cfg.image_format.ext().to_string()),
        ("output", "jpeg_quality", cfg.jpeg_quality.to_string()),
        ("output", "frame", cfg.frame.ini().to_string()),
        ("record", "video_dir", dir(&cfg.video_dir, default_video_dir())),
        ("record", "save_mode", mode(cfg.video_ask)),
        ("record", "notify", tf(cfg.video_notify)),
        ("record", "format", if cfg.gif { "gif" } else { "mp4" }.to_string()),
        ("record", "gif_fps", cfg.gif_fps.to_string()),
        ("record", "gif_width", cfg.gif_width.to_string()),
        ("record", "fps", cfg.fps.to_string()),
        ("record", "cursor", tf(cfg.cursor)),
        ("record", "ink_fade", tf(cfg.ink_fade)),
        ("capture", "magnifier", tf(cfg.magnifier)),
        ("capture", "detect_elements", tf(cfg.detect_elements)),
        ("capture", "delay", cfg.capture_delay.to_string()),
        ("capture", "ocr_language", cfg.ocr_language.clone()),
        ("history", "enabled", tf(cfg.history)),
        ("history", "keep", cfg.history_keep.to_string()),
        ("history", "max_mb", cfg.history_mb.to_string()),
        ("history", "max_days", cfg.history_days.to_string()),
        ("history", "ocr", tf(cfg.history_ocr)),
        ("upload", "host", cfg.upload_host.ini().to_string()),
        ("upload", "url", cfg.upload_url.clone()),
        ("upload", "field", cfg.upload_field.clone()),
        ("upload", "asked", tf(cfg.upload_asked)),
    ];
    for i in 0..DEFS.len() {
        let (sect, name) = key_place(i);
        vals.push((sect, name, cfg.keys[i].clone()));
    }
    let text = set_values(&read_ini(), &vals, &["output.quick_save"]);
    std::fs::write(ini_path(), text)
}

fn kv_line(k: &str, v: &str) -> String {
    if v.is_empty() {
        format!("{} =", k)
    } else {
        format!("{} = {}", k, v)
    }
}

/// Replace `key = value` lines in their sections; append missing ones; drop `remove` keys.
fn set_values(text: &str, vals: &[(&str, &str, String)], remove: &[&str]) -> String {
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut done = vec![false; vals.len()];
    let mut sect = String::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim().to_string();
        if l.starts_with('[') && l.ends_with(']') {
            sect = l[1..l.len() - 1].trim().to_ascii_lowercase();
        } else if !l.starts_with(';') && !l.starts_with('#') {
            if let Some((k, _)) = l.split_once('=') {
                let k = k.trim().to_ascii_lowercase();
                if remove.contains(&format!("{}.{}", sect, k).as_str()) {
                    lines.remove(i);
                    continue;
                }
                if let Some(j) = vals.iter().position(|(s, key, _)| *s == sect && *key == k) {
                    lines[i] = kv_line(vals[j].1, &vals[j].2);
                    done[j] = true;
                }
            }
        }
        i += 1;
    }
    for (j, (s, k, v)) in vals.iter().enumerate() {
        if done[j] {
            continue;
        }
        let head = format!("[{}]", s);
        let line = kv_line(k, v);
        match lines.iter().position(|l| l.trim().eq_ignore_ascii_case(&head)) {
            Some(h) => {
                // after the last non-empty line of that section
                let mut end = h + 1;
                while end < lines.len() && !lines[end].trim().starts_with('[') {
                    end += 1;
                }
                while end > h + 1 && lines[end - 1].trim().is_empty() {
                    end -= 1;
                }
                lines.insert(end, line);
            }
            None => {
                lines.push(String::new());
                lines.push(head);
                lines.push(line);
            }
        }
    }
    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    out
}

static THEME: AtomicU8 = AtomicU8::new(0);
/// ini_path() just created the (empty) settings file.
static FRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FRAME_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the share frame is on in the settings (the editor starts from this).
pub fn frame_on() -> bool {
    FRAME_ON.load(Ordering::Relaxed)
}

/// Theme chosen in the settings (System / Light / Dark), for every window of this process.
pub fn theme() -> Theme {
    match THEME.load(Ordering::Relaxed) {
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => Theme::System,
    }
}

fn parse_kv(text: &str) -> std::collections::HashMap<String, String> {
    let mut kv = std::collections::HashMap::new();
    let mut sect = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with(';') || l.starts_with('#') || l.is_empty() {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            sect = l[1..l.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            kv.insert(format!("{}.{}", sect, k.trim().to_ascii_lowercase()), v.trim().to_string());
        }
    }
    kv
}

/// "SAPlogon, C:\x\app.exe | capture record; notepad.exe": programs, each with the
/// system-wide shortcuts (ini names) it keeps for itself after "|" (none listed: all).
fn parse_apps(v: &str) -> Vec<PauseApp> {
    let mut out: Vec<PauseApp> = vec![];
    for entry in v.split([',', ';']) {
        let (path, names) = entry.split_once('|').unwrap_or((entry, ""));
        let Some(mut app) = PauseApp::from_path(path) else { continue };
        for n in names.split_whitespace() {
            if let Some(d) = DEFS.iter().find(|d| d.scope == Scope::Global && d.ini.eq_ignore_ascii_case(n)) {
                if !app.keys.contains(&d.k) {
                    app.keys.push(d.k);
                }
            }
        }
        if !out.iter().any(|a| a.exe == app.exe) {
            out.push(app);
        }
    }
    out
}

/// Read wispshot.ini (creating it on first run) and apply language, theme and key
/// bindings to this process.
pub fn load() -> Config {
    let path = ini_path();
    let fresh = FRESH.swap(false, Ordering::Relaxed);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => t,
        // empty or locked: most likely a save in progress; use the defaults without writing them
        Ok(_) | Err(_) if path.exists() && !fresh => default_ini(),
        _ => {
            // first run: comments in the Windows display language
            crate::i18n::set("system");
            let t = default_ini();
            let _ = std::fs::write(&path, t.replace('\n', "\r\n"));
            t
        }
    };
    let kv = parse_kv(&text);
    let get = |k: &str, d: &str| kv.get(k).cloned().unwrap_or_else(|| d.to_string());
    let b = |k: &str, d: bool| match get(k, "").to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "evet" => true,
        "false" | "0" | "no" | "off" | "hayir" | "hayır" => false,
        _ => d,
    };
    let dir = |k: &str, id: &windows::core::GUID| {
        let v = get(k, "");
        if v.is_empty() {
            default_dir(id)
        } else {
            PathBuf::from(v)
        }
    };
    // save_mode wins; the old `quick_save = true` still means "folder"
    let ask = |k: &str, legacy: Option<&str>, d: bool| match get(k, "").to_ascii_lowercase().as_str() {
        "ask" | "sor" => true,
        "folder" | "auto" | "klasor" | "klasör" => false,
        _ => legacy.map_or(d, |l| !b(l, !d)),
    };
    let keys: Vec<String> = (0..DEFS.len())
        .map(|i| {
            let (sect, name) = key_place(i);
            match kv.get(&format!("{}.{}", sect, name)) {
                // a value that does not parse counts as "off", but is kept for the user to fix
                Some(v) => v.clone(),
                None => DEFS[i].default.to_string(),
            }
        })
        .collect();
    let theme = match get("general.theme", "system").to_ascii_lowercase().as_str() {
        "light" | "acik" | "açık" => Theme::Light,
        "dark" | "koyu" => Theme::Dark,
        _ => Theme::System,
    };
    let cfg = Config {
        keys,
        language: get("general.language", "system"),
        theme,
        pause_fullscreen: b("hotkeys.pause_in_fullscreen", true),
        pause_apps: parse_apps(&get("hotkeys.pause_in_apps", "")),
        check_updates: b("general.check_updates", true),
        bar_tools: parse_bar(&get("editor.toolbar_tools", DEFAULT_BAR_TOOLS), &BAR_TOOLS),
        bar_actions: parse_bar(&get("editor.toolbar_actions", DEFAULT_BAR_ACTIONS), &BAR_ACTIONS),
        snipping_restore: b("hotkeys.snipping_restore", false),
        save_dir: dir("output.save_dir", &FOLDERID_Pictures),
        save_ask: ask("output.save_mode", Some("output.quick_save"), true),
        save_notify: b("output.notify", true),
        image_format: crate::output::Fmt::parse(&get("output.format", "png")).unwrap_or(crate::output::Fmt::Png),
        jpeg_quality: get("output.jpeg_quality", "90").parse::<u32>().unwrap_or(90).clamp(1, 100),
        frame: crate::frame::Style::parse(&get("output.frame", "off")),
        video_dir: dir("record.video_dir", &FOLDERID_Videos),
        video_ask: ask("record.save_mode", None, false),
        video_notify: b("record.notify", true),
        gif: get("record.format", "mp4").eq_ignore_ascii_case("gif"),
        gif_fps: get("record.gif_fps", "15").parse::<u32>().unwrap_or(15).clamp(1, 50),
        gif_width: get("record.gif_width", "800").parse::<u32>().unwrap_or(800),
        fps: get("record.fps", "30").parse().unwrap_or(30),
        cursor: b("record.cursor", true),
        magnifier: b("capture.magnifier", true),
        detect_elements: b("capture.detect_elements", false),
        capture_delay: get("capture.delay", "3").parse::<u32>().unwrap_or(3).clamp(1, 60),
        ocr_language: get("capture.ocr_language", "auto"),
        history: b("history.enabled", false),
        history_keep: get("history.keep", "100").parse::<u32>().unwrap_or(100).max(1),
        history_mb: get("history.max_mb", "500").parse::<u32>().unwrap_or(500).max(1),
        history_days: get("history.max_days", "0").parse::<u32>().unwrap_or(0).min(3650),
        history_ocr: b("history.ocr", true),
        upload_host: crate::upload::Host::parse(&get("upload.host", "uguu")),
        upload_url: get("upload.url", "").trim().to_string(),
        upload_field: get("upload.field", "file").trim().to_string(),
        upload_asked: b("upload.asked", false),
        ink_fade: b("record.ink_fade", true),
    };
    crate::i18n::set(&cfg.language);
    crate::output::set_jpeg_quality(cfg.jpeg_quality);
    crate::ocr::set_language(&cfg.ocr_language);
    if let Ok(mut b) = BAR.write() {
        *b = (cfg.bar_tools.clone(), cfg.bar_actions.clone());
    }
    FRAME_ON.store(cfg.frame != crate::frame::Style::Off, Ordering::Relaxed);
    THEME.store(cfg.theme as u8, Ordering::Relaxed);
    keys::activate(&cfg.keys);
    cfg
}

/// Switch the theme for this process (Settings applies it before the tray reloads).
pub fn set_theme(t: Theme) {
    THEME.store(t as u8, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_apps_round_trip() {
        let v = parse_apps(r"SAPlogon, C:\Program Files (x86)\SAP\excel.exe | capture record; notepad.exe | nope");
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].exe.as_str(), v[0].path.as_str()), ("saplogon.exe", ""));
        assert_eq!(v[1].exe, "excel.exe");
        assert_eq!(v[1].keys, vec![K::Capture, K::Record]);
        assert!(v[1].pauses(K::Record) && !v[1].pauses(K::PinClipboard));
        assert!(v[2].keys.is_empty() && v[2].pauses(K::Capture));
        assert_eq!(v[1].ini(), r"C:\Program Files (x86)\SAP\excel.exe | capture record");
        assert_eq!(parse_apps(&v.iter().map(|a| a.ini()).collect::<Vec<_>>().join(", ")), v);
    }
}
