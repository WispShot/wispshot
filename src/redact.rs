//! "Hide sensitive info": finds e-mail addresses, phone numbers, IBANs, card
//! numbers, Turkish ID numbers and API keys / tokens in OCR output and returns
//! the boxes to blur. Checksums (IBAN mod 97, Luhn, T.C. kimlik) keep numbers
//! that only look similar from being hidden; phones and tokens use shape rules.

use crate::ocr::Word;

/// Boxes (left, top, right, bottom) to blur, in the OCR image's pixels.
pub fn find(lines: &[Vec<Word>]) -> Vec<[f32; 4]> {
    let mut out = vec![];
    for line in lines {
        // the line as one string, with each word's char range in it
        let mut s: Vec<char> = vec![];
        let mut spans = vec![];
        for (w, _) in line {
            if !s.is_empty() {
                s.push(' ');
            }
            let a = s.len();
            s.extend(w.chars());
            spans.push((a, s.len()));
        }
        let mut hits: Vec<(usize, usize)> = vec![];
        for (i, &(a, b)) in spans.iter().enumerate() {
            let w = &line[i].0;
            if is_email(w) || is_token(w) {
                hits.push((a, b));
            }
        }
        hits.extend(numbers(&s));
        hits.extend(ibans(&s));
        for (a, b) in hits {
            // union of the words the match touches
            let mut r: Option<[f32; 4]> = None;
            for (i, &(wa, wb)) in spans.iter().enumerate() {
                if wa < b && a < wb {
                    let q = line[i].1;
                    r = Some(match r {
                        None => q,
                        Some(p) => [p[0].min(q[0]), p[1].min(q[1]), p[2].max(q[2]), p[3].max(q[3])],
                    });
                }
            }
            if let Some(r) = r {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
    }
    out
}

fn trim(w: &str) -> &str {
    w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '(' | ')' | '"' | '\'' | '<' | '>' | '[' | ']' | '.'))
}

fn is_email(w: &str) -> bool {
    let w = trim(w);
    let Some((user, host)) = w.split_once('@') else { return false };
    let Some((name, tld)) = host.rsplit_once('.') else { return false };
    !user.is_empty() && !name.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_alphabetic())
}

/// API keys, tokens, secrets: known prefixes, or a long random-looking run.
fn is_token(w: &str) -> bool {
    let w = trim(w);
    const PREFIXES: [&str; 12] =
        ["sk-", "sk_", "pk_", "rk_", "ghp_", "gho_", "ghs_", "github_pat_", "xox", "AKIA", "AIza", "eyJ"];
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=');
    if w.contains("://") || w.contains('@') || !w.chars().all(ok) {
        return false;
    }
    if w.len() >= 16 && PREFIXES.iter().any(|p| w.starts_with(p)) {
        return true;
    }
    if w.len() < 24 {
        return false;
    }
    let letters = w.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
    let upper = w.chars().filter(|c| c.is_ascii_uppercase()).count();
    let lower = w.chars().filter(|c| c.is_ascii_lowercase()).count();
    // mixed digits and letters of both cases: not a word, not a plain number
    digits >= 3 && letters >= 6 && (upper >= 2 && lower >= 2 || digits >= 6)
}

/// Digit runs across words (with spaces, dashes, dots, parentheses): cards, IDs, phones.
fn numbers(s: &[char]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut i = 0;
    while i < s.len() {
        if !(s[i].is_ascii_digit() || s[i] == '+' && s.get(i + 1).is_some_and(|c| c.is_ascii_digit())) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        let mut last_digit = i;
        while j < s.len() && (s[j].is_ascii_digit() || matches!(s[j], ' ' | '-' | '.' | '(' | ')' | '+' | '/')) {
            if s[j].is_ascii_digit() {
                last_digit = j;
            }
            j += 1;
        }
        let end = last_digit + 1;
        let run: String = s[start..end].iter().collect();
        let digits: Vec<u8> = run.bytes().filter(|b| b.is_ascii_digit()).map(|b| b - b'0').collect();
        if luhn_card(&digits) || tc_kimlik(&digits) || phone(&run, &digits) {
            out.push((start, end));
        }
        i = end.max(i + 1);
    }
    out
}

fn luhn_card(d: &[u8]) -> bool {
    if !(13..=19).contains(&d.len()) {
        return false;
    }
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| {
            let x = x as u32;
            if i % 2 == 1 {
                let y = x * 2;
                if y > 9 { y - 9 } else { y }
            } else {
                x
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// T.C. kimlik numarası: 11 digits, not starting with 0, two check digits.
fn tc_kimlik(d: &[u8]) -> bool {
    if d.len() != 11 || d[0] == 0 {
        return false;
    }
    let odd: u32 = [0, 2, 4, 6, 8].iter().map(|&i| d[i] as u32).sum();
    let even: u32 = [1, 3, 5, 7].iter().map(|&i| d[i] as u32).sum();
    let d10 = ((odd * 7 + 100 - even) % 10) as u8;
    let d11 = (d[..10].iter().map(|&x| x as u32).sum::<u32>() % 10) as u8;
    d[9] == d10 && d[10] == d11
}

fn phone(run: &str, d: &[u8]) -> bool {
    if !(10..=13).contains(&d.len()) {
        return false;
    }
    // +country, a leading 0 or an area code in parentheses: written like a phone
    if run.starts_with('+') || run.starts_with('0') || run.starts_with('(') {
        return true;
    }
    // otherwise only the 3-3-4 grouping (555-123-4567); dates and times group differently
    let groups: Vec<usize> =
        run.split(|c: char| !c.is_ascii_digit()).filter(|g| !g.is_empty()).map(|g| g.len()).collect();
    groups == [3, 3, 4]
}

/// IBAN: two letters, two check digits, then up to 30 letters/digits (spaces allowed).
fn ibans(s: &[char]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut i = 0;
    while i + 4 <= s.len() {
        let starts_word = i == 0 || !s[i - 1].is_alphanumeric();
        if !(starts_word
            && s[i].is_ascii_uppercase()
            && s[i + 1].is_ascii_uppercase()
            && s[i + 2].is_ascii_digit()
            && s[i + 3].is_ascii_digit())
        {
            i += 1;
            continue;
        }
        // collect the alphanumerics, remembering where each one sits
        let mut chars = vec![];
        let mut j = i;
        while j < s.len() && chars.len() < 34 && (s[j].is_ascii_alphanumeric() || s[j] == ' ') {
            if s[j] != ' ' {
                chars.push((s[j].to_ascii_uppercase(), j));
            }
            j += 1;
        }
        // the longest prefix that passes the checksum
        let found = (15..=chars.len()).rev().find(|&n| iban_ok(&chars[..n].iter().map(|c| c.0).collect::<Vec<_>>()));
        match found {
            Some(n) => {
                let end = chars[n - 1].1 + 1;
                out.push((i, end));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

fn iban_ok(c: &[char]) -> bool {
    // move the first four to the end, letters become 10..35, the number mod 97 must be 1
    let mut rem: u32 = 0;
    for &ch in c[4..].iter().chain(&c[..4]) {
        let v = match ch {
            '0'..='9' => ch as u32 - '0' as u32,
            'A'..='Z' => ch as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(words: &[&str]) -> Vec<Word> {
        words.iter().enumerate().map(|(i, w)| (w.to_string(), [i as f32 * 10.0, 0.0, i as f32 * 10.0 + 8.0, 10.0])).collect()
    }

    fn hidden(words: &[&str]) -> Vec<[f32; 4]> {
        find(&[line(words)])
    }

    #[test]
    fn finds_sensitive_things() {
        assert_eq!(hidden(&["mail:", "efe@example.com,", "thanks"]).len(), 1);
        // a valid test IBAN, written in groups
        assert_eq!(hidden(&["IBAN", "TR33", "0006", "1005", "1978", "6457", "8413", "26"]), vec![[10.0, 0.0, 78.0, 10.0]]);
        assert_eq!(hidden(&["card", "4111", "1111", "1111", "1111"]).len(), 1);
        assert_eq!(hidden(&["TC:", "10000000146"]).len(), 1);
        assert_eq!(hidden(&["tel", "+90", "532", "123", "45", "67"]), vec![[10.0, 0.0, 58.0, 10.0]]);
        assert_eq!(hidden(&["key", "sk-proj-a1B2c3D4e5F6g7H8"]).len(), 1);
        assert_eq!(hidden(&["AKIAIOSFODNN7EXAMPLE"]).len(), 1);
        assert_eq!(hidden(&["call", "555-123-4567"]).len(), 1);
        assert_eq!(hidden(&["0532", "123", "45", "67"]).len(), 1);
    }

    #[test]
    fn leaves_ordinary_text() {
        assert!(hidden(&["Toplam", "1.250,00", "TL", "2026-11-11", "17:53"]).is_empty());
        assert!(hidden(&["TR33", "0006", "1005", "1978", "6457", "8413", "27"]).is_empty()); // bad checksum
        assert!(hidden(&["4111", "1111", "1111", "1112"]).is_empty()); // fails Luhn
        assert!(hidden(&["12345678901"]).is_empty()); // not a valid kimlik no
        assert!(hidden(&["internationalization", "configuration"]).is_empty());
        assert!(hidden(&["https://example.com/a/very/long/path/that/goes/on"]).is_empty());
    }
}
