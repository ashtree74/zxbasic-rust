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
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}
