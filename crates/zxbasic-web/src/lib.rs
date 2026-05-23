//! wasm-bindgen façade over [`zxbasic_core::System`].

use wasm_bindgen::prelude::*;
use zxbasic_core::system::Key;
use zxbasic_core::System as CoreSystem;

#[wasm_bindgen]
pub struct System {
    inner: CoreSystem,
    frame_buf: Vec<u8>,
}

#[wasm_bindgen]
impl System {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: CoreSystem::new(),
            frame_buf: vec![0; CoreSystem::FRAME_RGBA_LEN],
        }
    }

    /// Pixel width of the RGBA buffer returned by [`Self::render`].
    #[wasm_bindgen(getter)]
    pub fn pixel_w(&self) -> usize {
        zxbasic_core::PIXEL_W
    }

    /// Pixel height of the RGBA buffer returned by [`Self::render`].
    #[wasm_bindgen(getter)]
    pub fn pixel_h(&self) -> usize {
        zxbasic_core::PIXEL_H
    }

    /// Advance one frame and return the freshly rendered RGBA buffer.
    pub fn render(&mut self) -> Box<[u8]> {
        self.inner.frame();
        self.inner.render_into(&mut self.frame_buf);
        self.frame_buf.clone().into_boxed_slice()
    }

    /// Feed a printable ASCII byte (32..=126) into the input line. Other
    /// values are ignored.
    pub fn feed_char(&mut self, b: u8) {
        self.inner.feed_key(Key::Char(b));
    }

    /// Feed an Enter / Return key.
    pub fn feed_enter(&mut self) {
        self.inner.feed_key(Key::Enter);
    }

    /// Feed a Backspace key.
    pub fn feed_backspace(&mut self) {
        self.inner.feed_key(Key::Backspace);
    }

    /// Spectrum BREAK (Caps Shift + Space). Hosts that have no chord
    /// support typically wire it to Esc.
    pub fn feed_break(&mut self) {
        self.inner.feed_key(Key::Break);
    }

    /// Modern-terminal command recall — Up walks back through previously
    /// entered immediate-mode lines, Down walks forward toward the live
    /// draft. No-op while a program is running.
    pub fn feed_history_prev(&mut self) {
        self.inner.feed_key(Key::HistoryPrev);
    }

    pub fn feed_history_next(&mut self) {
        self.inner.feed_key(Key::HistoryNext);
    }

    /// Current screen-border colour as a packed RGB integer (0xRRGGBB).
    /// JS reads this every frame to keep the border around the canvas in sync.
    pub fn border_rgb_packed(&self) -> u32 {
        let [r, g, b] = self.inner.border_rgb();
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }

    /// Drain queued `BEEP duration, pitch` requests. Returns a flat array of
    /// `[duration_sec_0, freq_hz_0, duration_sec_1, freq_hz_1, …]`. JS
    /// schedules them sequentially through the WebAudio context.
    pub fn take_beeps(&mut self) -> Box<[f32]> {
        let beeps = self.inner.drain_beeps();
        let mut out = Vec::with_capacity(beeps.len() * 2);
        for (d, f) in beeps {
            out.push(d);
            out.push(f);
        }
        out.into_boxed_slice()
    }

    /// `true` if the runtime asked the host to stop currently playing
    /// audio (typically after BREAK during a BEEP sequence). Single-shot —
    /// reading it clears the flag.
    pub fn take_audio_cancel(&mut self) -> bool {
        self.inner.take_audio_cancel()
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}
