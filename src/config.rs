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

#[derive(Clone)]
pub struct Config {
    /// One binding per `keys::DEFS` entry ("" = off).
    pub keys: Vec<String>,
    /// "system" or a language code ("en", "tr").
    pub language: String,
    pub theme: Theme,
    /// Release the hotkeys while a full-screen app (game, video) is in front.
    pub pause_fullscreen: bool,
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
    /// Keep finished captures in the history folder.
    pub history: bool,
    pub history_keep: u32,
    pub history_mb: u32,
    /// Delete history images older than this many days (0: never).
    pub history_days: u32,
    /// Read the text in history images (for searching).
    pub history_ocr: bool,
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
    s += "\n[hotkeys]\n";
    for d in DEFS.iter().filter(|d| d.scope == Scope::Global) {
        s += &format!("{} = {}\n", d.ini, d.default);
    }
    s += &format!("; {}\npause_in_fullscreen = true\n", t("ini.pause_fullscreen"));
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
        "\n[capture]\nmagnifier = true\n; {}\ndetect_elements = false\n; {}\ndelay = 3\n",
        t("ini.detect"),
        t("ini.delay")
    );
    s += &format!(
        "\n[history]\n; {}\nenabled = false\n; {}\nkeep = 100\nmax_mb = 500\n; {}\nmax_days = 0\n; {}\nocr = true\n",
        t("ini.history"),
        t("ini.history_limit"),
        t("ini.history_days"),
        t("ini.history_ocr")
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
    if p.exists() || std::fs::OpenOptions::new().create(true).append(true).open(&p).map(|_| ()).is_ok() {
        return p;
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
        ("hotkeys", "pause_in_fullscreen", tf(cfg.pause_fullscreen)),
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
        ("history", "enabled", tf(cfg.history)),
        ("history", "keep", cfg.history_keep.to_string()),
        ("history", "max_mb", cfg.history_mb.to_string()),
        ("history", "max_days", cfg.history_days.to_string()),
        ("history", "ocr", tf(cfg.history_ocr)),
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

/// Read wispshot.ini (creating it on first run) and apply language, theme and key
/// bindings to this process.
pub fn load() -> Config {
    let path = ini_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => t,
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
        history: b("history.enabled", false),
        history_keep: get("history.keep", "100").parse::<u32>().unwrap_or(100).max(1),
        history_mb: get("history.max_mb", "500").parse::<u32>().unwrap_or(500).max(1),
        history_days: get("history.max_days", "0").parse::<u32>().unwrap_or(0).min(3650),
        history_ocr: b("history.ocr", true),
        ink_fade: b("record.ink_fade", true),
    };
    crate::i18n::set(&cfg.language);
    crate::output::set_jpeg_quality(cfg.jpeg_quality);
    FRAME_ON.store(cfg.frame != crate::frame::Style::Off, Ordering::Relaxed);
    THEME.store(cfg.theme as u8, Ordering::Relaxed);
    keys::activate(&cfg.keys);
    cfg
}

/// Switch the theme for this process (Settings applies it before the tray reloads).
pub fn set_theme(t: Theme) {
    THEME.store(t as u8, Ordering::Relaxed);
}
