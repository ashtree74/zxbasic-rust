//! ZX Spectrum display: 256×192 pixel grid + 32×24 attribute grid → RGBA.
//!
//! Spectrum colour model:
//!   * Each character cell (8×8 pixels) has one attribute byte.
//!   * Bits 0..2 = INK (foreground colour, 0..7).
//!   * Bits 3..5 = PAPER (background colour, 0..7).
//!   * Bit 6 = BRIGHT (boost both ink and paper to the bright palette).
//!   * Bit 7 = FLASH (swap ink ↔ paper at ~50/32 Hz).
//!
//! Pixel `(x, y)` is foreground iff its bit in the display file is set; the
//! resulting RGB colour is read from the attribute byte for cell
//! `(x/8, y/8)`. This is the famous Spectrum "attribute clash" — only two
//! colours per cell.

use crate::charset::{FONT, FONT_FIRST_CHAR, FONT_GLYPH_COUNT};

pub const PIXEL_W: usize = 256;
pub const PIXEL_H: usize = 192;

pub const CELL_W: usize = 8;
pub const CELL_H: usize = 8;

pub const CHAR_W: usize = PIXEL_W / CELL_W; // 32
pub const CHAR_H: usize = PIXEL_H / CELL_H; // 24
pub const ATTR_W: usize = CHAR_W;
pub const ATTR_H: usize = CHAR_H;

/// The Spectrum's lower screen sits on row 22 — the same physical line
/// holds either a status report (boot copyright, `0 OK, X:Y` etc.) or the
/// active editor line with a K-mode cursor; never both at the same time.
pub const INPUT_ROW: usize = 22;
/// Last row of the scrolling upper screen (PRINT output).
pub const PRINT_BOTTOM: usize = 21;

pub const FRAME_RGBA_LEN: usize = PIXEL_W * PIXEL_H * 4;

const STRIDE_BYTES: usize = PIXEL_W / 8; // 32
const DISPLAY_FILE_LEN: usize = STRIDE_BYTES * PIXEL_H;
const ATTR_FILE_LEN: usize = ATTR_W * ATTR_H;

/// FLASH period in calls to [`Display::frame_advance`]. Spectrum flashes at
/// 32 frames (≈0.64s) per phase at 50Hz, so this matches if `frame_advance`
/// is called once per rendered frame at roughly that rate.
const FLASH_PERIOD_FRAMES: u32 = 16;

/// 16-entry RGB palette: 0..7 are the normal Spectrum colours, 8..15 the
/// BRIGHT variants. Standard pseudo-Spectrum values (CDh / FFh per channel).
const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0 black
    [0x00, 0x00, 0xcd], // 1 blue
    [0xcd, 0x00, 0x00], // 2 red
    [0xcd, 0x00, 0xcd], // 3 magenta
    [0x00, 0xcd, 0x00], // 4 green
    [0x00, 0xcd, 0xcd], // 5 cyan
    [0xcd, 0xcd, 0x00], // 6 yellow
    [0xcd, 0xcd, 0xcd], // 7 white
    [0x00, 0x00, 0x00], // 0 black (bright = black)
    [0x00, 0x00, 0xff], // 1 blue bright
    [0xff, 0x00, 0x00], // 2 red bright
    [0xff, 0x00, 0xff], // 3 magenta bright
    [0x00, 0xff, 0x00], // 4 green bright
    [0x00, 0xff, 0xff], // 5 cyan bright
    [0xff, 0xff, 0x00], // 6 yellow bright
    [0xff, 0xff, 0xff], // 7 white bright
];

/// Default attribute byte: black ink on white paper, no bright, no flash.
const DEFAULT_ATTR: u8 = 0 | (7 << 3);

pub struct Display {
    bits: [u8; DISPLAY_FILE_LEN],
    attrs: [u8; ATTR_FILE_LEN],
    print_cursor: (usize, usize),
    /// Counts up by 1 per [`Self::frame_advance`] call; bit 4 toggles ink ↔ paper for FLASH cells.
    flash_counter: u32,
}

impl Display {
    pub fn new() -> Self {
        Self {
            bits: [0; DISPLAY_FILE_LEN],
            attrs: [DEFAULT_ATTR; ATTR_FILE_LEN],
            print_cursor: (0, 0),
            flash_counter: 0,
        }
    }

    /// Clear pixels and reset attributes to `default_attr` (ink/paper used
    /// by the next things you draw or print). Print cursor moves to (0, 0).
    pub fn cls(&mut self, default_attr: u8) {
        self.bits.fill(0);
        self.attrs.fill(default_attr);
        self.print_cursor = (0, 0);
    }

    /// MVP-0-style clear: reset to default attrs (black on white). Kept for
    /// backwards-compat with the existing `CLS` command.
    pub fn clear(&mut self) {
        self.cls(DEFAULT_ATTR);
    }

    pub fn print_cursor(&self) -> (usize, usize) {
        self.print_cursor
    }

    pub fn set_print_cursor(&mut self, col: usize, row: usize) {
        self.print_cursor = (col.min(CHAR_W - 1), row.min(PRINT_BOTTOM));
    }

    /// Draw an 8×8 glyph at character cell `(col, row)`, replacing whatever
    /// was there. Also stamps the supplied attribute byte into that cell.
    pub fn print_at(&mut self, col: usize, row: usize, ch: char, attr: u8) {
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
        self.attrs[row * ATTR_W + col] = attr;
    }

    /// Write a string at a specific character cell using the default
    /// attribute. Convenience for static labels (e.g. the boot screen).
    pub fn print_str(&mut self, col: usize, row: usize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            let c = col + i;
            if c >= CHAR_W {
                break;
            }
            self.print_at(c, row, ch, DEFAULT_ATTR);
        }
    }

    pub fn clear_row(&mut self, row: usize, attr: u8) {
        if row >= CHAR_H {
            return;
        }
        let y0 = row * CELL_H;
        for dy in 0..CELL_H {
            let off = (y0 + dy) * STRIDE_BYTES;
            self.bits[off..off + STRIDE_BYTES].fill(0);
        }
        for c in 0..ATTR_W {
            self.attrs[row * ATTR_W + c] = attr;
        }
    }

    /// Print a string at the current cursor with the given attribute,
    /// wrapping and scrolling within the print area.
    pub fn print_with_attr(&mut self, s: &str, attr: u8) {
        for ch in s.chars() {
            if ch == '\n' {
                self.advance_newline(attr);
            } else {
                let (col, row) = self.print_cursor;
                self.print_at(col, row, ch, attr);
                self.print_cursor.0 += 1;
                if self.print_cursor.0 >= CHAR_W {
                    self.advance_newline(attr);
                }
            }
        }
    }

    /// Print a string + newline using the default attribute (kept for
    /// callers that don't care about colour, e.g. system messages).
    pub fn println(&mut self, s: &str) {
        self.print_with_attr(s, DEFAULT_ATTR);
        self.advance_newline(DEFAULT_ATTR);
    }

    fn advance_newline(&mut self, attr: u8) {
        self.print_cursor.0 = 0;
        if self.print_cursor.1 < PRINT_BOTTOM {
            self.print_cursor.1 += 1;
        } else {
            self.scroll_print_area_up(attr);
        }
    }

    fn scroll_print_area_up(&mut self, blank_attr: u8) {
        let area_bot_px = (PRINT_BOTTOM + 1) * CELL_H;
        for py in 0..(area_bot_px - CELL_H) {
            let src = (py + CELL_H) * STRIDE_BYTES;
            let dst = py * STRIDE_BYTES;
            self.bits.copy_within(src..src + STRIDE_BYTES, dst);
        }
        let last_row_top_px = area_bot_px - CELL_H;
        for py in last_row_top_px..area_bot_px {
            let off = py * STRIDE_BYTES;
            self.bits[off..off + STRIDE_BYTES].fill(0);
        }
        // Scroll the attribute file too (rows 0..=PRINT_BOTTOM-1 ← rows 1..=PRINT_BOTTOM).
        for r in 0..PRINT_BOTTOM {
            let src = (r + 1) * ATTR_W;
            let dst = r * ATTR_W;
            self.attrs.copy_within(src..src + ATTR_W, dst);
        }
        for c in 0..ATTR_W {
            self.attrs[PRINT_BOTTOM * ATTR_W + c] = blank_attr;
        }
    }

    /// Redraw the single lower-screen row. The caller chooses whether the
    /// line shows a status message (boot copyright, `0 OK, …` report) or
    /// the active editor input with a K-mode cursor — both options share
    /// the same physical row, never coexisting.
    pub fn print_input(&mut self, text: &str, cursor_col: Option<usize>) {
        self.clear_row(INPUT_ROW, DEFAULT_ATTR);
        for (i, ch) in text.chars().enumerate() {
            if i >= CHAR_W {
                break;
            }
            self.print_at(i, INPUT_ROW, ch, DEFAULT_ATTR);
        }
        if let Some(col) = cursor_col {
            if col < CHAR_W {
                let cursor_attr = make_attr(7, 0, false, true);
                self.print_at(col, INPUT_ROW, 'K', cursor_attr);
            }
        }
    }

    /// Set or clear a single pixel `(x, y)` and stamp `cell_attr` into the
    /// cell that pixel belongs to. `(0, 0)` is the *bottom-left* corner —
    /// Spectrum's PLOT coordinate convention — so we flip y internally.
    pub fn plot(&mut self, x: i32, y: i32, set: bool, cell_attr: u8) {
        if x < 0 || y < 0 || x >= PIXEL_W as i32 || y >= PIXEL_H as i32 {
            return;
        }
        let x = x as usize;
        let py = (PIXEL_H - 1) - y as usize; // flip Y
        let byte_idx = py * STRIDE_BYTES + x / 8;
        let bit = 7 - (x % 8) as u8;
        if set {
            self.bits[byte_idx] |= 1 << bit;
        } else {
            self.bits[byte_idx] &= !(1 << bit);
        }
        let cell = (py / CELL_H) * ATTR_W + x / CELL_W;
        self.attrs[cell] = cell_attr;
    }

    /// Bresenham line from `(x0, y0)` to `(x1, y1)` (Spectrum convention,
    /// y-up).
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, attr: u8) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = x0;
        let mut y = y0;
        loop {
            self.plot(x, y, true, attr);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Midpoint circle at `(cx, cy)` (Spectrum convention, y-up) with
    /// integer radius `r`.
    pub fn draw_circle(&mut self, cx: i32, cy: i32, r: i32, attr: u8) {
        if r <= 0 {
            self.plot(cx, cy, true, attr);
            return;
        }
        let mut x = r;
        let mut y = 0;
        let mut err = 0;
        while x >= y {
            for &(dx, dy) in &[
                (x, y),
                (y, x),
                (-y, x),
                (-x, y),
                (-x, -y),
                (-y, -x),
                (y, -x),
                (x, -y),
            ] {
                self.plot(cx + dx, cy + dy, true, attr);
            }
            y += 1;
            err += 1 + 2 * y;
            if 2 * (err - x) + 1 > 0 {
                x -= 1;
                err += 1 - 2 * x;
            }
        }
    }

    /// Advance the FLASH animation by one frame.
    pub fn frame_advance(&mut self) {
        self.flash_counter = self.flash_counter.wrapping_add(1);
    }

    fn flash_phase(&self) -> bool {
        (self.flash_counter / FLASH_PERIOD_FRAMES) % 2 == 1
    }

    pub fn render_into(&self, out: &mut [u8]) {
        assert_eq!(out.len(), FRAME_RGBA_LEN);
        let phase = self.flash_phase();
        for y in 0..PIXEL_H {
            for byte_x in 0..STRIDE_BYTES {
                let byte = self.bits[y * STRIDE_BYTES + byte_x];
                let cell = (y / CELL_H) * ATTR_W + byte_x;
                let attr = self.attrs[cell];
                let mut ink = attr & 0x07;
                let mut paper = (attr >> 3) & 0x07;
                let bright = (attr & 0x40) != 0;
                let flash = (attr & 0x80) != 0;
                if flash && phase {
                    core::mem::swap(&mut ink, &mut paper);
                }
                let ink_rgb = PALETTE[ink as usize + if bright { 8 } else { 0 }];
                let paper_rgb = PALETTE[paper as usize + if bright { 8 } else { 0 }];
                for bit in 0..8 {
                    let lit = (byte >> (7 - bit)) & 1 == 1;
                    let px = byte_x * 8 + bit;
                    let i = (y * PIXEL_W + px) * 4;
                    let rgb = if lit { ink_rgb } else { paper_rgb };
                    out[i] = rgb[0];
                    out[i + 1] = rgb[1];
                    out[i + 2] = rgb[2];
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
    // Spectrum charset puts the copyright sign at code 0x7F (last printable
    // glyph of the standard set). Unicode `©` is U+00A9; remap it.
    let cp: u32 = if ch == '\u{00A9}' { 0x7F } else { ch as u32 };
    let first = FONT_FIRST_CHAR as u32;
    let last = first + FONT_GLYPH_COUNT as u32 - 1;
    if cp >= first && cp <= last {
        &FONT[(cp - first) as usize]
    } else {
        &FONT[0]
    }
}

/// RGB lookup for a Spectrum colour 0..7 with optional BRIGHT.
pub fn spectrum_palette(colour: u8, bright: bool) -> [u8; 3] {
    let idx = (colour & 7) as usize + if bright { 8 } else { 0 };
    PALETTE[idx]
}

/// Build a Spectrum-style attribute byte from its parts.
pub fn make_attr(ink: u8, paper: u8, bright: bool, flash: bool) -> u8 {
    (ink & 7)
        | ((paper & 7) << 3)
        | (if bright { 0x40 } else { 0 })
        | (if flash { 0x80 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_advances_cursor() {
        let mut d = Display::new();
        d.print_with_attr("HI", DEFAULT_ATTR);
        assert_eq!(d.print_cursor(), (2, 0));
    }

    #[test]
    fn plot_sets_bit_and_attr() {
        let mut d = Display::new();
        let attr = make_attr(2, 7, false, false);
        d.plot(0, 0, true, attr);
        // (0, 0) in Spectrum coords is the bottom-left pixel.
        let py = PIXEL_H - 1;
        let idx = py * STRIDE_BYTES;
        assert_eq!(d.bits[idx] & 0x80, 0x80, "bit not set");
        let cell = (py / CELL_H) * ATTR_W;
        assert_eq!(d.attrs[cell], attr);
    }

    #[test]
    fn draw_line_horizontal() {
        let mut d = Display::new();
        d.draw_line(0, 0, 31, 0, DEFAULT_ATTR);
        // 32 pixels set on the bottom row.
        let py = PIXEL_H - 1;
        let bytes_set = (0..4)
            .map(|i| d.bits[py * STRIDE_BYTES + i])
            .filter(|&b| b == 0xff)
            .count();
        assert_eq!(bytes_set, 4);
    }
}
