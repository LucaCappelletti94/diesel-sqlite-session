//! Incremental BLOB I/O wrappers for `SQLite`.
//!
//! Wraps the `sqlite3_blob_*` family so callers can stream bytes in and out
//! of a fixed-size blob column without materializing a whole `Vec<u8>`.
//! Diesel already ships a read-only handle; this crate adds the write side
//! because writes are what raise the pre-update hook (see
//! [`crate::PreUpdateEvent::blob_write_column`]).
//!
//! [`SqliteBlob`] is `!Send + !Sync` and RAII with the same "drop before the
//! connection" contract as [`crate::Session`] and [`crate::PreUpdateHook`].

use std::ffi::{c_int, CString};
use std::marker::PhantomData;
use std::ptr;

use diesel::SqliteConnection;
use thiserror::Error;

use crate::errors::SqliteErrorCode;
use crate::ffi::{
    sqlite3_blob, sqlite3_blob_bytes, sqlite3_blob_close, sqlite3_blob_open, sqlite3_blob_read,
    sqlite3_blob_reopen, sqlite3_blob_write, SQLITE_OK,
};

/// Access mode for a blob handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlobMode {
    /// Read-only handle (`flags = 0`). Write attempts return
    /// [`BlobError::ReadOnly`] without touching `SQLite`.
    ReadOnly,
    /// Read plus write handle (`flags = 1`). Each successful write fires the
    /// pre-update hook once with [`crate::PreUpdateOp::Delete`] and
    /// [`crate::PreUpdateEvent::blob_write_column`] set to the column index
    /// this handle was opened on.
    ReadWrite,
}

impl BlobMode {
    const fn to_flags(self) -> c_int {
        match self {
            Self::ReadOnly => 0,
            Self::ReadWrite => 1,
        }
    }
}

/// Errors raised by the blob wrappers.
#[derive(Debug, Error)]
pub enum BlobError {
    /// One of `database`, `table`, or `column` contained an interior null byte
    /// and could not be converted to a C string.
    #[error("blob location string contains a null byte")]
    InvalidName,
    /// A read or write would extend past the end of the blob.
    #[error(
        "blob range out of bounds: offset {offset} + buf {buf_len} exceeds blob length {blob_len}"
    )]
    OffsetOutOfRange {
        /// Requested starting offset.
        offset: usize,
        /// Length of the user buffer.
        buf_len: usize,
        /// Actual blob size reported by `sqlite3_blob_bytes`.
        blob_len: usize,
    },
    /// The user buffer, offset, or blob size does not fit into an `i32` and
    /// so cannot be passed through the `SQLite` C API.
    #[error("blob length {value} does not fit into a 32-bit signed integer")]
    LengthOverflow {
        /// The offending count that failed the `c_int::try_from` conversion.
        value: usize,
    },
    /// A write was attempted through a handle opened with
    /// [`BlobMode::ReadOnly`].
    #[error("cannot write through a read-only blob handle")]
    ReadOnly,
    /// `sqlite3_blob_open` returned a non-`OK` code.
    #[error("SQLite failed to open blob: {0}")]
    OpenFailed(SqliteErrorCode),
    /// `sqlite3_blob_read` returned a non-`OK` code.
    #[error("SQLite failed to read blob: {0}")]
    ReadFailed(SqliteErrorCode),
    /// `sqlite3_blob_write` returned a non-`OK` code.
    #[error("SQLite failed to write blob: {0}")]
    WriteFailed(SqliteErrorCode),
    /// `sqlite3_blob_reopen` returned a non-`OK` code.
    #[error("SQLite failed to reopen blob: {0}")]
    ReopenFailed(SqliteErrorCode),
    /// `sqlite3_blob_close` returned a non-`OK` code. Raised only from
    /// [`SqliteBlob::close`], never from `Drop`.
    #[error("SQLite failed to close blob: {0}")]
    CloseFailed(SqliteErrorCode),
}

/// A live incremental-blob handle.
///
/// See the [module docs](self) for the safety contract.
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::SqliteBlob;
/// assert_send::<SqliteBlob>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::SqliteBlob;
/// assert_sync::<SqliteBlob>();
/// ```
pub struct SqliteBlob {
    handle: *mut sqlite3_blob,
    mode: BlobMode,
    _not_send_or_sync: PhantomData<*const ()>,
}

impl SqliteBlob {
    /// Open an incremental blob handle.
    pub(crate) fn open_internal(
        conn: &mut SqliteConnection,
        database: &str,
        table: &str,
        column: &str,
        rowid: i64,
        mode: BlobMode,
    ) -> Result<Self, BlobError> {
        let c_database = CString::new(database).map_err(|_| BlobError::InvalidName)?;
        let c_table = CString::new(table).map_err(|_| BlobError::InvalidName)?;
        let c_column = CString::new(column).map_err(|_| BlobError::InvalidName)?;

        let mut handle: *mut sqlite3_blob = ptr::null_mut();
        // SAFETY: `with_raw_connection` yields a live `sqlite3*`; all three
        // `CString`s and `handle` outlive the call on this stack frame.
        let rc = unsafe {
            conn.with_raw_connection(|raw| {
                sqlite3_blob_open(
                    raw,
                    c_database.as_ptr(),
                    c_table.as_ptr(),
                    c_column.as_ptr(),
                    rowid,
                    mode.to_flags(),
                    &mut handle,
                )
            })
        };
        if rc != SQLITE_OK {
            return Err(BlobError::OpenFailed(SqliteErrorCode::from_error(rc)));
        }
        if handle.is_null() {
            return Err(BlobError::OpenFailed(SqliteErrorCode::Error));
        }
        Ok(Self {
            handle,
            mode,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Access mode this handle was opened with.
    #[must_use]
    pub fn mode(&self) -> BlobMode {
        self.mode
    }

    /// Size of the blob in bytes (`sqlite3_blob_bytes`).
    #[must_use]
    pub fn len(&self) -> usize {
        // SAFETY: `self.handle` is a live blob handle owned by this `SqliteBlob`.
        let n = unsafe { sqlite3_blob_bytes(self.handle) };
        usize::try_from(n).unwrap_or(0)
    }

    /// True iff [`len`](Self::len) is zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `buf.len()` bytes starting at `offset` into `buf`.
    ///
    /// # Errors
    ///
    /// - [`BlobError::OffsetOutOfRange`] when the read would extend past the
    ///   end of the blob or when `offset + buf.len()` overflows `usize`.
    /// - [`BlobError::LengthOverflow`] when `offset` or `buf.len()` do not fit
    ///   into an `i32`.
    /// - [`BlobError::ReadFailed`] when `SQLite` returns a non-`OK` code.
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), BlobError> {
        let range = self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }
        // SAFETY: `buf` is a valid mutable slice of `range.n` bytes and
        // `self.handle` is a live blob handle; `range` was pre-validated.
        let rc = unsafe {
            sqlite3_blob_read(self.handle, buf.as_mut_ptr().cast(), range.n, range.offset)
        };
        if rc != SQLITE_OK {
            return Err(BlobError::ReadFailed(SqliteErrorCode::from_error(rc)));
        }
        Ok(())
    }

    /// Write `buf` into the blob starting at `offset`.
    ///
    /// # Errors
    ///
    /// - [`BlobError::ReadOnly`] if this handle was opened with
    ///   [`BlobMode::ReadOnly`].
    /// - [`BlobError::OffsetOutOfRange`] when the write would extend past the
    ///   end of the blob or when `offset + buf.len()` overflows `usize`.
    /// - [`BlobError::LengthOverflow`] when `offset` or `buf.len()` do not fit
    ///   into an `i32`.
    /// - [`BlobError::WriteFailed`] when `SQLite` returns a non-`OK` code.
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> Result<(), BlobError> {
        if matches!(self.mode, BlobMode::ReadOnly) {
            return Err(BlobError::ReadOnly);
        }
        let range = self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }
        // SAFETY: `buf` is a valid slice of `range.n` bytes and `self.handle`
        // is a live blob handle opened with `SQLITE_OPEN_READWRITE`.
        let rc =
            unsafe { sqlite3_blob_write(self.handle, buf.as_ptr().cast(), range.n, range.offset) };
        if rc != SQLITE_OK {
            return Err(BlobError::WriteFailed(SqliteErrorCode::from_error(rc)));
        }
        Ok(())
    }

    /// Point this handle at a different rowid in the same database, table,
    /// and column (`sqlite3_blob_reopen`). Mode is preserved.
    ///
    /// # Errors
    ///
    /// [`BlobError::ReopenFailed`] when `SQLite` returns a non-`OK` code
    /// (typically because the target row does not exist).
    pub fn reopen(&mut self, rowid: i64) -> Result<(), BlobError> {
        // SAFETY: `self.handle` is a live blob handle owned by this `SqliteBlob`.
        let rc = unsafe { sqlite3_blob_reopen(self.handle, rowid) };
        if rc != SQLITE_OK {
            return Err(BlobError::ReopenFailed(SqliteErrorCode::from_error(rc)));
        }
        Ok(())
    }

    /// Close the handle and surface any `sqlite3_blob_close` error. `Drop`
    /// closes silently.
    ///
    /// # Errors
    ///
    /// [`BlobError::CloseFailed`] when `SQLite` returns a non-`OK` code.
    pub fn close(mut self) -> Result<(), BlobError> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        // SAFETY: `handle` came from `sqlite3_blob_open`; nulling `self.handle`
        // first makes `Drop` a no-op.
        let rc = unsafe { sqlite3_blob_close(handle) };
        if rc != SQLITE_OK {
            return Err(BlobError::CloseFailed(SqliteErrorCode::from_error(rc)));
        }
        Ok(())
    }

    /// Validate `offset + buf_len` against `self.len()` and pack the pair into
    /// a `Range` of `c_int` values for the FFI call.
    fn check_range(&self, offset: usize, buf_len: usize) -> Result<Range, BlobError> {
        let blob_len = self.len();
        let end = offset
            .checked_add(buf_len)
            .ok_or(BlobError::OffsetOutOfRange {
                offset,
                buf_len,
                blob_len,
            })?;
        if end > blob_len {
            return Err(BlobError::OffsetOutOfRange {
                offset,
                buf_len,
                blob_len,
            });
        }
        let n =
            c_int::try_from(buf_len).map_err(|_| BlobError::LengthOverflow { value: buf_len })?;
        let offset =
            c_int::try_from(offset).map_err(|_| BlobError::LengthOverflow { value: offset })?;
        Ok(Range { offset, n })
    }
}

impl std::fmt::Debug for SqliteBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteBlob")
            .field("mode", &self.mode)
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Drop for SqliteBlob {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: `self.handle` was returned by `sqlite3_blob_open` and no
        // other code path closes it. Any error is silently discarded; callers
        // who care use `close()` instead.
        unsafe {
            let _ = sqlite3_blob_close(self.handle);
        }
    }
}

/// Post-validation range packed for a single `sqlite3_blob_read` /
/// `sqlite3_blob_write` call.
struct Range {
    offset: c_int,
    n: c_int,
}

#[cfg(test)]
mod tests {
    use super::BlobMode;
    use crate::BlobError;

    #[test]
    fn blob_mode_to_flags_matches_sqlite_convention() {
        assert_eq!(BlobMode::ReadOnly.to_flags(), 0);
        assert_eq!(BlobMode::ReadWrite.to_flags(), 1);
    }

    #[test]
    fn blob_error_offset_out_of_range_carries_context() {
        let e = BlobError::OffsetOutOfRange {
            offset: 5,
            buf_len: 10,
            blob_len: 8,
        };
        let msg = format!("{e}");
        assert!(msg.contains('5'));
        assert!(msg.contains("10"));
        assert!(msg.contains('8'));
    }

    #[test]
    fn blob_error_length_overflow_carries_value() {
        let e = BlobError::LengthOverflow { value: usize::MAX };
        let msg = format!("{e}");
        assert!(msg.contains(&format!("{}", usize::MAX)));
    }
}
