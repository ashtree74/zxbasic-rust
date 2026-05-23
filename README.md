# zxbasic-rust

A native ZX Spectrum BASIC runtime in Rust, targeting WebAssembly for the browser.

This is **not** a Z80 emulator — it's a fresh reimplementation of the Spectrum
BASIC interpreter, line editor, calculator, and display model in idiomatic Rust.
Games and other machine-code software written for Spectrum will **not** run
here. What *will* run is anything written in Spectrum BASIC.

## Project layout

```
crates/
  zxbasic-core/     # platform-agnostic interpreter, display, editor, keyboard
  zxbasic-web/      # wasm-bindgen frontend (canvas + key events)
  zxbasic-native/   # winit + softbuffer desktop dev harness
tools/              # one-shot data-extraction binaries (font, tokens, etc.)
web/                # static page + JS loader for the wasm bundle
vendor/zxrom/       # original Sinclair ROM Z80 source as submodule
                    # (https://github.com/cheveron/zxrom.git) — used only as
                    # specification & source of data tables (font, token list,
                    # error messages). Not compiled or linked.
```

## Status

Pre-MVP-0. See `../plans/` in the parent for the design plan.

## Build

```
cargo build                       # native check
cd crates/zxbasic-web
wasm-pack build --target web      # WASM bundle → crates/zxbasic-web/pkg/
```

Serve `web/` over HTTP and open `index.html`.

## License

Source: dual MIT / Apache-2.0. Data tables extracted from `vendor/zxrom`
(CC BY-SA 4.0) are reproduced under the same CC BY-SA terms; see
`vendor/zxrom/Readme.md`.
