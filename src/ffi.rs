//! FFI bindings for `SQLite` session extension.
//!
//! This module re-exports the appropriate FFI bindings based on the target platform:
//! - Native targets: `libsqlite3-sys`
//! - WASM targets: `sqlite-wasm-rs`

#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub use libsqlite3_sys::*;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use sqlite_wasm_rs::*;

/// The bytes of a non-NULL `value`, empty for a zero-length one, or `None` when
/// `SQLite` reports a length that does not fit `usize` or no buffer.
///
/// # Safety
///
/// `value` must point to a live, non-NULL `sqlite3_value` that nothing changes
/// or frees for `'a`, so the buffer `SQLite` hands back stays valid and
/// unmodified for the returned borrow.
pub(crate) unsafe fn value_bytes<'a>(value: *mut sqlite3_value) -> Option<&'a [u8]> {
    // SAFETY: the caller guarantees `value` points to a live `sqlite3_value`.
    let len = unsafe { sqlite3_value_bytes(value) };
    let Ok(len) = usize::try_from(len) else {
        return None;
    };
    if len == 0 {
        return Some(&[]);
    }
    // SAFETY: the caller guarantees `value` is live and non-NULL, which is what
    // `sqlite3_value_blob` requires, and the length was read just above.
    let ptr = unsafe { sqlite3_value_blob(value) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `ptr` is non-null (checked above) and points to the `len` bytes
    // `SQLite` reported for this value. `u8` has alignment 1. The caller keeps the
    // value alive and unmodified for `'a`, so the slice stays dereferenceable and
    // no `&mut` to the buffer exists while it is borrowed.
    Some(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) })
}
