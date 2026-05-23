//! ZX Spectrum display: 256×192 pixel grid + (future) 32×24 attribute grid → RGBA.
//!
//! MVP-1 scope:
//!   * 256×192 mono pixel buffer
//!   * Single ink/paper colour pair (no per-cell attribute file yet)
//!   * 8×8 glyphs from [`crate::charset::FONT`]
//!   * A scrolling print area (rows 0..=21) with a `print` cursor that wraps
//!     on width and scrolls when it falls off the bottom of the area.
//!   * A dedicated single-line input area (row 23) that the System redraws
//!     on every keystroke via [`Display::print_input`].
//!
//! Attribute file, FLASH/BRIGHT, and graphics primitives (PLOT/DRAW/CIRCLE)
//! arrive in MVP-4.

use crate::charset::{FONT, FONT_FIRST_CHAR, FONT_GLYPH_COUNT};

/// Width of the Spectrum screen in pixels.
pub const PIXEL_W: usize = 256;
/// Height of the Spectrum screen in pixels.
pub const PIXEL_H: usize = 192;

/// Character cell width in pixels.
pub const CELL_W: usize = 8;
/// Character cell height in pixels.
pub const CELL_H: usize = 8;

/// Width of the screen in character cells.
pub const CHAR_W: usize = PIXEL_W / CELL_W; // 32
/// Height of the screen in character cells.
pub const CHAR_H: usize = PIXEL_H / CELL_H; // 24
/// Width of the attribute grid in cells (same as [`CHAR_W`] for now).
pub const ATTR_W: usize = CHAR_W;
/// Height of the attribute grid in cells.
pub const ATTR_H: usize = CHAR_H;

/// First row reserved for the input/status area (matches Spectrum's lower
/// screen, with one line allocated for now).
pub const INPUT_ROW: usize = 23;
/// Bottom row (inclusive) of the scrolling print area. Output that lands
/// below this triggers a one-row scroll-up of rows 0..=PRINT_BOTTOM.
pub const PRINT_BOTTOM: usize = 22;

/// Length of an RGBA frame buffer, in bytes. `PIXEL_W * PIXEL_H * 4`.
pub const FRAME_RGBA_LEN: usize = PIXEL_W * PIXEL_H * 4;

const STRIDE_BYTES: usize = PIXEL_W / 8; // 32
const DISPLAY_FILE_LEN: usize = STRIDE_BYTES * PIXEL_H;

/// 256×192 monochrome pixel framebuffer plus a print cursor.
pub struct Display {
    bits: [u8; DISPLAY_FILE_LEN],
    ink: Rgb,
    paper: Rgb,
    /// `(col, row)` for the print cursor. Always inside the print area.
    print_cursor: (usize, usize),
}

/// 24-bit RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Display {
    /// New display with the default Spectrum boot colours (black ink on white
    /// paper) and the print cursor at the top-left.
    pub fn new() -> Self {
        Self {
            bits: [0; DISPLAY_FILE_LEN],
            ink: Rgb(0, 0, 0),
            paper: Rgb(0xff, 0xff, 0xff),
            print_cursor: (0, 0),
        }
    }

    /// Clear all pixels to paper and reset the print cursor.
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.print_cursor = (0, 0);
    }

    /// Current print cursor `(col, row)`.
    pub fn print_cursor(&self) -> (usize, usize) {
        self.print_cursor
    }

    /// Draw an 8×8 glyph from [`FONT`] at character cell `(col, row)`,
    /// overwriting whatever was there.
    pub fn print_at(&mut self, col: usize, row: usize, ch: char) {
        if col >= CHAR_W || row >= CHAR_H {
            return;
        }
        let glyph = glyph_for(ch);
        let x = col * CELL_W;
        let y = row * CELL_H;
        for (dy, row_bits) in glyph.iter().enumerate() {
            let byte_idx = (y + dy) * STRIDE_BYTES + x / 8;
            self.bits[byte_idx] = *row_bits;
        }
    }

    /// Write a string starting at `(col, row)`. Drops characters that fall
    /// off the right edge.
    pub fn print_str(&mut self, col: usize, row: usize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            let c = col + i;
            if c >= CHAR_W {
                break;
            }
            self.print_at(c, row, ch);
        }
    }

    /// Clear a single character row to paper.
    pub fn clear_row(&mut self, row: usize) {
        if row >= CHAR_H {
            return;
        }
        let y0 = row * CELL_H;
        for dy in 0..CELL_H {
            let off = (y0 + dy) * STRIDE_BYTES;
            self.bits[off..off + STRIDE_BYTES].fill(0);
        }
    }

    /// Move the print cursor.
    pub fn set_print_cursor(&mut self, col: usize, row: usize) {
        self.print_cursor = (col.min(CHAR_W - 1), row.min(PRINT_BOTTOM));
    }

    /// Print a string at the current print cursor, advancing through the
    /// print area. Honours embedded `\n`, wraps at column [`CHAR_W`], and
    /// scrolls when it reaches the bottom.
    pub fn print(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.advance_newline();
            } else {
                let (col, row) = self.print_cursor;
                self.print_at(col, row, ch);
                self.print_cursor.0 += 1;
                if self.print_cursor.0 >= CHAR_W {
                    self.advance_newline();
                }
            }
        }
    }

    /// Print `s` followed by a newline.
    pub fn println(&mut self, s: &str) {
        self.print(s);
        self.advance_newline();
    }

    fn advance_newline(&mut self) {
        self.print_cursor.0 = 0;
        if self.print_cursor.1 < PRINT_BOTTOM {
            self.print_cursor.1 += 1;
        } else {
            self.scroll_print_area_up();
            // Cursor stays on the (now-blank) last row of the print area.
        }
    }

    /// Scroll the print area (rows 0..=PRINT_BOTTOM) up by one character row.
    /// The input row (23) is left untouched.
    fn scroll_print_area_up(&mut self) {
        let area_top_px = 0;
        let area_bot_px = (PRINT_BOTTOM + 1) * CELL_H;
        // Move pixel rows [CELL_H, area_bot_px) up by CELL_H rows.
        for py in area_top_px..(area_bot_px - CELL_H) {
            let src = (py + CELL_H) * STRIDE_BYTES;
            let dst = py * STRIDE_BYTES;
            // `copy_within` handles overlap safely.
            self.bits.copy_within(src..src + STRIDE_BYTES, dst);
        }
        // Clear the now-empty bottom row of the print area.
        let last_row_top_px = area_bot_px - CELL_H;
        for py in last_row_top_px..area_bot_px {
            let off = py * STRIDE_BYTES;
            self.bits[off..off + STRIDE_BYTES].fill(0);
        }
    }

    /// Redraw the single-line input area (row [`INPUT_ROW`]) with `text`
    /// followed by a block cursor at position `cursor_col` (i.e. just after
    /// the last character). Characters past column [`CHAR_W`] are dropped.
    pub fn print_input(&mut self, text: &str, cursor_col: usize) {
        self.clear_row(INPUT_ROW);
        for (i, ch) in text.chars().enumerate() {
            if i >= CHAR_W {
                break;
            }
            self.print_at(i, INPUT_ROW, ch);
        }
        if cursor_col < CHAR_W {
            // Inverse-video block as a crude cursor: draw `_` for now. The
            // real Spectrum K/L/C/E/G cursors land in MVP-2.
            self.print_at(cursor_col, INPUT_ROW, '_');
        }
    }

    /// Render the pixel buffer into an RGBA byte buffer of length
    /// [`FRAME_RGBA_LEN`]. Panics if the slice is the wrong size.
    pub fn render_into(&self, out: &mut [u8]) {
        assert_eq!(
            out.len(),
            FRAME_RGBA_LEN,
            "RGBA buffer must be {} bytes",
            FRAME_RGBA_LEN
        );
        let Rgb(ir, ig, ib) = self.ink;
        let Rgb(pr, pg, pb) = self.paper;
        for y in 0..PIXEL_H {
            for byte_x in 0..STRIDE_BYTES {
                let byte = self.bits[y * STRIDE_BYTES + byte_x];
                for bit in 0..8 {
                    let lit = (byte >> (7 - bit)) & 1 == 1;
                    let px = byte_x * 8 + bit;
                    let i = (y * PIXEL_W + px) * 4;
                    if lit {
                        out[i] = ir;
                        out[i + 1] = ig;
                        out[i + 2] = ib;
                    } else {
                        out[i] = pr;
                        out[i + 1] = pg;
                        out[i + 2] = pb;
                    }
                    out[i + 3] = 0xff;
                }
            }
        }
    }
}

impl Default for Display {
    fn default() -> Self {
        Self::new()
    }
}

fn glyph_for(ch: char) -> &'static [u8; 8] {
    let cp = ch as u32;
    let first = FONT_FIRST_CHAR as u32;
    let last = first + FONT_GLYPH_COUNT as u32 - 1;
    if cp >= first && cp <= last {
        &FONT[(cp - first) as usize]
    } else {
        // Space.
        &FONT[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_advances_cursor() {
        let mut d = Display::new();
        d.print("HI");
        assert_eq!(d.print_cursor(), (2, 0));
    }

    #[test]
    fn print_wraps_on_width() {
        let mut d = Display::new();
        d.print(&"X".repeat(CHAR_W));
        // After exactly CHAR_W chars the wrap moves us to col 0, row 1.
        assert_eq!(d.print_cursor(), (0, 1));
    }

    #[test]
    fn print_scrolls_at_bottom() {
        let mut d = Display::new();
        d.set_print_cursor(0, PRINT_BOTTOM);
        d.print("Z\n");
        // Wrote on the last row, then newline → scroll. Cursor stays on row.
        assert_eq!(d.print_cursor(), (0, PRINT_BOTTOM));
    }
}
