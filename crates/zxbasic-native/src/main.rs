//! MVP-0: native dev harness is a stub — we only need the WASM build for now.
//! Real winit/softbuffer wiring lands in MVP-1.

fn main() {
    let sys = zxbasic_core::System::new();
    let mut buf = vec![0u8; zxbasic_core::FRAME_RGBA_LEN];
    sys.render_into(&mut buf);
    println!(
        "zxbasic-native (MVP-0 stub): rendered {} bytes of RGBA from a {}×{} screen",
        buf.len(),
        zxbasic_core::PIXEL_W,
        zxbasic_core::PIXEL_H
    );
}
