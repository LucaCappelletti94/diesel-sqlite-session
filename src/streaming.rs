//! Shared trampolines for the streamed variants of the session extension.
//!
//! Each streamed `SQLite` function takes an `xInput` / `xOutput` callback
//! pair. The generic trampolines here bridge those C signatures to
//! [`std::io::Read`] / [`std::io::Write`]: every streamed method packs the
//! user's `R` or `W` into an [`InputContext`] / [`OutputContext`] and hands
//! `SQLite` a pointer to that context. Panics inside the user's stream are
//! caught and mapped to `SQLITE_IOERR` so unwinding never crosses the FFI
//! boundary.

use std::ffi::{c_int, c_void};
use std::io::{Read, Write};
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

/// User's writer plus sticky error / panic slots (mirrors [`InputContext`]).
pub(crate) struct OutputContext<W: Write> {
    pub(crate) writer: W,
    pub(crate) error: Option<std::io::Error>,
    pub(crate) panicked: bool,
}

impl<W: Write> OutputContext<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
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

/// C trampoline for `xOutput`.
///
/// # Safety
///
/// `ctx` must point to a live [`OutputContext<W>`]. `data` must be readable
/// for `n_data` bytes for the duration of the call.
pub(crate) unsafe extern "C" fn write_trampoline<W>(
    ctx: *mut c_void,
    data: *const c_void,
    n_data: c_int,
) -> c_int
where
    W: Write,
{
    // SAFETY: `ctx` matches the type we installed.
    let ctx = unsafe { &mut *(ctx.cast::<OutputContext<W>>()) };
    if ctx.error.is_some() || ctx.panicked {
        return SQLITE_IOERR;
    }
    let n = usize::try_from(n_data).unwrap_or(0);
    if n == 0 {
        return SQLITE_OK;
    }
    // SAFETY: SQLite promises `data` is readable for `n` bytes.
    let buf = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), n) };
    match catch_unwind(AssertUnwindSafe(|| ctx.writer.write_all(buf))) {
        Ok(Ok(())) => SQLITE_OK,
        Ok(Err(e)) => {
            ctx.error = Some(e);
            SQLITE_IOERR
        }
        Err(_) => {
            ctx.panicked = true;
            SQLITE_IOERR
        }
    }
}
