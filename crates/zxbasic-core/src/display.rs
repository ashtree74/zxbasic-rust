//! ZX Spectrum display: 256×192 pixel grid + 32×24 attribute grid → RGBA.
//!
//! MVP-0 scope:
//!   * 256×192 mono pixel buffer
//!   * Single ink/paper colour pair (no per-cell attribute file yet)
//!   * `print_at` writes 8×8 glyphs from [`crate::charset::FONT`] into the
//!     pixel buffer at character coordinates (col, row).
//!   * `to_rgba` flattens the pixel buffer into a Canvas-compatible RGBA byte
//!     buffer.
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

/// Length of an RGBA frame buffer, in bytes. `PIXEL_W * PIXEL_H * 4`.
pub const FRAME_RGBA_LEN: usize = PIXEL_W * PIXEL_H * 4;

const DISPLAY_FILE_LEN: usize = PIXEL_W * PIXEL_H / 8;

/// 256×192 monochrome pixel framebuffer.
///
/// One bit per pixel, MSB = leftmost pixel of its byte. Row-major: byte 0 is
/// the top-left 8 pixels. For MVP-0 we use a simple linear layout — the real
/// Spectrum display file has the iconic interleaved Y-coordinate layout, which
/// we will replicate when `PEEK`/`POKE` of the display file matters (MVP-5).
pub struct Display {
    bits: [u8; DISPLAY_FILE_LEN],
    ink: Rgb,
    paper: Rgb,
}

/// 24-bit RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Display {
    /// New display with the default Spectrum boot colours (black ink on white
    /// paper, the screen you see before `MODE` clears).
    pub fn new() -> Self {
        Self {
            bits: [0; DISPLAY_FILE_LEN],
            ink: Rgb(0, 0, 0),
            paper: Rgb(0xff, 0xff, 0xff),
        }
    }

    /// Clear all pixels to paper.
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }

    /// Draw an 8×8 glyph from [`FONT`] at character cell `(col, row)`.
    ///
    /// Characters outside the supported codepoint range are silently rendered
    /// as a space.
    pub fn print_at(&mut self, col: usize, row: usize, ch: char) {
        if col >= CHAR_W || row >= CHAR_H {
            return;
        }
        let glyph = glyph_for(ch);
        let x = col * CELL_W;
        let y = row * CELL_H;
        for (dy, row_bits) in glyph.iter().enumerate() {
            let py = y + dy;
            let byte_idx = (py * PIXEL_W + x) / 8;
            // x is a multiple of 8, so writes align to a single byte.
            self.bits[byte_idx] = *row_bits;
        }
    }

    /// Write a string starting at character cell `(col, row)`, advancing along
    /// the row. Characters that fall off the right edge are dropped — there is
    /// no scrolling yet (MVP-1 will add a proper print cursor with scroll).
    pub fn print_str(&mut self, col: usize, row: usize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            let c = col + i;
            if c >= CHAR_W {
                break;
            }
            self.print_at(c, row, ch);
        }
    }

    /// Render the pixel buffer into an RGBA byte buffer of length
    /// [`FRAME_RGBA_LEN`]. Panics if the slice is the wrong size.
    pub fn render_into(&self, out: &mut [u8]) {
        assert_eq!(out.len(), FRAME_RGBA_LEN, "RGBA buffer must be {} bytes", FRAME_RGBA_LEN);
        let Rgb(ir, ig, ib) = self.ink;
        let Rgb(pr, pg, pb) = self.paper;
        for y in 0..PIXEL_H {
            for byte_x in 0..(PIXEL_W / 8) {
                let byte = self.bits[y * (PIXEL_W / 8) + byte_x];
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
