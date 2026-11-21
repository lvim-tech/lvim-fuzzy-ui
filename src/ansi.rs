//! ANSI SGR parsing for `--ansi` items and preview output. Non-SGR escape
//! sequences (cursor movement, OSC titles, ...) are dropped.

use crossterm::style::{Attribute, Attributes, Color};

use crate::theme::Style;

/// A run of text starting at `start` (byte offset into the stripped text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: u32,
    pub style: Style,
}

/// Strip escape sequences from `s`; return the plain text, the style runs
/// and the style in effect at the end (carried to the next line).
pub fn parse(s: &str, mut style: Style) -> (String, Vec<Run>, Style) {
    let mut out = String::with_capacity(s.len());
    let mut runs: Vec<Run> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    let push_run = |runs: &mut Vec<Run>, at: usize, st: Style| {
        if let Some(last) = runs.last_mut() {
            if last.start as usize == at {
                last.style = st;
                return;
            }
            if last.style == st {
                return;
            }
        } else if st == Style::default() {
            return;
        }
        runs.push(Run {
            start: at as u32,
            style: st,
        });
    };
    if style != Style::default() {
        runs.push(Run { start: 0, style });
    }
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() {
            match b[i + 1] {
                b'[' => {
                    // CSI: params then a final byte in 0x40..=0x7e
                    let mut j = i + 2;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == b'm' {
                        apply_sgr(&s[i + 2..j], &mut style);
                        push_run(&mut runs, out.len(), style);
                    }
                    i = j + 1;
                    continue;
                }
                b']' => {
                    // OSC: until BEL or ST
                    let mut j = i + 2;
                    while j < b.len() {
                        if b[j] == 0x07 {
                            j += 1;
                            break;
                        }
                        if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'\\' {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                _ => {
                    i += 2;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        if ch != '\r' {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    (out, runs, style)
}

fn color_256(n: u16) -> Color {
    Color::AnsiValue(n as u8)
}

fn basic(n: u16, bright: bool) -> Color {
    match (n, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::DarkRed,
        (2, false) => Color::DarkGreen,
        (3, false) => Color::DarkYellow,
        (4, false) => Color::DarkBlue,
        (5, false) => Color::DarkMagenta,
        (6, false) => Color::DarkCyan,
        (7, false) => Color::Grey,
        (0, true) => Color::DarkGrey,
        (1, true) => Color::Red,
        (2, true) => Color::Green,
        (3, true) => Color::Yellow,
        (4, true) => Color::Blue,
        (5, true) => Color::Magenta,
        (6, true) => Color::Cyan,
        _ => Color::White,
    }
}

fn apply_sgr(params: &str, st: &mut Style) {
    let nums: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let mut k = 0;
    while k < nums.len() {
        let n = nums[k];
        match n {
            0 => *st = Style::default(),
            1 => st.attrs.set(Attribute::Bold),
            2 => st.attrs.set(Attribute::Dim),
            3 => st.attrs.set(Attribute::Italic),
            4 => st.attrs.set(Attribute::Underlined),
            5 => st.attrs.set(Attribute::SlowBlink),
            7 => st.attrs.set(Attribute::Reverse),
            9 => st.attrs.set(Attribute::CrossedOut),
            22 => {
                st.attrs.unset(Attribute::Bold);
                st.attrs.unset(Attribute::Dim);
            }
            23 => st.attrs.unset(Attribute::Italic),
            24 => st.attrs.unset(Attribute::Underlined),
            25 => st.attrs.unset(Attribute::SlowBlink),
            27 => st.attrs.unset(Attribute::Reverse),
            29 => st.attrs.unset(Attribute::CrossedOut),
            30..=37 => st.fg = Some(basic(n - 30, false)),
            39 => st.fg = None,
            40..=47 => st.bg = Some(basic(n - 40, false)),
            49 => st.bg = None,
            90..=97 => st.fg = Some(basic(n - 90, true)),
            100..=107 => st.bg = Some(basic(n - 100, true)),
            38 | 48 => {
                let c = match nums.get(k + 1) {
                    Some(5) => {
                        let c = nums.get(k + 2).map(|&v| color_256(v));
                        k += 2;
                        c
                    }
                    Some(2) => {
                        let c = match (nums.get(k + 2), nums.get(k + 3), nums.get(k + 4)) {
                            (Some(&r), Some(&g), Some(&b)) => Some(Color::Rgb {
                                r: r as u8,
                                g: g as u8,
                                b: b as u8,
                            }),
                            _ => None,
                        };
                        k += 4;
                        c
                    }
                    _ => None,
                };
                if n == 38 {
                    st.fg = c;
                } else {
                    st.bg = c;
                }
            }
            _ => {}
        }
        k += 1;
    }
    st.regular = false;
    if st.attrs == Attributes::default() && st.fg.is_none() && st.bg.is_none() {
        *st = Style::default();
    }
}

/// Style in effect at byte offset `at`.
pub fn style_at(runs: &[Run], at: usize) -> Style {
    match runs.binary_search_by(|r| (r.start as usize).cmp(&at)) {
        Ok(i) => runs[i].style,
        Err(0) => Style::default(),
        Err(i) => runs[i - 1].style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_and_styles() {
        let (t, r, _) = parse(
            "\x1b[31mred\x1b[0m plain \x1b[1;38;5;208mx\x1b]0;title\x07y",
            Style::default(),
        );
        assert_eq!(t, "red plain xy");
        assert_eq!(style_at(&r, 0).fg, Some(Color::DarkRed));
        assert_eq!(style_at(&r, 4), Style::default());
        let s = style_at(&r, 10);
        assert_eq!(s.fg, Some(Color::AnsiValue(208)));
        assert!(s.attrs.has(Attribute::Bold));
    }
}
