//! The bindings generator, as a binary of this crate.
//!
//! uniffi's own `uniffi-bindgen` has to be the same version as the `uniffi` the library was
//! built with, so it is built here rather than installed:
//!
//! ```sh
//! cargo run -q -p junk-ffi --bin uniffi-bindgen -- generate \
//!     --library target/aarch64-apple-ios-sim/debug/libjunk_ffi.a \
//!     --language swift --out-dir bindings
//! ```
//!
//! `--library` reads the interface out of the built staticlib itself, which is what makes
//! proc-macro-only bindings work with no UDL and no `build.rs`.
#![warn(missing_docs)]

fn main() {
    uniffi::uniffi_bindgen_main();
}
