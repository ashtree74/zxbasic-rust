//! Single-owner state machine for the whole runtime.
//!
//! MVP-0: just holds a [`Display`] and paints the iconic Spectrum boot screen.
//! Subsequent MVPs grow it with editor, keyboard, program storage, interpreter,
//! and event loop hooks.

use crate::display::{Display, CHAR_H, FRAME_RGBA_LEN};

/// Top-level runtime state.
pub struct System {
    display: Display,
}

impl System {
    /// New system with the Spectrum boot screen pre-painted.
    pub fn new() -> Self {
        let mut display = Display::new();
        paint_boot_screen(&mut display);
        Self { display }
    }

    /// Length of the RGBA buffer that [`Self::render_into`] expects.
    pub const FRAME_RGBA_LEN: usize = FRAME_RGBA_LEN;

    /// Render the current screen state into an RGBA byte buffer of length
    /// [`Self::FRAME_RGBA_LEN`].
    pub fn render_into(&self, out: &mut [u8]) {
        self.display.render_into(out);
    }

    /// Advance one frame. MVP-0: no-op. Will drive the interpreter, blink,
    /// FLASH, etc. in later MVPs.
    pub fn frame(&mut self) {}
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

fn paint_boot_screen(d: &mut Display) {
    // The real Spectrum prints, two lines from the bottom:
    //   "© 1982 Sinclair Research Ltd"
    // We're not the original Spectrum (and we don't pretend to be), so we
    // adapt it slightly to mark this implementation:
    //   "© 2026 zxbasic-rust"        (one line above the bottom)
    //   "based on Sinclair 1982 ROM" (bottom line)
    //
    // The character cell font has no `©` glyph (ASCII 169 is outside 32..=127),
    // so we substitute "(c)".
    let line_a = "(c) 2026 zxbasic-rust";
    let line_b = "based on Sinclair 1982 ROM";
    d.print_str(0, CHAR_H - 2, line_a);
    d.print_str(0, CHAR_H - 1, line_b);
}
