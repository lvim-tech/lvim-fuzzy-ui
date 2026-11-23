//! The terminal's keyboard protocol (kitty): without it Shift-Enter, Ctrl-Enter and the like reach a program as a plain Enter. A tool that wants
//! those keys calls [`push`] when it takes the terminal and [`pop`] when it gives it back; a terminal that does not speak the protocol is left alone.

use std::io::Write;
use std::sync::OnceLock;

use crossterm::event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::queue;

/// Is the protocol to be asked for? It is, unless `LVIM_KEYBOARD_PROTOCOL=0`. The terminal is not asked whether it speaks it (the question goes to
/// stdout, which a tool whose output is read by a shell function must keep clean): a terminal that does not simply ignores the request.
fn supported() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| !std::env::var("LVIM_KEYBOARD_PROTOCOL").is_ok_and(|v| v == "0"))
}

/// Ask the terminal to tell keys with modifiers apart (`CSI u`): Shift-Enter and Ctrl-Enter become their own keys. Must be called in raw mode.
pub fn push<W: Write>(out: &mut W) {
    if supported() {
        let _ = queue!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
}

/// Put the keyboard back as it was before [`push`].
pub fn pop<W: Write>(out: &mut W) {
    if supported() {
        let _ = queue!(out, PopKeyboardEnhancementFlags);
    }
}
