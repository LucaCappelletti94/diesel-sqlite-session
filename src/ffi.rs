//! FFI bindings for `SQLite` session extension.
//!
//! This module re-exports the appropriate FFI bindings based on the target platform:
//! - Native targets: `libsqlite3-sys`
//! - WASM targets: `sqlite-wasm-rs`

#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub use libsqlite3_sys::*;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use sqlite_wasm_rs::*;
