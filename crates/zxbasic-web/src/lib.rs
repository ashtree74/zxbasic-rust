//! wasm-bindgen façade over [`zxbasic_core::System`].

use wasm_bindgen::prelude::*;
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

    /// RGBA frame dimensions for the caller to size the canvas / ImageData.
    #[wasm_bindgen(getter)]
    pub fn pixel_w(&self) -> usize {
        zxbasic_core::PIXEL_W
    }

    #[wasm_bindgen(getter)]
    pub fn pixel_h(&self) -> usize {
        zxbasic_core::PIXEL_H
    }

    /// Advance one frame and return a borrow of the freshly rendered RGBA
    /// buffer. JS side wraps this with `new ImageData(new Uint8ClampedArray(…),
    /// width, height)`.
    pub fn render(&mut self) -> Box<[u8]> {
        self.inner.frame();
        self.inner.render_into(&mut self.frame_buf);
        self.frame_buf.clone().into_boxed_slice()
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}
