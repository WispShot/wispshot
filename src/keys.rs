//! Every keyboard shortcut of the app in one table. Each action has a default chord and
//! can be rebound in Settings; the bindings live in wispshot.ini ([hotkeys] for the
//! global ones, [keys] for the rest).
//!
//! Keys that work the same everywhere are not in the table on purpose: Esc (back/close),
//! Enter (copy), arrows (nudge), Delete (remove the picked drawing) and text editing keys.

use std::cell::RefCell;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// System-wide hotkeys (RegisterHotKey).
    Global,
    /// The capture editor.
    Editor,
    /// Drawing on a running recording.
    Ink,
    /// A pinned image.
    Pin,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum K {
    Capture,
    CaptureDelayed,
    Record,
    PinClipboard,
    Draw,
    Copy,
    CopyFile,
    Save,
    PinIt,
    Ocr,
    RecordArea,
    ScrollCapture,
    Redact,
    SelectMonitor,
    SelectDrawings,
    Undo,
    Redo,
    PickColor,
    Detect,
    EditText,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Marker,
    Text,
    Counter,
    Mosaic,
    Blur,
    InkPen,
    InkMarker,
    InkArrow,
    InkRect,
    InkCounter,
    InkColor1,
    InkColor2,
    InkColor3,
    InkColor4,
    InkFade,
    InkUndo,
    InkClear,
    PinCopy,
    PinCopyFile,
    PinSave,
    PinOcr,
    PinZoom,
}

pub struct Def {
    pub k: K,
    pub scope: Scope,
    /// Name in wispshot.ini; also the i18n label key ("key.<ini>").
    pub ini: &'static str,
    pub default: &'static str,
}

const fn d(k: K, scope: Scope, ini: &'static str, default: &'static str) -> Def {
    Def { k, scope, ini, default }
}

/// In the order of `K`.
pub const DEFS: &[Def] = &[
    d(K::Capture, Scope::Global, "capture", "F1"),
    d(K::CaptureDelayed, Scope::Global, "capture_delayed", "Ctrl+Shift+F1"),
    d(K::Record, Scope::Global, "record", "Shift+F1"),
    d(K::PinClipboard, Scope::Global, "pin_clipboard", "F3"),
    d(K::Draw, Scope::Global, "record_draw", "Shift+F2"),
    d(K::Copy, Scope::Editor, "editor.copy", "Ctrl+C"),
    d(K::CopyFile, Scope::Editor, "editor.copy_file", "Ctrl+Shift+C"),
    d(K::Save, Scope::Editor, "editor.save", "Ctrl+S"),
    d(K::PinIt, Scope::Editor, "editor.pin", "Ctrl+T"),
    d(K::Ocr, Scope::Editor, "editor.ocr", "Ctrl+O"),
    d(K::RecordArea, Scope::Editor, "editor.record", "Ctrl+R"),
    d(K::ScrollCapture, Scope::Editor, "editor.scroll", "Ctrl+L"),
    d(K::Redact, Scope::Editor, "editor.redact", "Ctrl+B"),
    d(K::SelectMonitor, Scope::Editor, "editor.select_screen", "Ctrl+A"),
    d(K::SelectDrawings, Scope::Editor, "editor.select_drawings", "Ctrl+Shift+A"),
    d(K::Undo, Scope::Editor, "editor.undo", "Ctrl+Z"),
    d(K::Redo, Scope::Editor, "editor.redo", "Ctrl+Y"),
    d(K::PickColor, Scope::Editor, "editor.pick_color", "C"),
    d(K::Detect, Scope::Editor, "editor.detect_elements", "D"),
    d(K::EditText, Scope::Editor, "editor.edit_text", "F2"),
    d(K::Rect, Scope::Editor, "editor.rect", "R"),
    d(K::Ellipse, Scope::Editor, "editor.ellipse", "E"),
    d(K::Arrow, Scope::Editor, "editor.arrow", "A"),
    d(K::Line, Scope::Editor, "editor.line", "L"),
    d(K::Pen, Scope::Editor, "editor.pen", "P"),
    d(K::Marker, Scope::Editor, "editor.marker", "M"),
    d(K::Text, Scope::Editor, "editor.text", "T"),
    d(K::Counter, Scope::Editor, "editor.counter", "N"),
    d(K::Mosaic, Scope::Editor, "editor.mosaic", "X"),
    d(K::Blur, Scope::Editor, "editor.blur", "B"),
    d(K::InkPen, Scope::Ink, "draw.pen", "P"),
    d(K::InkMarker, Scope::Ink, "draw.marker", "M"),
    d(K::InkArrow, Scope::Ink, "draw.arrow", "A"),
    d(K::InkRect, Scope::Ink, "draw.rect", "R"),
    d(K::InkCounter, Scope::Ink, "draw.counter", "N"),
    d(K::InkColor1, Scope::Ink, "draw.color1", "1"),
    d(K::InkColor2, Scope::Ink, "draw.color2", "2"),
    d(K::InkColor3, Scope::Ink, "draw.color3", "3"),
    d(K::InkColor4, Scope::Ink, "draw.color4", "4"),
    d(K::InkFade, Scope::Ink, "draw.fade", "F"),
    d(K::InkUndo, Scope::Ink, "draw.undo", "Ctrl+Z"),
    d(K::InkClear, Scope::Ink, "draw.clear", "Delete"),
    d(K::PinCopy, Scope::Pin, "pin.copy", "Ctrl+C"),
    d(K::PinCopyFile, Scope::Pin, "pin.copy_file", "Ctrl+Shift+C"),
    d(K::PinSave, Scope::Pin, "pin.save", "Ctrl+S"),
    d(K::PinOcr, Scope::Pin, "pin.ocr", "Ctrl+O"),
    d(K::PinZoom, Scope::Pin, "pin.zoom_100", "0"),
];

pub fn def(k: K) -> &'static Def {
    &DEFS[k as usize]
}

pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
pub const WIN: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub mods: u8,
    pub vk: u16,
}

const NAMES: &[(&str, u16)] = &[
    ("PrintScreen", 0x2C),
    ("Pause", 0x13),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("ScrollLock", 0x91),
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("Enter", 0x0D),
    ("Esc", 0x1B),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("`", 0xC0),
    ("-", 0xBD),
    ("=", 0xBB),
    ("[", 0xDB),
    ("]", 0xDD),
    (";", 0xBA),
    ("'", 0xDE),
    (",", 0xBC),
    (".", 0xBE),
    ("/", 0xBF),
    ("\\", 0xDC),
];

fn key_vk(k: &str) -> Option<u16> {
    let b = k.as_bytes();
    if b.len() == 1 && b[0].is_ascii_alphanumeric() {
        return Some(b[0].to_ascii_uppercase() as u16);
    }
    let lower = k.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(VK_F1.0 + n - 1);
        }
    }
    if let Some(n) = lower.strip_prefix("num").and_then(|n| n.parse::<u16>().ok()) {
        if n <= 9 {
            return Some(VK_NUMPAD0.0 + n);
        }
    }
    let alias = match lower.as_str() {
        "prtsc" | "print" | "snapshot" => "printscreen",
        "ins" => "insert",
        "del" => "delete",
        "escape" => "esc",
        "return" => "enter",
        "oem3" => "`",
        x => x,
    };
    NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(alias)).map(|(_, v)| *v)
}

fn vk_name(vk: u16) -> Option<String> {
    if (b'A' as u16..=b'Z' as u16).contains(&vk) || (b'0' as u16..=b'9' as u16).contains(&vk) {
        return Some((vk as u8 as char).to_string());
    }
    if (VK_F1.0..=VK_F24.0).contains(&vk) {
        return Some(format!("F{}", vk - VK_F1.0 + 1));
    }
    if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk) {
        return Some(format!("Num{}", vk - VK_NUMPAD0.0));
    }
    NAMES.iter().find(|(_, v)| *v == vk).map(|(n, _)| n.to_string())
}

pub fn parse(s: &str) -> Option<Chord> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut mods = 0;
    let mut vk = None;
    // "Ctrl++" style is not supported; '+' itself is "=" (Shift+=)
    for part in s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= CTRL,
            "alt" => mods |= ALT,
            "shift" => mods |= SHIFT,
            "win" | "super" => mods |= WIN,
            k => vk = key_vk(k),
        }
    }
    vk.map(|vk| Chord { mods, vk })
}

impl Chord {
    /// Canonical text, as stored in the ini ("Ctrl+Shift+A").
    pub fn text(&self) -> Option<String> {
        let mut p: Vec<String> = vec![];
        for (m, n) in [(CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift"), (WIN, "Win")] {
            if self.mods & m != 0 {
                p.push(n.into());
            }
        }
        p.push(vk_name(self.vk)?);
        Some(p.join("+"))
    }

    /// The chord being pressed right now with key `vk`.
    pub fn now(vk: u16) -> Chord {
        let down = |v: VIRTUAL_KEY| unsafe { GetKeyState(v.0 as i32) < 0 };
        let mut mods = 0;
        if down(VK_CONTROL) {
            mods |= CTRL;
        }
        if down(VK_MENU) {
            mods |= ALT;
        }
        if down(VK_SHIFT) {
            mods |= SHIFT;
        }
        if down(VK_LWIN) || down(VK_RWIN) {
            mods |= WIN;
        }
        Chord { mods, vk }
    }

    pub fn hotkey_mods(&self) -> HOT_KEY_MODIFIERS {
        let mut m = MOD_NOREPEAT;
        if self.mods & CTRL != 0 {
            m |= MOD_CONTROL;
        }
        if self.mods & ALT != 0 {
            m |= MOD_ALT;
        }
        if self.mods & SHIFT != 0 {
            m |= MOD_SHIFT;
        }
        if self.mods & WIN != 0 {
            m |= MOD_WIN;
        }
        m
    }
}

/// "Ctrl+Shift+A" -> "Ctrl + Shift + A"; punctuation keys show the character of the
/// user's keyboard layout (on a Turkish keyboard ";" is "Ş").
pub fn display(text: &str) -> String {
    let Some(c) = parse(text) else { return String::new() };
    let mut p: Vec<String> = vec![];
    for (m, n) in [(CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift"), (WIN, "Win")] {
        if c.mods & m != 0 {
            p.push(n.into());
        }
    }
    let key = if (0xBA..=0xE2).contains(&c.vk) {
        let ch = unsafe { MapVirtualKeyW(c.vk as u32, MAPVK_VK_TO_CHAR) } & 0xFFFF;
        char::from_u32(ch).filter(|c| !c.is_control()).map(|c| c.to_uppercase().to_string())
    } else {
        None
    };
    p.push(key.unwrap_or_else(|| vk_name(c.vk).unwrap_or_default()));
    p.join(" + ")
}

/// Is `k` a key we can bind (not a bare modifier)?
pub fn bindable(vk: u16) -> bool {
    vk_name(vk).is_some()
}

thread_local! {
    /// Bindings in effect for this process (from the config it loaded).
    static ACTIVE: RefCell<Vec<Option<Chord>>> = RefCell::new(DEFS.iter().map(|d| parse(d.default)).collect());
}

/// Use the bindings from `texts` (one per DEFS entry).
pub fn activate(texts: &[String]) {
    ACTIVE.with(|a| *a.borrow_mut() = texts.iter().map(|t| parse(t)).collect());
}

/// Which action of `scope` does key `vk` (with the current modifiers) trigger?
pub fn pressed(scope: Scope, vk: u16) -> Option<K> {
    let c = Chord::now(vk);
    ACTIVE.with(|a| {
        let a = a.borrow();
        DEFS.iter().zip(a.iter()).find(|(d, b)| d.scope == scope && **b == Some(c)).map(|(d, _)| d.k)
    })
}

/// Shortcut text for tooltips ("" when unbound).
pub fn label(k: K) -> String {
    ACTIVE.with(|a| {
        a.borrow().get(k as usize).copied().flatten().and_then(|c| c.text()).map(|t| display(&t)).unwrap_or_default()
    })
}

/// The other action that already uses `c` where it would clash with `k`, if any.
/// Global hotkeys clash with everything (they swallow the key system-wide).
pub fn clash(texts: &[String], k: K, c: Chord) -> Option<K> {
    let scope = def(k).scope;
    DEFS.iter()
        .zip(texts)
        .find(|(d, t)| {
            d.k != k && (d.scope == scope || d.scope == Scope::Global || scope == Scope::Global) && parse(t) == Some(c)
        })
        .map(|(d, _)| d.k)
}
