//! ZX Spectrum BASIC runtime, platform-agnostic core.
//!
//! Public surface (MVP-0): font data only. `System` and the rest come in MVP-1+.

#![forbid(unsafe_code)]

pub mod charset;
pub mod display;
pub mod expression;
pub mod fp_format;
pub mod system;

pub use display::{ATTR_H, FRAME_RGBA_LEN, PIXEL_H, PIXEL_W};
pub use system::System;
