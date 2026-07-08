//! Iterate over `SQLite` changeset blobs without applying them.
//!
//! Wraps the `sqlite3changeset_start` / `_next` / `_op` / `_pk` / `_old` /
//! `_new` / `_finalize` family. Given the bytes returned by
//! [`crate::Session::changeset`] or [`crate::Session::patchset`], step
//! through each recorded row and inspect its old and new values.
//!
//! ```
//! use diesel::prelude::*;
//! use diesel_sqlite_session::{ChangesetOp, ChangesetReader, SqliteSessionExt};
//!
//! let mut conn = SqliteConnection::establish(":memory:").unwrap();
//! diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
//!     .execute(&mut conn).unwrap();
//! let mut session = conn.create_session().unwrap();
//! session.attach_all().unwrap();
//! diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
//!     .execute(&mut conn).unwrap();
//! let changeset = session.changeset().unwrap();
//! drop(session);
//!
//! let mut reader = ChangesetReader::open(&changeset).unwrap();
//! while let Some(row) = reader.next().unwrap() {
//!     match row.op() {
//!         ChangesetOp::Insert => println!("insert into {}", row.table()),
//!         ChangesetOp::Update => println!("update on {}", row.table()),
//!         ChangesetOp::Delete => println!("delete from {}", row.table()),
//!     }
//! }
//! ```

use std::ffi::{c_char, c_int, c_uchar, c_void, CStr};
use std::marker::PhantomData;
use std::ptr;

use thiserror::Error;

use crate::errors::SqliteErrorCode;
use crate::ffi::{
    sqlite3_changeset_iter, sqlite3_value, sqlite3_value_blob, sqlite3_value_bytes,
    sqlite3_value_double, sqlite3_value_int64, sqlite3_value_type, sqlite3changeset_finalize,
    sqlite3changeset_new, sqlite3changeset_next, sqlite3changeset_old, sqlite3changeset_op,
    sqlite3changeset_pk, sqlite3changeset_start, sqlite3changeset_start_strm,
    sqlite3changeset_start_v2, sqlite3changeset_start_v2_strm, SQLITE_BLOB, SQLITE_DELETE,
    SQLITE_DONE, SQLITE_FLOAT, SQLITE_INSERT, SQLITE_INTEGER, SQLITE_NULL, SQLITE_OK, SQLITE_ROW,
    SQLITE_TEXT, SQLITE_UPDATE,
};

/// `SQLITE_CHANGESETSTART_INVERT`, hard-coded because older `libsqlite3-sys`
/// releases used through the shared `ffi` re-export do not define it.
const SQLITE_CHANGESETSTART_INVERT: c_int = 0x0002;

/// Row-modification operation recorded by a changeset entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ChangesetOp {
    /// `INSERT`.
    Insert = SQLITE_INSERT,
    /// `UPDATE`.
    Update = SQLITE_UPDATE,
    /// `DELETE`.
    Delete = SQLITE_DELETE,
}

impl ChangesetOp {
    /// Convert an `SQLite` op code into a [`ChangesetOp`].
    ///
    /// # Examples
    ///
    /// ```
    /// use diesel_sqlite_session::ChangesetOp;
    ///
    /// assert_eq!(ChangesetOp::from_raw(18), Some(ChangesetOp::Insert));
    /// assert_eq!(ChangesetOp::from_raw(0), None);
    /// ```
    #[must_use]
    pub const fn from_raw(code: i32) -> Option<Self> {
        match code {
            SQLITE_INSERT => Some(Self::Insert),
            SQLITE_UPDATE => Some(Self::Update),
            SQLITE_DELETE => Some(Self::Delete),
            _ => None,
        }
    }

    /// Get the raw `SQLite` op code.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        self as i32
    }
}

/// Dynamic type of a value read from a changeset row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ChangesetColumnType {
    /// 64-bit signed integer.
    Integer = SQLITE_INTEGER,
    /// IEEE 754 double.
    Float = SQLITE_FLOAT,
    /// UTF-8 text.
    Text = SQLITE_TEXT,
    /// Raw byte string.
    Blob = SQLITE_BLOB,
    /// `NULL`.
    Null = SQLITE_NULL,
}

impl ChangesetColumnType {
    fn from_raw(code: i32) -> Self {
        match code {
            SQLITE_INTEGER => Self::Integer,
            SQLITE_FLOAT => Self::Float,
            SQLITE_TEXT => Self::Text,
            SQLITE_BLOB => Self::Blob,
            _ => Self::Null,
        }
    }
}

/// Errors raised by the changeset iterator.
#[derive(Debug, Error)]
pub enum ChangesetError {
    /// The input changeset buffer is empty.
    #[error("changeset buffer is empty")]
    EmptyChangeset,
    /// The changeset buffer is larger than `i32::MAX` and cannot be passed to
    /// the C API.
    #[error("changeset length {value} does not fit into a 32-bit signed integer")]
    LengthOverflow {
        /// The length that failed the conversion.
        value: usize,
    },
    /// `sqlite3changeset_start` or `_start_v2` returned a non-`OK` code.
    #[error("SQLite failed to start changeset iterator: {0}")]
    StartFailed(SqliteErrorCode),
    /// `sqlite3changeset_next` returned neither `SQLITE_ROW` nor `SQLITE_DONE`.
    #[error("SQLite failed to advance the changeset iterator: {0}")]
    NextFailed(SqliteErrorCode),
    /// `sqlite3changeset_op` returned a non-`OK` code, or reported an
    /// unrecognized op code.
    #[error("SQLite failed to read the current op: {0}")]
    OpFailed(SqliteErrorCode),
    /// `sqlite3changeset_op` reported an op code outside `INSERT/UPDATE/DELETE`.
    #[error("SQLite reported an unknown changeset op code {code}")]
    UnknownOp {
        /// The unrecognized op code.
        code: i32,
    },
    /// `sqlite3changeset_pk` returned a non-`OK` code.
    #[error("SQLite failed to read primary key mask: {0}")]
    PkFailed(SqliteErrorCode),
    /// A column index passed to [`ChangesetRow::old_value`],
    /// [`ChangesetRow::new_value`], or [`ChangesetRow::is_primary_key`] is
    /// `>=` [`ChangesetRow::column_count`].
    #[error("column index {index} out of range (count = {count})")]
    ColumnOutOfRange {
        /// The requested index.
        index: u32,
        /// The current row's column count.
        count: usize,
    },
    /// [`ChangesetRow::old_value`] was called on an `INSERT` row, where the
    /// pre-image is absent.
    #[error("pre-image is not available for INSERT rows")]
    OldNotAvailableOnInsert,
    /// [`ChangesetRow::new_value`] was called on a `DELETE` row, where the
    /// post-image is absent.
    #[error("post-image is not available for DELETE rows")]
    NewNotAvailableOnDelete,
    /// `sqlite3changeset_old` or `_new` returned a non-`OK` code.
    #[error("SQLite failed to read column value: {0}")]
    ValueReadFailed(SqliteErrorCode),
    /// The table name reported by `sqlite3changeset_op` is not valid UTF-8.
    #[error("changeset table name is not valid UTF-8")]
    TableNameNotUtf8,
    /// `sqlite3changeset_invert` returned a non-`OK` code.
    #[error("SQLite failed to invert changeset: {0}")]
    InvertFailed(SqliteErrorCode),
    /// `sqlite3changeset_concat` returned a non-`OK` code.
    #[error("SQLite failed to concatenate changesets: {0}")]
    ConcatFailed(SqliteErrorCode),
    /// `sqlite3changegroup_new` returned a non-`OK` code.
    #[error("SQLite failed to create changegroup: {0}")]
    ChangegroupCreateFailed(SqliteErrorCode),
    /// `sqlite3changegroup_schema` returned a non-`OK` code.
    #[error("SQLite failed to attach schema to changegroup: {0}")]
    ChangegroupSchemaFailed(SqliteErrorCode),
    /// `sqlite3changegroup_add` returned a non-`OK` code.
    #[error("SQLite failed to fold changeset into changegroup: {0}")]
    ChangegroupAddFailed(SqliteErrorCode),
    /// `sqlite3changegroup_output` returned a non-`OK` code.
    #[error("SQLite failed to serialize changegroup: {0}")]
    ChangegroupOutputFailed(SqliteErrorCode),
    /// A database or schema name contained an interior null byte.
    #[error("changegroup schema name contains a null byte")]
    InvalidSchemaName,
    /// `sqlite3rebaser_create` returned a non-`OK` code.
    #[error("SQLite failed to create rebaser: {0}")]
    RebaserCreateFailed(SqliteErrorCode),
    /// `sqlite3rebaser_configure` returned a non-`OK` code.
    #[error("SQLite failed to configure rebaser: {0}")]
    RebaserConfigureFailed(SqliteErrorCode),
    /// `sqlite3rebaser_rebase` returned a non-`OK` code.
    #[error("SQLite failed to rebase changeset: {0}")]
    RebaserRebaseFailed(SqliteErrorCode),
    /// A streamed reader returned an [`std::io::Error`].
    #[error("streamed changeset reader failed: {0}")]
    ReaderIo(#[from] std::io::Error),
    /// A streamed reader panicked.
    #[error("streamed changeset reader panicked")]
    ReaderPanicked,
    /// A streamed writer returned an [`std::io::Error`].
    #[error("streamed changeset writer failed: {0}")]
    WriterIo(std::io::Error),
    /// A streamed writer panicked.
    #[error("streamed changeset writer panicked")]
    WriterPanicked,
}

/// A value borrowed from the current changeset row.
///
/// The lifetime `'a` is bound to the enclosing [`ChangesetRow`] and therefore
/// to the current iterator position. Advancing the iterator invalidates the
/// value.
pub struct ChangesetValue<'a> {
    value: *mut sqlite3_value,
    _marker: PhantomData<&'a sqlite3_value>,
}

impl<'a> ChangesetValue<'a> {
    pub(crate) fn from_ptr(value: *mut sqlite3_value) -> Self {
        Self {
            value,
            _marker: PhantomData,
        }
    }

    /// Dynamic storage class reported by `sqlite3_value_type`.
    #[must_use]
    pub fn column_type(&self) -> ChangesetColumnType {
        // SAFETY: `self.value` is a live `sqlite3_value*` valid for the
        // enclosing iterator position.
        let ty = unsafe { sqlite3_value_type(self.value) };
        ChangesetColumnType::from_raw(ty)
    }

    /// True iff the column is `NULL`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.column_type() == ChangesetColumnType::Null
    }

    /// Read the value as an `i64` (`sqlite3_value_int64`).
    #[must_use]
    pub fn as_i64(&self) -> i64 {
        // SAFETY: value is live for the iterator's current position.
        unsafe { sqlite3_value_int64(self.value) }
    }

    /// Read the value as an `f64` (`sqlite3_value_double`).
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        // SAFETY: value is live for the iterator's current position.
        unsafe { sqlite3_value_double(self.value) }
    }

    /// Read the value as UTF-8 text. Returns `None` if the underlying bytes
    /// are `NULL` or not valid UTF-8.
    #[must_use]
    pub fn as_text(&self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Read the value as a byte slice. Returns `None` for `NULL` values.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        if self.is_null() {
            return None;
        }
        // SAFETY: value is live and non-NULL.
        let len = unsafe { sqlite3_value_bytes(self.value) };
        let Ok(len) = usize::try_from(len) else {
            return None;
        };
        if len == 0 {
            return Some(&[]);
        }
        // SAFETY: `sqlite3_value_blob` returns a non-null pointer to `len`
        // bytes readable for the current iterator position.
        let ptr = unsafe { sqlite3_value_blob(self.value) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: length matches the value the SQLite side reports.
        let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        Some(slice)
    }
}

impl std::fmt::Debug for ChangesetValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesetValue")
            .field("column_type", &self.column_type())
            .finish_non_exhaustive()
    }
}

/// Read-only iterator over a changeset blob.
///
/// Borrows the input bytes for its whole lifetime because `SQLite` keeps a
/// raw pointer into them. `!Send + !Sync`.
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::ChangesetReader;
/// assert_send::<ChangesetReader<'static>>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::ChangesetReader;
/// assert_sync::<ChangesetReader<'static>>();
/// ```
pub struct ChangesetReader<'a> {
    iter: *mut sqlite3_changeset_iter,
    /// Kept alive for streamed variants (`open_strm` / `open_inverted_strm`)
    /// so the boxed `InputContext<R>` stays at a stable heap address for the
    /// whole iterator lifetime. `None` for the buffered variants.
    _keep_reader: Option<Box<dyn ReaderKeepAlive + 'a>>,
    _borrow: PhantomData<&'a [u8]>,
    _not_send_or_sync: PhantomData<*const ()>,
}

/// Type-erasure trait used to keep the streamed reader's `InputContext`
/// alive without exposing its generic type.
trait ReaderKeepAlive {}
impl<T> ReaderKeepAlive for T {}

impl<'a> ChangesetReader<'a> {
    /// Open a reader over the given changeset bytes.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::EmptyChangeset`] if `bytes` is empty.
    /// - [`ChangesetError::LengthOverflow`] if `bytes.len()` does not fit in `i32`.
    /// - [`ChangesetError::StartFailed`] on any `sqlite3changeset_start` error.
    pub fn open(bytes: &'a [u8]) -> Result<Self, ChangesetError> {
        Self::open_impl(bytes, false)
    }

    /// Open a reader that iterates the inverse of `bytes`
    /// (`SQLITE_CHANGESETSTART_INVERT`): `INSERT` reads as `DELETE` and vice
    /// versa; `UPDATE` rows swap old and new fields.
    ///
    /// # Errors
    ///
    /// Same as [`open`](Self::open).
    pub fn open_inverted(bytes: &'a [u8]) -> Result<Self, ChangesetError> {
        Self::open_impl(bytes, true)
    }

    fn open_impl(bytes: &'a [u8], invert: bool) -> Result<Self, ChangesetError> {
        if bytes.is_empty() {
            return Err(ChangesetError::EmptyChangeset);
        }
        let len = c_int::try_from(bytes.len())
            .map_err(|_| ChangesetError::LengthOverflow { value: bytes.len() })?;

        let mut iter: *mut sqlite3_changeset_iter = ptr::null_mut();
        // SAFETY: `bytes` outlives the reader via `'a`;
        // `sqlite3changeset_start*` treats `pChangeset` as read-only.
        let rc = unsafe {
            let buf = bytes.as_ptr().cast_mut().cast::<c_void>();
            if invert {
                sqlite3changeset_start_v2(&mut iter, len, buf, SQLITE_CHANGESETSTART_INVERT)
            } else {
                sqlite3changeset_start(&mut iter, len, buf)
            }
        };
        if rc != SQLITE_OK {
            return Err(ChangesetError::StartFailed(SqliteErrorCode::from_error(rc)));
        }
        if iter.is_null() {
            return Err(ChangesetError::StartFailed(SqliteErrorCode::Error));
        }
        Ok(Self {
            iter,
            _keep_reader: None,
            _borrow: PhantomData,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Open a streamed reader over any [`std::io::Read`]. `SQLite` pulls
    /// bytes on demand, so this handles changesets larger than memory.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::StartFailed`] on any `SQLite` error, including
    ///   `SQLITE_IOERR` raised by the trampoline when `reader` errored on
    ///   the initial chunk.
    pub fn open_strm<R>(reader: R) -> Result<Self, ChangesetError>
    where
        R: std::io::Read + 'a,
    {
        Self::open_strm_impl(reader, false)
    }

    /// Streamed variant of [`open_inverted`](Self::open_inverted).
    ///
    /// # Errors
    ///
    /// Same as [`open_strm`](Self::open_strm).
    pub fn open_inverted_strm<R>(reader: R) -> Result<Self, ChangesetError>
    where
        R: std::io::Read + 'a,
    {
        Self::open_strm_impl(reader, true)
    }

    fn open_strm_impl<R>(reader: R, invert: bool) -> Result<Self, ChangesetError>
    where
        R: std::io::Read + 'a,
    {
        let mut ctx = Box::new(crate::streaming::InputContext::new(reader));
        let ptr = std::ptr::addr_of_mut!(*ctx).cast::<c_void>();

        let mut iter: *mut sqlite3_changeset_iter = ptr::null_mut();
        // SAFETY: `ctx` stays boxed until moved into `_keep_reader`, so
        // `ptr` is a stable address for the whole FFI call.
        let rc = unsafe {
            if invert {
                sqlite3changeset_start_v2_strm(
                    &mut iter,
                    Some(crate::streaming::read_trampoline::<R>),
                    ptr,
                    SQLITE_CHANGESETSTART_INVERT,
                )
            } else {
                sqlite3changeset_start_strm(
                    &mut iter,
                    Some(crate::streaming::read_trampoline::<R>),
                    ptr,
                )
            }
        };
        if rc != SQLITE_OK {
            return Err(ChangesetError::StartFailed(SqliteErrorCode::from_error(rc)));
        }
        if iter.is_null() {
            return Err(ChangesetError::StartFailed(SqliteErrorCode::Error));
        }
        Ok(Self {
            iter,
            _keep_reader: Some(ctx),
            _borrow: PhantomData,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Advance the iterator. Returns `Ok(None)` at end. The returned
    /// [`ChangesetRow`] borrows the reader mutably; the next call
    /// invalidates it.
    ///
    /// # Errors
    ///
    /// [`ChangesetError::NextFailed`] if `sqlite3changeset_next` returns
    /// neither `SQLITE_ROW` nor `SQLITE_DONE`. Also propagates
    /// [`ChangesetError::OpFailed`] / `PkFailed` from populating the row.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<ChangesetRow<'_>>, ChangesetError> {
        // SAFETY: `self.iter` is a live iterator owned by this reader.
        let rc = unsafe { sqlite3changeset_next(self.iter) };
        match rc {
            SQLITE_ROW => {
                // SAFETY: `sqlite3changeset_next` returned SQLITE_ROW, so the
                // iterator is positioned at a valid row.
                let row = unsafe { ChangesetRow::read_current(self.iter) }?;
                Ok(Some(row))
            }
            SQLITE_DONE => Ok(None),
            _ => Err(ChangesetError::NextFailed(SqliteErrorCode::from_error(rc))),
        }
    }
}

impl Drop for ChangesetReader<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.iter` was returned by `sqlite3changeset_start*` and
        // is still live. `sqlite3changeset_finalize` closes it exactly once.
        unsafe {
            let _ = sqlite3changeset_finalize(self.iter);
        }
    }
}

impl std::fmt::Debug for ChangesetReader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesetReader").finish_non_exhaustive()
    }
}

/// The current row exposed by [`ChangesetReader::next`]. Holds the table,
/// column count, indirect flag, and PK mask; values are read on demand via
/// [`old_value`](Self::old_value) / [`new_value`](Self::new_value).
pub struct ChangesetRow<'a> {
    iter: *mut sqlite3_changeset_iter,
    op: ChangesetOp,
    table: &'a str,
    column_count: usize,
    indirect: bool,
    pk_mask: &'a [c_uchar],
    _marker: PhantomData<&'a mut sqlite3_changeset_iter>,
}

impl<'a> ChangesetRow<'a> {
    pub(crate) unsafe fn read_current(
        iter: *mut sqlite3_changeset_iter,
    ) -> Result<Self, ChangesetError> {
        let mut table_ptr: *const c_char = ptr::null();
        let mut n_col: c_int = 0;
        let mut op_code: c_int = 0;
        let mut indirect_int: c_int = 0;
        // SAFETY: called only after `sqlite3changeset_next` returned SQLITE_ROW.
        let rc = unsafe {
            sqlite3changeset_op(
                iter,
                &mut table_ptr,
                &mut n_col,
                &mut op_code,
                &mut indirect_int,
            )
        };
        if rc != SQLITE_OK {
            return Err(ChangesetError::OpFailed(SqliteErrorCode::from_error(rc)));
        }
        let op =
            ChangesetOp::from_raw(op_code).ok_or(ChangesetError::UnknownOp { code: op_code })?;
        let table = if table_ptr.is_null() {
            ""
        } else {
            // SAFETY: `table_ptr` is a valid C string per `sqlite3changeset_op`.
            unsafe { CStr::from_ptr(table_ptr) }
                .to_str()
                .map_err(|_| ChangesetError::TableNameNotUtf8)?
        };
        let column_count = usize::try_from(n_col).unwrap_or(0);
        let indirect = indirect_int != 0;

        let mut pk_ptr: *mut c_uchar = ptr::null_mut();
        let mut pk_n: c_int = 0;
        // SAFETY: iterator is live at a row position.
        let rc = unsafe { sqlite3changeset_pk(iter, &mut pk_ptr, &mut pk_n) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::PkFailed(SqliteErrorCode::from_error(rc)));
        }
        let pk_len = usize::try_from(pk_n).unwrap_or(0);
        let pk_mask: &'a [c_uchar] = if pk_ptr.is_null() || pk_len == 0 {
            &[]
        } else {
            // SAFETY: `sqlite3changeset_pk` returns a pointer into the iterator's
            // memory valid until the next `_next`; `_marker` binds it to `'a`.
            unsafe { std::slice::from_raw_parts(pk_ptr, pk_len) }
        };

        Ok(Self {
            iter,
            op,
            table,
            column_count,
            indirect,
            pk_mask,
            _marker: PhantomData,
        })
    }

    /// Raw iterator pointer at this row (used by
    /// [`crate::Changegroup::add_change`] to fold a single change without
    /// re-serializing the whole changeset).
    #[must_use]
    pub(crate) fn as_raw_iter(&self) -> *mut sqlite3_changeset_iter {
        self.iter
    }

    /// The row's operation kind.
    #[must_use]
    pub fn op(&self) -> ChangesetOp {
        self.op
    }

    /// The table this row belongs to.
    #[must_use]
    pub fn table(&self) -> &'a str {
        self.table
    }

    /// Number of columns in the row (`sqlite3changeset_op`).
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    /// The indirect flag from `sqlite3changeset_op`. `true` for rows recorded
    /// through a trigger while the session had `set_indirect(true)` in effect.
    #[must_use]
    pub fn indirect(&self) -> bool {
        self.indirect
    }

    /// True if column `index` is part of the row's primary key.
    ///
    /// # Errors
    ///
    /// [`ChangesetError::ColumnOutOfRange`] if `index` is `>=` the row's
    /// primary key mask length (which equals [`column_count`](Self::column_count)).
    pub fn is_primary_key(&self, index: u32) -> Result<bool, ChangesetError> {
        let i = usize::try_from(index).unwrap_or(usize::MAX);
        if i >= self.pk_mask.len() {
            return Err(ChangesetError::ColumnOutOfRange {
                index,
                count: self.pk_mask.len(),
            });
        }
        Ok(self.pk_mask[i] != 0)
    }

    /// Read the pre-image value at `index`. Returns `Ok(None)` if `SQLite`
    /// recorded no old value (an `UPDATE` column that was not modified).
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::OldNotAvailableOnInsert`] on `INSERT` rows.
    /// - [`ChangesetError::ColumnOutOfRange`] if `index >= column_count()`.
    /// - [`ChangesetError::ValueReadFailed`] on any other `SQLite` error.
    pub fn old_value(&self, index: u32) -> Result<Option<ChangesetValue<'a>>, ChangesetError> {
        if matches!(self.op, ChangesetOp::Insert) {
            return Err(ChangesetError::OldNotAvailableOnInsert);
        }
        self.value_at(index, sqlite3changeset_old)
    }

    /// Read the post-image value at `index`. Returns `Ok(None)` if `SQLite`
    /// recorded no new value (an `UPDATE` column that was not modified).
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::NewNotAvailableOnDelete`] on `DELETE` rows.
    /// - [`ChangesetError::ColumnOutOfRange`] if `index >= column_count()`.
    /// - [`ChangesetError::ValueReadFailed`] on any other `SQLite` error.
    pub fn new_value(&self, index: u32) -> Result<Option<ChangesetValue<'a>>, ChangesetError> {
        if matches!(self.op, ChangesetOp::Delete) {
            return Err(ChangesetError::NewNotAvailableOnDelete);
        }
        self.value_at(index, sqlite3changeset_new)
    }

    fn value_at(
        &self,
        index: u32,
        accessor: unsafe extern "C" fn(
            *mut sqlite3_changeset_iter,
            c_int,
            *mut *mut sqlite3_value,
        ) -> c_int,
    ) -> Result<Option<ChangesetValue<'a>>, ChangesetError> {
        if usize::try_from(index).unwrap_or(usize::MAX) >= self.column_count {
            return Err(ChangesetError::ColumnOutOfRange {
                index,
                count: self.column_count,
            });
        }
        let idx = c_int::try_from(index).map_err(|_| ChangesetError::ColumnOutOfRange {
            index,
            count: self.column_count,
        })?;
        let mut value: *mut sqlite3_value = ptr::null_mut();
        // SAFETY: `self.iter` is a live iterator at a row position and
        // `value` is a valid out-pointer.
        let rc = unsafe { accessor(self.iter, idx, &mut value) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ValueReadFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(ChangesetValue::from_ptr(value)))
        }
    }
}

impl std::fmt::Debug for ChangesetRow<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesetRow")
            .field("op", &self.op)
            .field("table", &self.table)
            .field("column_count", &self.column_count)
            .field("indirect", &self.indirect)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{ChangesetColumnType, ChangesetError, ChangesetOp};

    #[test]
    fn changeset_op_from_raw_roundtrips() {
        for op in [
            ChangesetOp::Insert,
            ChangesetOp::Update,
            ChangesetOp::Delete,
        ] {
            assert_eq!(ChangesetOp::from_raw(op.to_raw()), Some(op));
        }
        assert_eq!(ChangesetOp::from_raw(0), None);
        assert_eq!(ChangesetOp::from_raw(999), None);
    }

    #[test]
    fn changeset_column_type_from_raw_falls_back_to_null() {
        assert_eq!(
            ChangesetColumnType::from_raw(super::SQLITE_INTEGER),
            ChangesetColumnType::Integer,
        );
        assert_eq!(
            ChangesetColumnType::from_raw(super::SQLITE_FLOAT),
            ChangesetColumnType::Float,
        );
        assert_eq!(
            ChangesetColumnType::from_raw(super::SQLITE_TEXT),
            ChangesetColumnType::Text,
        );
        assert_eq!(
            ChangesetColumnType::from_raw(super::SQLITE_BLOB),
            ChangesetColumnType::Blob,
        );
        assert_eq!(
            ChangesetColumnType::from_raw(super::SQLITE_NULL),
            ChangesetColumnType::Null,
        );
        assert_eq!(
            ChangesetColumnType::from_raw(-42),
            ChangesetColumnType::Null,
        );
    }

    #[test]
    fn column_out_of_range_display_carries_indices() {
        let e = ChangesetError::ColumnOutOfRange { index: 9, count: 3 };
        let msg = format!("{e}");
        assert!(msg.contains('9'));
        assert!(msg.contains('3'));
    }
}
