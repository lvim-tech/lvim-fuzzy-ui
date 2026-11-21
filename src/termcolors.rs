//! Ask the terminal for its own colours: the default foreground (OSC 10),
//! background (OSC 11) and the 16-colour palette (OSC 4). A DA1 query goes last
//! as a sentinel — every terminal answers it — so the read stops as soon as the
//! answers are in, or after a short timeout when the terminal says nothing.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use crossterm::style::Color;

#[derive(Clone, Debug)]
pub struct TermColors {
    pub fg: Color,
    pub bg: Color,
    /// ANSI 0–15.
    pub palette: [Option<Color>; 16],
}

/// `rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel) → Color.
fn parse_rgb(s: &str) -> Option<Color> {
    let body = s.strip_prefix("rgb:")?;
    let mut it = body.split('/');
    let mut ch = || -> Option<u8> {
        let h = it.next()?;
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len() as u32)) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    Some(Color::Rgb {
        r: ch()?,
        g: ch()?,
        b: ch()?,
    })
}

/// Pull the OSC replies out of what the terminal sent back.
fn parse_replies(data: &str) -> (Option<Color>, Option<Color>, [Option<Color>; 16]) {
    let mut fg = None;
    let mut bg = None;
    let mut pal = [None; 16];
    for chunk in data.split("\x1b]").skip(1) {
        // the reply ends with BEL or ST (ESC \)
        let end = chunk.find(['\x07', '\x1b']).unwrap_or(chunk.len());
        let body = &chunk[..end];
        let mut parts = body.splitn(3, ';');
        match parts.next() {
            Some("10") => fg = parts.next().and_then(parse_rgb),
            Some("11") => bg = parts.next().and_then(parse_rgb),
            Some("4") => {
                if let (Some(n), Some(c)) = (
                    parts.next().and_then(|n| n.parse::<usize>().ok()),
                    parts.next(),
                ) && n < 16
                {
                    pal[n] = parse_rgb(c);
                }
            }
            _ => {}
        }
    }
    (fg, bg, pal)
}

/// Query /dev/tty (raw mode is switched on for the exchange and restored).
/// None when the terminal does not answer the fg/bg queries.
pub fn query() -> Option<TermColors> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if !was_raw {
        crossterm::terminal::enable_raw_mode().ok()?;
    }
    let mut q = String::from("\x1b]10;?\x07\x1b]11;?\x07");
    for n in 0..16 {
        q.push_str(&format!("\x1b]4;{n};?\x07"));
    }
    q.push_str("\x1b[c"); // DA1: the sentinel
    let ok = tty
        .write_all(q.as_bytes())
        .and_then(|_| tty.flush())
        .is_ok();
    let mut data = Vec::new();
    if ok {
        let fd = tty.as_raw_fd();
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut buf = [0u8; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the open tty, a bounded timeout.
            let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
            if r <= 0 {
                break;
            }
            match tty.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
            }
            // the DA1 answer (ESC [ ? … c) closes the exchange
            if let Some(p) = data.windows(3).rposition(|w| w == b"\x1b[?")
                && data[p..].contains(&b'c')
            {
                break;
            }
        }
    }
    if !was_raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    let (fg, bg, palette) = parse_replies(&String::from_utf8_lossy(&data));
    Some(TermColors {
        fg: fg?,
        bg: bg?,
        palette,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_osc_replies() {
        let data = "\x1b]10;rgb:5e5e/8888/8888\x07\x1b]11;rgb:2020/2727/2b2b\x1b\\\x1b]4;4;rgb:44/7c/a2\x07\x1b[?62;c";
        let (fg, bg, pal) = parse_replies(data);
        assert_eq!(
            fg,
            Some(Color::Rgb {
                r: 0x5e,
                g: 0x88,
                b: 0x88
            })
        );
        assert_eq!(
            bg,
            Some(Color::Rgb {
                r: 0x20,
                g: 0x27,
                b: 0x2b
            })
        );
        assert_eq!(
            pal[4],
            Some(Color::Rgb {
                r: 0x44,
                g: 0x7c,
                b: 0xa2
            })
        );
        assert_eq!(pal[0], None);
        assert_eq!(
            parse_rgb("rgb:f/0/8"),
            Some(Color::Rgb {
                r: 255,
                g: 0,
                b: 136
            })
        );
    }
}
