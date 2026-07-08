//! Shared trampolines for the streamed variants of the session extension.
//!
//! Each streamed `SQLite` reader takes an `xInput` callback. The generic
//! trampoline here bridges that C signature to [`std::io::Read`]: every
//! streamed reader packs the user's `R` into an [`InputContext`] and hands
//! `SQLite` a pointer to that context. Panics inside the user's stream are
//! caught and mapped to `SQLITE_IOERR` so unwinding never crosses the FFI
//! boundary.

use std::ffi::{c_int, c_void};
use std::io::Read;
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::ffi::{SQLITE_IOERR, SQLITE_OK};

/// User's reader plus sticky error / panic slots for the input trampoline.
pub(crate) struct InputContext<R: Read> {
    pub(crate) reader: R,
    /// Sticky `io::Error` from the user's `Read`. `Some` means the trampoline
    /// aborted at least one call; the outer function surfaces the error.
    pub(crate) error: Option<std::io::Error>,
    /// Set when the user's `Read` panicked and `catch_unwind` caught it.
    pub(crate) panicked: bool,
}

impl<R: Read> InputContext<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            error: None,
            panicked: false,
        }
    }
}

/// C trampoline for `xInput`.
///
/// # Safety
///
/// `ctx` must point to a live [`InputContext<R>`]. `data` must be writable
/// for `*p_n_data` bytes for the duration of the call.
pub(crate) unsafe extern "C" fn read_trampoline<R>(
    ctx: *mut c_void,
    data: *mut c_void,
    p_n_data: *mut c_int,
) -> c_int
where
    R: Read,
{
    // SAFETY: `ctx` matches the type we installed.
    let ctx = unsafe { &mut *(ctx.cast::<InputContext<R>>()) };
    if ctx.error.is_some() || ctx.panicked {
        // Once we've reported an error we must not touch the reader. SQLite
        // may still call us; return 0 bytes so the session treats input as
        // exhausted.
        // SAFETY: `p_n_data` is a valid `int*` per the FFI contract.
        unsafe { *p_n_data = 0 };
        return SQLITE_IOERR;
    }
    // SAFETY: `p_n_data` is a valid `int*` per the FFI contract.
    let want = unsafe { *p_n_data };
    let want = usize::try_from(want).unwrap_or(0);
    if want == 0 {
        return SQLITE_OK;
    }
    // SAFETY: SQLite promises `data` is writable for `want` bytes.
    let buf = unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), want) };
    match catch_unwind(AssertUnwindSafe(|| ctx.reader.read(buf))) {
        Ok(Ok(n)) => {
            let n_c = c_int::try_from(n).unwrap_or(0);
            // SAFETY: `p_n_data` is a valid `int*` per the FFI contract.
            unsafe { *p_n_data = n_c };
            SQLITE_OK
        }
        Ok(Err(e)) => {
            ctx.error = Some(e);
            // SAFETY: `p_n_data` is a valid `int*` per the FFI contract.
            unsafe { *p_n_data = 0 };
            SQLITE_IOERR
        }
        Err(_) => {
            ctx.panicked = true;
            // SAFETY: `p_n_data` is a valid `int*` per the FFI contract.
            unsafe { *p_n_data = 0 };
            SQLITE_IOERR
        }
    }
}
