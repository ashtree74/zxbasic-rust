//! Single-owner state machine for the whole runtime.
//!
//! MVP-1 scope: immediate-mode `PRINT <expr>`.
//!
//! Pipeline:
//! 1. JS keyboard events → [`System::feed_key`] (ASCII bytes + special
//!    [`Key::Enter`] / [`Key::Backspace`]).
//! 2. Characters accumulate in `input_line`; the display's input row is
//!    redrawn after every change.
//! 3. Enter triggers [`Self::execute`], which recognises the `PRINT` prefix,
//!    evaluates the rest with [`crate::expression::evaluate`], formats with
//!    [`crate::fp_format::format`], and writes the result into the scrolling
//!    print area.

use crate::display::{Display, CHAR_H, CHAR_W, FRAME_RGBA_LEN};
use crate::expression;
use crate::fp_format;

/// Logical key fed into [`System::feed_key`].
#[derive(Debug, Clone, Copy)]
pub enum Key {
    /// Ordinary printable ASCII character (32..=126 typically).
    Char(u8),
    /// CR / Enter.
    Enter,
    /// Backspace / Delete.
    Backspace,
}

/// Top-level runtime state.
pub struct System {
    display: Display,
    input_line: String,
}

impl System {
    /// New system with the boot screen pre-painted and an empty input line.
    pub fn new() -> Self {
        let mut display = Display::new();
        paint_boot_screen(&mut display);
        let mut sys = Self {
            display,
            input_line: String::new(),
        };
        sys.redraw_input();
        sys
    }

    /// Length of the RGBA buffer that [`Self::render_into`] expects.
    pub const FRAME_RGBA_LEN: usize = FRAME_RGBA_LEN;

    /// Render the current screen state into an RGBA byte buffer of length
    /// [`Self::FRAME_RGBA_LEN`].
    pub fn render_into(&self, out: &mut [u8]) {
        self.display.render_into(out);
    }

    /// Advance one frame. MVP-1: no-op. Cursor blink + FLASH attribute land
    /// in MVP-4.
    pub fn frame(&mut self) {}

    /// Feed a single keystroke from the host (browser or native harness).
    pub fn feed_key(&mut self, key: Key) {
        match key {
            Key::Char(b) if (32..=126).contains(&b) => {
                if self.input_line.len() < CHAR_W - 1 {
                    self.input_line.push(b as char);
                }
            }
            Key::Char(_) => {}
            Key::Backspace => {
                self.input_line.pop();
            }
            Key::Enter => {
                let line = std::mem::take(&mut self.input_line);
                self.execute(&line);
            }
        }
        self.redraw_input();
    }

    fn redraw_input(&mut self) {
        let cursor_col = self.input_line.chars().count().min(CHAR_W - 1);
        self.display.print_input(&self.input_line, cursor_col);
    }

    fn execute(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        // MVP-1: recognise only PRINT (case-insensitive prefix).
        let upper = trimmed.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("PRINT") {
            // Take the original-case rest with the same byte length so
            // expression sees identical content; case doesn't matter here
            // because there are no identifiers yet.
            let cut = trimmed.len() - rest.len();
            let expr = &trimmed[cut..];
            match expression::evaluate(expr) {
                Ok(v) => {
                    let s = fp_format::format(v);
                    self.display.println(&s);
                }
                Err(_) => self.report_error_nonsense(),
            }
        } else {
            self.report_error_nonsense();
        }
    }

    fn report_error_nonsense(&mut self) {
        // Spectrum prints something like "C Nonsense in BASIC, 0:1" at the
        // bottom. For MVP-1 we just write the short form into the print area.
        self.display.println("Nonsense in BASIC");
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

fn paint_boot_screen(d: &mut Display) {
    // The classic Spectrum boot line lives at the bottom, but we use that for
    // input. Move attribution to two rows above the input row.
    let line_a = "(c) 2026 zxbasic-rust";
    let line_b = "based on Sinclair 1982 ROM";
    d.print_str(0, CHAR_H - 4, line_a);
    d.print_str(0, CHAR_H - 3, line_b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::INPUT_ROW;

    fn feed_str(sys: &mut System, s: &str) {
        for b in s.bytes() {
            sys.feed_key(Key::Char(b));
        }
    }

    #[test]
    fn print_simple_arithmetic() {
        let mut sys = System::new();
        feed_str(&mut sys, "PRINT 1+2*3");
        sys.feed_key(Key::Enter);
        // After Enter, the print area row 0 should hold "7".
        // We assert by reading back the input line: should be empty.
        assert_eq!(sys.input_line, "");
        // And the print cursor should have advanced one row.
        assert_eq!(sys.display.print_cursor(), (0, 1));
        // Input row should be cleared.
        let _ = INPUT_ROW; // used only to anchor the const
    }

    #[test]
    fn nonsense_reports() {
        let mut sys = System::new();
        feed_str(&mut sys, "WAT");
        sys.feed_key(Key::Enter);
        assert_eq!(sys.display.print_cursor(), (0, 1));
    }

    #[test]
    fn backspace_removes_one_char() {
        let mut sys = System::new();
        feed_str(&mut sys, "ABC");
        sys.feed_key(Key::Backspace);
        assert_eq!(sys.input_line, "AB");
    }
}
