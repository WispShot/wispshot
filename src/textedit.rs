//! In-place text box editing for the Text tool: caret, selection, clipboard, word jumps
//! and a small typing history. Positions are UTF-16 indices, like DirectWrite uses.

use crate::gfx::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::*;

pub struct TextEdit {
    /// Top-left of the text in image coordinates.
    pub x: f32,
    pub y: f32,
    pub buf: Vec<u16>,
    pub caret: usize,
    /// Other end of the selection (== caret: nothing selected).
    pub anchor: usize,
    undo: Vec<(Vec<u16>, usize, usize)>,
    redo: Vec<(Vec<u16>, usize, usize)>,
    /// Last change was plain typing: further typing joins the same undo step.
    typing: bool,
}

fn is_low(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

fn is_word(u: u16) -> bool {
    char::from_u32(u as u32).map_or(true, |c| c.is_alphanumeric() || c == '_')
}

impl TextEdit {
    pub fn new(x: f32, y: f32, s: &str) -> TextEdit {
        let buf: Vec<u16> = s.encode_utf16().collect();
        let n = buf.len();
        TextEdit { x, y, buf, caret: n, anchor: n, undo: vec![], redo: vec![], typing: false }
    }

    pub fn text(&self) -> String {
        String::from_utf16_lossy(&self.buf)
    }

    pub fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_sel(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected(&self) -> String {
        let (a, b) = self.range();
        String::from_utf16_lossy(&self.buf[a..b])
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.buf.len();
        self.typing = false;
    }

    fn snapshot(&mut self, typing: bool) {
        if !(typing && self.typing) {
            self.undo.push((self.buf.clone(), self.caret, self.anchor));
            self.redo.clear();
        }
        self.typing = typing;
    }

    /// Ctrl+Z inside the box. False when there is nothing left to undo.
    pub fn undo(&mut self) -> bool {
        let Some(st) = self.undo.pop() else { return false };
        self.redo.push((std::mem::take(&mut self.buf), self.caret, self.anchor));
        (self.buf, self.caret, self.anchor) = st;
        self.typing = false;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(st) = self.redo.pop() else { return false };
        self.undo.push((std::mem::take(&mut self.buf), self.caret, self.anchor));
        (self.buf, self.caret, self.anchor) = st;
        self.typing = false;
        true
    }

    fn cut_sel(&mut self) {
        let (a, b) = self.range();
        self.buf.drain(a..b);
        self.caret = a;
        self.anchor = a;
    }

    /// Type or paste: replaces the selection.
    pub fn insert(&mut self, s: &[u16], typing: bool) {
        if s.is_empty() && !self.has_sel() {
            return;
        }
        self.snapshot(typing && !self.has_sel());
        self.cut_sel();
        let c = self.caret;
        self.buf.splice(c..c, s.iter().copied());
        self.caret = c + s.len();
        self.anchor = self.caret;
    }

    pub fn insert_str(&mut self, s: &str) {
        let w: Vec<u16> = s.replace("\r\n", "\n").encode_utf16().collect();
        self.insert(&w, false);
    }

    /// Remove the selection (true) if there is one.
    pub fn delete_sel(&mut self) -> bool {
        if !self.has_sel() {
            return false;
        }
        self.snapshot(false);
        self.cut_sel();
        true
    }

    /// Backspace (`forward` = Delete); `word` = with Ctrl.
    pub fn erase(&mut self, forward: bool, word: bool) {
        if self.delete_sel() {
            return;
        }
        let to = match (forward, word) {
            (false, false) => self.prev(self.caret),
            (true, false) => self.next(self.caret),
            (false, true) => self.word_left(self.caret),
            (true, true) => self.word_right(self.caret),
        };
        if to == self.caret {
            return;
        }
        self.snapshot(false);
        let (a, b) = (to.min(self.caret), to.max(self.caret));
        self.buf.drain(a..b);
        self.caret = a;
        self.anchor = a;
    }

    pub fn prev(&self, i: usize) -> usize {
        let mut i = i.saturating_sub(1);
        while i > 0 && is_low(self.buf[i]) {
            i -= 1;
        }
        i
    }

    pub fn next(&self, i: usize) -> usize {
        let n = self.buf.len();
        let mut i = (i + 1).min(n);
        while i < n && is_low(self.buf[i]) {
            i += 1;
        }
        i
    }

    pub fn word_left(&self, i: usize) -> usize {
        let mut i = i;
        while i > 0 && !is_word(self.buf[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(self.buf[i - 1]) {
            i -= 1;
        }
        i
    }

    pub fn word_right(&self, i: usize) -> usize {
        let n = self.buf.len();
        let mut i = i;
        while i < n && is_word(self.buf[i]) {
            i += 1;
        }
        while i < n && !is_word(self.buf[i]) && self.buf[i] != b'\n' as u16 {
            i += 1;
        }
        i
    }

    pub fn line_start(&self, i: usize) -> usize {
        self.buf[..i].iter().rposition(|&u| u == b'\n' as u16).map_or(0, |p| p + 1)
    }

    pub fn line_end(&self, i: usize) -> usize {
        self.buf[i..].iter().position(|&u| u == b'\n' as u16).map_or(self.buf.len(), |p| i + p)
    }

    /// The word (or run of spaces/punctuation) around i, for double-click.
    pub fn word_at(&self, i: usize) -> (usize, usize) {
        let n = self.buf.len();
        if n == 0 {
            return (0, 0);
        }
        let i = i.min(n - 1);
        let kind = is_word(self.buf[i]);
        let mut a = i;
        while a > 0 && is_word(self.buf[a - 1]) == kind && self.buf[a - 1] != b'\n' as u16 {
            a -= 1;
        }
        let mut b = i;
        while b < n && is_word(self.buf[b]) == kind && self.buf[b] != b'\n' as u16 {
            b += 1;
        }
        (a, b.max(a))
    }

    /// Put the caret at `pos`; `extend` keeps the anchor (Shift / drag).
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        self.caret = pos.min(self.buf.len());
        if !extend {
            self.anchor = self.caret;
        }
        self.typing = false;
    }

    // ------------------------------------------------------------ layout

    pub fn layout(&self, g: &Gfx, fmt: &IDWriteTextFormat) -> Option<IDWriteTextLayout> {
        unsafe { g.dw.CreateTextLayout(&self.buf, fmt, 100000.0, 100000.0).ok() }
    }

    /// Text index nearest to (px, py) in image coordinates.
    pub fn hit(&self, l: &IDWriteTextLayout, px: f32, py: f32) -> usize {
        let (mut trailing, mut inside) = (Default::default(), Default::default());
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            if l.HitTestPoint(px - self.x, py - self.y, &mut trailing, &mut inside, &mut m).is_err() {
                return self.caret;
            }
        }
        let i = m.textPosition as usize + if trailing.as_bool() { m.length as usize } else { 0 };
        i.min(self.buf.len())
    }

    /// Caret position: (x, top, height) in image coordinates.
    pub fn caret_pos(&self, l: &IDWriteTextLayout, i: usize) -> (f32, f32, f32) {
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.HitTestTextPosition(i as u32, false, &mut x, &mut y, &mut m);
        }
        (self.x + x, self.y + m.top, m.height)
    }

    /// One highlight rectangle per selected line part.
    pub fn sel_rects(&self, l: &IDWriteTextLayout) -> Vec<D2D_RECT_F> {
        let (a, b) = self.range();
        if a == b {
            return vec![];
        }
        let mut n = 0u32;
        let mut v = vec![DWRITE_HIT_TEST_METRICS::default(); 64];
        unsafe {
            let hit = |v: &mut [DWRITE_HIT_TEST_METRICS], n: &mut u32| l.HitTestTextRange(a as u32, (b - a) as u32, self.x, self.y, Some(v), n).is_ok();
            // more line parts than the buffer holds: `n` now has the real count
            if !hit(&mut v, &mut n) && n as usize > v.len() {
                v.resize(n as usize, DWRITE_HIT_TEST_METRICS::default());
                hit(&mut v, &mut n);
            }
            if n as usize > v.len() {
                return vec![];
            }
        }
        v.truncate(n as usize);
        v.iter()
            .map(|m| {
                // a selected line break shows as a small block, like other editors
                let w = if m.width < 1.0 { 4.0 } else { m.width };
                rc(m.left, m.top, m.left + w, m.top + m.height)
            })
            .collect()
    }

    /// The dashed frame around the box (never smaller than one empty line of `min_h`).
    pub fn frame(&self, l: &IDWriteTextLayout, min_h: f32) -> D2D_RECT_F {
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        let (w, h) = (m.widthIncludingTrailingWhitespace.max(4.0), m.height.max(min_h));
        rc(self.x - 3.0, self.y - 2.0, self.x + w + 6.0, self.y + h + 2.0)
    }

    /// Up/Down: same x on the line above/below.
    pub fn vertical(&self, l: &IDWriteTextLayout, down: bool) -> usize {
        let (x, top, h) = self.caret_pos(l, self.caret);
        let y = if down { top + h * 1.5 } else { top - h * 0.5 };
        if y < self.y {
            return 0;
        }
        self.hit(l, x, y)
    }
}
