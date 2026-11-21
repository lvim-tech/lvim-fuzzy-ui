//! A cell buffer with diffed flushing: every frame is drawn into a `Canvas`,
//! then only the cells that changed since the previous frame are written to
//! the terminal (inside a synchronized update, so there is no flicker).
//! Popups are drawn on top of the base view in the same buffer.

use std::io::Write;

use crossterm::style::{
    Attribute, Attributes, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::{cursor, queue};
use unicode_width::UnicodeWidthChar;

use crate::theme::Style;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme; empty for the trailing half of a wide char.
    pub ch: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: Attributes,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: " ".into(),
            fg: None,
            bg: None,
            attrs: Attributes::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

pub struct Canvas {
    pub w: u16,
    pub h: u16,
    cells: Vec<Cell>,
    /// Background for cells that do not set one (the theme's `bg`).
    pub base_bg: Option<Color>,
    pub base_fg: Option<Color>,
}

pub fn char_width(c: char) -> usize {
    if c == '\t' { 1 } else { c.width().unwrap_or(0) }
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Truncate `s` to at most `w` columns.
pub fn truncate(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

impl Canvas {
    pub fn new(w: u16, h: u16) -> Canvas {
        Canvas {
            w,
            h,
            cells: vec![Cell::default(); w as usize * h as usize],
            base_bg: None,
            base_fg: None,
        }
    }

    /// Copy `other` onto this canvas with its top-left at (x, y).
    pub fn blit(&mut self, other: &Canvas, x: u16, y: u16) {
        for oy in 0..other.h {
            for ox in 0..other.w {
                if let Some(i) = self.idx(x + ox, y + oy) {
                    self.cells[i] =
                        other.cells[oy as usize * other.w as usize + ox as usize].clone();
                }
            }
        }
    }

    pub fn clear(&mut self) {
        let c = Cell {
            bg: self.base_bg,
            fg: self.base_fg,
            ..Cell::default()
        };
        for x in self.cells.iter_mut() {
            *x = c.clone();
        }
    }

    fn idx(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.w && y < self.h).then(|| y as usize * self.w as usize + x as usize)
    }

    /// Fill a rectangle with spaces in `style`.
    pub fn fill(&mut self, r: Rect, style: Style) {
        for y in r.y..r.y.saturating_add(r.h).min(self.h) {
            for x in r.x..r.x.saturating_add(r.w).min(self.w) {
                if let Some(i) = self.idx(x, y) {
                    let c = &mut self.cells[i];
                    c.ch = " ".into();
                    c.fg = style.fg.or(self.base_fg);
                    c.bg = style.bg.or(self.base_bg);
                    c.attrs = style.attrs;
                }
            }
        }
    }

    /// Put `s` at (x, y), clipped to `max_x` (exclusive). Returns the next x.
    /// Unset colours in `style` keep what is already in the cell, so text
    /// drawn on a highlighted row keeps the row background.
    pub fn put(&mut self, x: u16, y: u16, s: &str, style: Style, max_x: u16) -> u16 {
        let mut cx = x;
        let limit = max_x.min(self.w);
        for ch in s.chars() {
            let ch = if ch.is_control() { '?' } else { ch };
            let cw = char_width(ch) as u16;
            if cw == 0 {
                continue;
            }
            if cx + cw > limit {
                break;
            }
            if let Some(i) = self.idx(cx, y) {
                let c = &mut self.cells[i];
                c.ch = ch.to_string();
                if style.fg.is_some() {
                    c.fg = style.fg;
                }
                if style.bg.is_some() {
                    c.bg = style.bg;
                }
                c.attrs = if style.regular {
                    Attributes::default()
                } else {
                    style.attrs
                };
            }
            if cw == 2
                && let Some(i) = self.idx(cx + 1, y)
            {
                let bg = self.cells[i - 1].bg;
                let c = &mut self.cells[i];
                c.ch = String::new();
                c.bg = bg;
            }
            cx += cw;
        }
        cx
    }

    /// An image icon (iconimg): its two placeholder cells at (x, y), the image id as their foreground, on
    /// `bg`, no attributes (an underline colour or a dim would change what the terminal reads). Returns the
    /// next x, or x unchanged when the two cells do not fit before `max_x`.
    pub fn put_image_icon(&mut self, x: u16, y: u16, id: u32, bg: Option<Color>, max_x: u16) -> u16 {
        if x + 2 > max_x.min(self.w) {
            return x;
        }
        for (k, cell) in crate::iconimg::CELLS.iter().enumerate() {
            if let Some(i) = self.idx(x + k as u16, y) {
                let c = &mut self.cells[i];
                c.ch = (*cell).to_string();
                c.fg = Some(crate::iconimg::id_color(id));
                c.bg = bg.or(self.base_bg);
                c.attrs = Attributes::default();
            }
        }
        x + 2
    }

    /// Placeholder cells of kitty image `id` (placed virtually over `cols` × `rows` cells) at (x, y),
    /// clipped to the canvas.
    pub fn put_image(&mut self, x: u16, y: u16, cols: u16, rows: u16, id: u32) {
        let dia = &crate::iconimg::DIACRITICS;
        let fg = Some(crate::iconimg::id_color(id));
        for r in 0..rows.min(dia.len() as u16) {
            for c in 0..cols.min(dia.len() as u16) {
                if let Some(i) = self.idx(x + c, y + r) {
                    let mut s = String::from('\u{10EEEE}');
                    s.push(char::from_u32(dia[r as usize]).unwrap_or('\u{305}'));
                    s.push(char::from_u32(dia[c as usize]).unwrap_or('\u{305}'));
                    let cell = &mut self.cells[i];
                    cell.ch = s;
                    cell.fg = fg;
                    cell.bg = self.base_bg;
                    cell.attrs = Attributes::default();
                }
            }
        }
    }

    /// A picture in half blocks: `rgba` is `w` × `h` pixels with w = the columns and h = 2 × the rows; each
    /// cell is `▀` with the upper pixel as its foreground and the lower as its background (alpha over the
    /// canvas background).
    pub fn put_halfblocks(&mut self, x: u16, y: u16, rgba: &[u8], w: u32, h: u32) {
        let bg = match self.base_bg {
            Some(Color::Rgb { r, g, b }) => (r, g, b),
            _ => (0, 0, 0),
        };
        let px = |px: u32, py: u32| -> Color {
            let i = ((py * w + px) * 4) as usize;
            let a = rgba.get(i + 3).copied().unwrap_or(255) as u32;
            let mix = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
            Color::Rgb {
                r: mix(rgba.get(i).copied().unwrap_or(0), bg.0),
                g: mix(rgba.get(i + 1).copied().unwrap_or(0), bg.1),
                b: mix(rgba.get(i + 2).copied().unwrap_or(0), bg.2),
            }
        };
        for row in 0..h.div_ceil(2) {
            for col in 0..w {
                if let Some(i) = self.idx(x + col as u16, y + row as u16) {
                    let top = px(col, row * 2);
                    let bottom = if row * 2 + 1 < h { px(col, row * 2 + 1) } else { top };
                    let cell = &mut self.cells[i];
                    cell.ch = "▀".to_string();
                    cell.fg = Some(top);
                    cell.bg = Some(bottom);
                    cell.attrs = Attributes::default();
                }
            }
        }
    }

    /// Set only the background of a horizontal span.
    pub fn set_bg(&mut self, x: u16, y: u16, w: u16, bg: Option<Color>) {
        for cx in x..x.saturating_add(w).min(self.w) {
            if let Some(i) = self.idx(cx, y) {
                self.cells[i].bg = bg;
            }
        }
    }

    /// Draw a box border around `r` with `chars` = [tl, t, tr, r, br, b, bl, l].
    pub fn border(&mut self, r: Rect, chars: &[&str; 8], sides: [bool; 4], style: Style) {
        if r.w < 2 || r.h < 1 {
            return;
        }
        let [top, right, bottom, left] = sides;
        let x2 = r.x + r.w - 1;
        let y2 = r.y + r.h - 1;
        if top {
            for x in r.x..=x2 {
                self.put(x, r.y, chars[1], style, x + 1);
            }
        }
        if bottom {
            for x in r.x..=x2 {
                self.put(x, y2, chars[5], style, x + 1);
            }
        }
        if left {
            for y in r.y..=y2 {
                self.put(r.x, y, chars[7], style, r.x + 1);
            }
        }
        if right {
            for y in r.y..=y2 {
                self.put(x2, y, chars[3], style, x2 + 1);
            }
        }
        if top && left {
            self.put(r.x, r.y, chars[0], style, r.x + 1);
        }
        if top && right {
            self.put(x2, r.y, chars[2], style, x2 + 1);
        }
        if bottom && right {
            self.put(x2, y2, chars[4], style, x2 + 1);
        }
        if bottom && left {
            self.put(r.x, y2, chars[6], style, r.x + 1);
        }
    }

    /// Write the cells that differ from `prev` (all of them when `prev` is
    /// None or has another size).
    pub fn flush<W: Write>(
        &self,
        out: &mut W,
        prev: Option<&Canvas>,
        pos: &mut Positioner,
    ) -> std::io::Result<()> {
        let full = prev.is_none_or(|p| p.w != self.w || p.h != self.h);
        let mut cur_fg: Option<Option<Color>> = None;
        let mut cur_bg: Option<Option<Color>> = None;
        let mut cur_attrs: Option<Attributes> = None;
        let mut last_pos: Option<(u16, u16)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                let i = y as usize * self.w as usize + x as usize;
                let c = &self.cells[i];
                if c.ch.is_empty() {
                    continue;
                }
                if !full && prev.is_some_and(|p| p.cells[i] == *c && !wide_changed(p, self, i)) {
                    continue;
                }
                if last_pos != Some((x, y)) {
                    pos.goto(out, x, y)?;
                }
                if cur_attrs != Some(c.attrs) {
                    queue!(out, SetAttribute(Attribute::Reset))?;
                    for a in Attribute::iterator() {
                        if c.attrs.has(a) && a != Attribute::Reset {
                            queue!(out, SetAttribute(a))?;
                        }
                    }
                    cur_attrs = Some(c.attrs);
                    cur_fg = None;
                    cur_bg = None;
                }
                if cur_fg != Some(c.fg) {
                    queue!(out, SetForegroundColor(c.fg.unwrap_or(Color::Reset)))?;
                    cur_fg = Some(c.fg);
                }
                if cur_bg != Some(c.bg) {
                    queue!(out, SetBackgroundColor(c.bg.unwrap_or(Color::Reset)))?;
                    cur_bg = Some(c.bg);
                }
                queue!(out, Print(&c.ch))?;
                let cw = c.ch.chars().next().map_or(1, char_width).max(1) as u16;
                last_pos = Some((x + cw, y));
            }
        }
        queue!(out, SetAttribute(Attribute::Reset))?;
        Ok(())
    }
}

/// Moves the terminal cursor. Full screen uses absolute positions; inline
/// (`--height`) mode moves relative to the first row of the finder, so it
/// never needs to ask the terminal where the cursor is.
pub struct Positioner {
    pub inline: bool,
    /// Current cursor row relative to the finder (inline mode).
    pub row: u16,
}

impl Positioner {
    pub fn goto<W: Write>(&mut self, out: &mut W, x: u16, y: u16) -> std::io::Result<()> {
        if self.inline {
            queue!(out, cursor::MoveToColumn(x))?;
            if y > self.row {
                queue!(out, cursor::MoveDown(y - self.row))?;
            } else if y < self.row {
                queue!(out, cursor::MoveUp(self.row - y))?;
            }
            self.row = y;
        } else {
            queue!(out, cursor::MoveTo(x, y))?;
        }
        Ok(())
    }
}

/// A wide char's trailing half changed: redraw the leading cell too.
fn wide_changed(prev: &Canvas, cur: &Canvas, i: usize) -> bool {
    let next = i + 1;
    next < cur.cells.len() && cur.cells[next].ch.is_empty() != prev.cells[next].ch.is_empty()
}

pub const BORDER_ROUNDED: [&str; 8] = ["╭", "─", "╮", "│", "╯", "─", "╰", "│"];
pub const BORDER_SHARP: [&str; 8] = ["┌", "─", "┐", "│", "┘", "─", "└", "│"];
pub const BORDER_BOLD: [&str; 8] = ["┏", "━", "┓", "┃", "┛", "━", "┗", "┃"];
pub const BORDER_DOUBLE: [&str; 8] = ["╔", "═", "╗", "║", "╝", "═", "╚", "║"];
pub const BORDER_DASHED: [&str; 8] = ["╭", "╌", "╮", "╎", "╯", "╌", "╰", "╎"];
pub const BORDER_BLOCK: [&str; 8] = ["▛", "▀", "▜", "▐", "▟", "▄", "▙", "▌"];
pub const BORDER_THINBLOCK: [&str; 8] = ["🭽", "▔", "🭾", "▕", "🭿", "▁", "🭼", "▏"];
pub const BORDER_ASCII: [&str; 8] = ["+", "-", "+", "|", "+", "-", "+", "|"];
