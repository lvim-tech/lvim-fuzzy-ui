//! fzf `--color` grammar and the colour slots used by the renderer.

use crossterm::style::{Attribute, Attributes, Color};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: Attributes,
    /// `regular` was given: drop inherited attributes.
    pub regular: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Fg,
    Bg,
    Hl,
    CurFg,
    CurBg,
    CurHl,
    SelFg,
    SelBg,
    SelHl,
    Gutter,
    Query,
    Disabled,
    Info,
    Border,
    Separator,
    Scrollbar,
    Label,
    Prompt,
    Pointer,
    Marker,
    Spinner,
    Header,
    Footer,
    Ghost,
    PreviewFg,
    PreviewBg,
    PreviewBorder,
    PreviewScrollbar,
    PreviewLabel,
    ListBorder,
    ListLabel,
    InputBorder,
    InputLabel,
    InputBg,
    HeaderBorder,
    HeaderLabel,
    FooterBorder,
    FooterLabel,
    Nth,
}

const NAMES: &[(&str, Slot)] = &[
    ("fg", Slot::Fg),
    ("list-fg", Slot::Fg),
    ("bg", Slot::Bg),
    ("list-bg", Slot::Bg),
    ("hl", Slot::Hl),
    ("fg+", Slot::CurFg),
    ("current-fg", Slot::CurFg),
    ("bg+", Slot::CurBg),
    ("current-bg", Slot::CurBg),
    ("hl+", Slot::CurHl),
    ("current-hl", Slot::CurHl),
    ("selected-fg", Slot::SelFg),
    ("selected-bg", Slot::SelBg),
    ("selected-hl", Slot::SelHl),
    ("gutter", Slot::Gutter),
    ("query", Slot::Query),
    ("input-fg", Slot::Query),
    ("disabled", Slot::Disabled),
    ("info", Slot::Info),
    ("border", Slot::Border),
    ("separator", Slot::Separator),
    ("scrollbar", Slot::Scrollbar),
    ("label", Slot::Label),
    ("border-label", Slot::Label),
    ("prompt", Slot::Prompt),
    ("pointer", Slot::Pointer),
    ("marker", Slot::Marker),
    ("spinner", Slot::Spinner),
    ("header", Slot::Header),
    ("header-fg", Slot::Header),
    ("footer", Slot::Footer),
    ("footer-fg", Slot::Footer),
    ("ghost", Slot::Ghost),
    ("preview-fg", Slot::PreviewFg),
    ("preview-bg", Slot::PreviewBg),
    ("preview-border", Slot::PreviewBorder),
    ("preview-scrollbar", Slot::PreviewScrollbar),
    ("preview-label", Slot::PreviewLabel),
    ("list-border", Slot::ListBorder),
    ("list-label", Slot::ListLabel),
    ("input-border", Slot::InputBorder),
    ("input-label", Slot::InputLabel),
    ("input-bg", Slot::InputBg),
    ("header-border", Slot::HeaderBorder),
    ("header-label", Slot::HeaderLabel),
    ("footer-border", Slot::FooterBorder),
    ("footer-label", Slot::FooterLabel),
    ("nth", Slot::Nth),
];

/// Slots that fall back to another slot when unset.
fn parent(s: Slot) -> Option<Slot> {
    Some(match s {
        Slot::CurFg => Slot::Fg,
        Slot::CurHl => Slot::Hl,
        Slot::SelFg => Slot::Fg,
        Slot::SelHl => Slot::Hl,
        Slot::SelBg => Slot::Bg,
        Slot::Query => Slot::Fg,
        Slot::Disabled => Slot::Query,
        Slot::Separator => Slot::Border,
        Slot::Scrollbar => Slot::Border,
        Slot::PreviewFg => Slot::Fg,
        Slot::PreviewBg => Slot::Bg,
        Slot::PreviewBorder => Slot::Border,
        Slot::PreviewScrollbar => Slot::PreviewBorder,
        Slot::PreviewLabel => Slot::Label,
        Slot::ListBorder => Slot::Border,
        Slot::ListLabel => Slot::Label,
        Slot::InputBorder => Slot::Border,
        Slot::InputLabel => Slot::Label,
        Slot::InputBg => Slot::Bg,
        Slot::HeaderBorder => Slot::Border,
        Slot::HeaderLabel => Slot::Label,
        Slot::FooterBorder => Slot::Border,
        Slot::FooterLabel => Slot::Label,
        Slot::Footer => Slot::Header,
        Slot::Ghost => Slot::Info,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Theme {
    slots: std::collections::HashMap<Slot, Style>,
    pub mono: bool,
}

fn c256(n: u8) -> Option<Color> {
    Some(Color::AnsiValue(n))
}

impl Theme {
    pub fn base(name: &str) -> Option<Theme> {
        let mut t = Theme {
            slots: Default::default(),
            mono: false,
        };
        let set = |t: &mut Theme, s: Slot, fg: Option<Color>, bg: Option<Color>| {
            t.slots.insert(
                s,
                Style {
                    fg,
                    bg,
                    ..Default::default()
                },
            );
        };
        match name {
            "dark" | "dark256" => {
                set(&mut t, Slot::Hl, c256(108), None);
                set(&mut t, Slot::CurFg, c256(254), None);
                set(&mut t, Slot::CurBg, None, c256(236));
                set(&mut t, Slot::CurHl, c256(151), None);
                set(&mut t, Slot::Info, c256(144), None);
                set(&mut t, Slot::Prompt, c256(110), None);
                set(&mut t, Slot::Pointer, c256(161), None);
                set(&mut t, Slot::Marker, c256(168), None);
                set(&mut t, Slot::Spinner, c256(148), None);
                set(&mut t, Slot::Header, c256(109), None);
                set(&mut t, Slot::Border, c256(59), None);
                set(&mut t, Slot::Gutter, None, c256(236));
                set(&mut t, Slot::Label, c256(145), None);
                set(&mut t, Slot::Ghost, c256(59), None);
            }
            "light" | "light256" => {
                set(&mut t, Slot::Hl, c256(30), None);
                set(&mut t, Slot::CurFg, c256(237), None);
                set(&mut t, Slot::CurBg, None, c256(254));
                set(&mut t, Slot::CurHl, c256(24), None);
                set(&mut t, Slot::Info, c256(101), None);
                set(&mut t, Slot::Prompt, c256(25), None);
                set(&mut t, Slot::Pointer, c256(161), None);
                set(&mut t, Slot::Marker, c256(168), None);
                set(&mut t, Slot::Spinner, c256(161), None);
                set(&mut t, Slot::Header, c256(31), None);
                set(&mut t, Slot::Border, c256(145), None);
                set(&mut t, Slot::Gutter, None, c256(251));
                set(&mut t, Slot::Label, c256(59), None);
                set(&mut t, Slot::Ghost, c256(145), None);
            }
            "base16" | "16" => {
                set(&mut t, Slot::Hl, Some(Color::DarkGreen), None);
                set(&mut t, Slot::CurFg, Some(Color::Grey), None);
                set(&mut t, Slot::CurBg, None, Some(Color::Black));
                set(&mut t, Slot::CurHl, Some(Color::Green), None);
                set(&mut t, Slot::Info, Some(Color::DarkYellow), None);
                set(&mut t, Slot::Prompt, Some(Color::DarkBlue), None);
                set(&mut t, Slot::Pointer, Some(Color::Red), None);
                set(&mut t, Slot::Marker, Some(Color::Magenta), None);
                set(&mut t, Slot::Spinner, Some(Color::DarkGreen), None);
                set(&mut t, Slot::Header, Some(Color::DarkCyan), None);
                set(&mut t, Slot::Border, Some(Color::DarkGrey), None);
                set(&mut t, Slot::Gutter, None, Some(Color::Black));
            }
            "bw" => t.mono = true,
            _ => return None,
        }
        Some(t)
    }

    /// Apply one `--color` value: `[BASE][,NAME:COLOR[:ATTR]...]...`.
    pub fn apply(&mut self, spec: &str) -> Result<(), String> {
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if !part.contains(':') {
                let base = Theme::base(part)
                    .ok_or_else(|| format!("invalid color specification: {part}"))?;
                *self = base;
                continue;
            }
            let mut it = part.split(':');
            let name = it.next().unwrap_or("");
            let slot = NAMES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, s)| *s)
                .ok_or_else(|| format!("invalid color specification: {part}"))?;
            let mut st = self.slots.get(&slot).copied().unwrap_or_default();
            let bg_slot = matches!(
                slot,
                Slot::Bg
                    | Slot::CurBg
                    | Slot::SelBg
                    | Slot::Gutter
                    | Slot::PreviewBg
                    | Slot::InputBg
            );
            for tok in it {
                if let Some(a) = parse_attr(tok) {
                    if tok == "regular" {
                        st.attrs = Attributes::default();
                        st.regular = true;
                    } else {
                        st.attrs.set(a);
                    }
                } else if let Some(c) = parse_color(tok) {
                    if bg_slot {
                        st.bg = c;
                    } else {
                        st.fg = c;
                    }
                } else {
                    return Err(format!("invalid color specification: {part}"));
                }
            }
            self.slots.insert(slot, st);
        }
        Ok(())
    }

    pub fn set(&mut self, s: Slot, st: Style) {
        self.slots.insert(s, st);
    }

    pub fn get(&self, s: Slot) -> Style {
        if self.mono {
            return Style::default();
        }
        let mut cur = Some(s);
        while let Some(x) = cur {
            if let Some(st) = self.slots.get(&x) {
                return *st;
            }
            cur = parent(x);
        }
        Style::default()
    }
}

fn parse_attr(s: &str) -> Option<Attribute> {
    Some(match s {
        "regular" => Attribute::Reset,
        "bold" | "strong" => Attribute::Bold,
        "dim" => Attribute::Dim,
        "italic" => Attribute::Italic,
        "underline" => Attribute::Underlined,
        "reverse" => Attribute::Reverse,
        "blink" => Attribute::SlowBlink,
        "strikethrough" => Attribute::CrossedOut,
        _ => return None,
    })
}

/// `-1` (default), 0..255, `#rrggbb`, or a colour name.
fn parse_color(s: &str) -> Option<Option<Color>> {
    if s == "-1" || s == "default" {
        return Some(None);
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(Some(Color::Rgb {
                r: (v >> 16) as u8,
                g: (v >> 8) as u8,
                b: v as u8,
            }));
        }
        return None;
    }
    if let Ok(n) = s.parse::<u8>() {
        return Some(Some(Color::AnsiValue(n)));
    }
    Some(Some(match s {
        "black" => Color::Black,
        "red" => Color::DarkRed,
        "green" => Color::DarkGreen,
        "yellow" => Color::DarkYellow,
        "blue" => Color::DarkBlue,
        "magenta" => Color::DarkMagenta,
        "cyan" => Color::DarkCyan,
        "white" => Color::Grey,
        "bright-black" => Color::DarkGrey,
        "bright-red" => Color::Red,
        "bright-green" => Color::Green,
        "bright-yellow" => Color::Yellow,
        "bright-blue" => Color::Blue,
        "bright-magenta" => Color::Magenta,
        "bright-cyan" => Color::Cyan,
        "bright-white" => Color::White,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lvim_colorscheme_fzf_extra_parses() {
        let mut t = Theme::base("dark").unwrap();
        for s in [
            "bg+:#22292d",
            "bg:#1c2327",
            "border:#617349",
            "fg:#649999",
            "gutter:#1c2327",
            "header:#e19556",
            "hl+:#ec665b",
            "hl:#e8594d",
            "info:#649192",
            "marker:#ed6445",
            "pointer:#ed6445",
            "prompt:#7e965a",
            "query:#619191:regular",
            "scrollbar:#617349",
            "separator:#e19556",
            "spinner:#e36245",
        ] {
            t.apply(s).unwrap();
        }
        assert_eq!(
            t.get(Slot::CurBg).bg,
            Some(Color::Rgb {
                r: 0x22,
                g: 0x29,
                b: 0x2d
            })
        );
        assert!(t.get(Slot::Query).regular);
        assert!(t.apply("nope:1").is_err());
        t.apply("light,fg:1:bold").unwrap();
        assert_eq!(t.get(Slot::Fg).fg, Some(Color::AnsiValue(1)));
    }
}
