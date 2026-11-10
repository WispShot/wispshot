//! UI text in several languages. Each language is a plain `key = value` file in
//! `lang/`, compiled into the exe. A missing key falls back to English, then to the key
//! itself (so a forgotten string is easy to spot). Adding a language = adding a file
//! and a line in `LANGS`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// (code, name shown in Settings, file)
pub const LANGS: &[(&str, &str, &str)] =
    &[("en", "English", include_str!("../lang/en.txt")), ("tr", "Türkçe", include_str!("../lang/tr.txt"))];

static CUR: AtomicUsize = AtomicUsize::new(0);
static TABLES: [OnceLock<Vec<(&'static str, &'static str)>>; LANGS.len()] = [const { OnceLock::new() }; LANGS.len()];

fn table(i: usize) -> &'static [(&'static str, &'static str)] {
    TABLES[i].get_or_init(|| {
        let mut v: Vec<(&'static str, &'static str)> = LANGS[i]
            .2
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                if l.is_empty() || l.starts_with('#') {
                    return None;
                }
                let (k, v) = l.split_once('=')?;
                let v = v.trim();
                // "\n" in a value is a line break; only then is a copy needed
                let v: &'static str =
                    if v.contains("\\n") { Box::leak(v.replace("\\n", "\n").into_boxed_str()) } else { v };
                Some((k.trim(), v))
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    })
}

fn find(i: usize, key: &str) -> Option<&'static str> {
    let t = table(i);
    t.binary_search_by(|(k, _)| (*k).cmp(key)).ok().map(|j| t[j].1)
}

/// Pick the language: "system" (Windows display language), or a code like "tr".
pub fn set(setting: &str) {
    let code = if setting.eq_ignore_ascii_case("system") || setting.is_empty() {
        system_code()
    } else {
        setting.to_ascii_lowercase()
    };
    let i = LANGS.iter().position(|l| l.0 == code).unwrap_or(0);
    CUR.store(i, Ordering::Relaxed);
}

fn system_code() -> String {
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3FF;
    match id {
        0x1F => "tr".into(),
        _ => "en".into(),
    }
}

pub fn t(key: &'static str) -> &'static str {
    let i = CUR.load(Ordering::Relaxed);
    find(i, key).or_else(|| find(0, key)).unwrap_or(key)
}

/// `t` with `{0}`, `{1}`… replaced.
pub fn tf(key: &'static str, args: &[&str]) -> String {
    let mut s = t(key).to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{}}}", i), a);
    }
    s
}
