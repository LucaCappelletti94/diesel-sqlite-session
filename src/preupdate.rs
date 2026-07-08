//! `SQLite` pre-update hook wrappers.
//!
//! Wraps `sqlite3_preupdate_hook` and its companions (`_old`, `_new`,
//! `_count`, `_depth`, `_blobwrite`). They only exist when `SQLite` is built
//! with `SQLITE_ENABLE_PREUPDATE_HOOK`, the same flag the session extension
//! needs, so this crate is their natural home.
//!
//! # Semantics
//!
//! The callback fires once per row, just before a real `INSERT`, `UPDATE`, or
//! `DELETE` on a rowid table (never for virtual or `WITHOUT ROWID` tables).
//! Inside it, use [`PreUpdateEvent::old_value`] / `new_value` for column
//! values and [`PreUpdateEvent::blob_write_column`] to detect ongoing
//! `sqlite3_blob_write` calls. The closure must not touch the triggering
//! connection (same rule as `sqlite3_commit_hook` and `sqlite3_update_hook`).
//! Panics are caught in the trampoline.
//!
//! # Mutual exclusion with `Session`
//!
//! The session extension uses the same `sqlite3_preupdate_hook` slot for its
//! own callback, so [`crate::Session`] and [`PreUpdateHook`] must not overlap
//! on the same connection. Drop every `Session` before `on_preupdate`, and
//! every `PreUpdateHook` before `create_session`. Both `install` and `Drop`
//! refuse to reclaim any `pCtx` pointer they did not install themselves.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use diesel::SqliteConnection;
use thiserror::Error;

use crate::errors::SqliteErrorCode;
use crate::ffi::{
    sqlite3, sqlite3_int64, sqlite3_preupdate_blobwrite, sqlite3_preupdate_count,
    sqlite3_preupdate_depth, sqlite3_preupdate_hook, sqlite3_preupdate_new, sqlite3_preupdate_old,
    sqlite3_value, sqlite3_value_blob, sqlite3_value_bytes, sqlite3_value_double,
    sqlite3_value_int64, sqlite3_value_type, SQLITE_BLOB, SQLITE_DELETE, SQLITE_FLOAT,
    SQLITE_INSERT, SQLITE_INTEGER, SQLITE_NULL, SQLITE_OK, SQLITE_TEXT, SQLITE_UPDATE,
};

/// The row-modification operation that triggered a pre-update callback.
///
/// A `sqlite3_blob_write` on a rowid table also fires the pre-update hook and
/// is reported here as [`PreUpdateOp::Delete`]. Use
/// [`PreUpdateEvent::blob_write_column`] to distinguish that case from a real
/// `DELETE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum PreUpdateOp {
    /// `INSERT`.
    Insert = SQLITE_INSERT,
    /// `UPDATE`.
    Update = SQLITE_UPDATE,
    /// `DELETE`, or an active `sqlite3_blob_write` on a rowid column.
    Delete = SQLITE_DELETE,
}

impl PreUpdateOp {
    /// Convert an `SQLite` operation code into a [`PreUpdateOp`]. Returns
    /// `None` for any code other than `SQLITE_INSERT`, `_UPDATE`, or `_DELETE`.
    ///
    /// # Examples
    ///
    /// ```
    /// use diesel_sqlite_session::PreUpdateOp;
    ///
    /// assert_eq!(PreUpdateOp::from_raw(18), Some(PreUpdateOp::Insert));
    /// assert_eq!(PreUpdateOp::from_raw(0), None);
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

    /// Get the raw `SQLite` operation code.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        self as i32
    }
}

/// The dynamic type of a pre-update column value.
///
/// Matches the values reported by `sqlite3_value_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum PreUpdateColumnType {
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

impl PreUpdateColumnType {
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

/// Errors raised by the pre-update accessors.
#[derive(Debug, Error)]
pub enum PreUpdateError {
    /// The column index passed to [`PreUpdateEvent::old_value`] or
    /// [`PreUpdateEvent::new_value`] is `>=` [`PreUpdateEvent::column_count`].
    #[error("column index {index} out of range (count = {count})")]
    ColumnOutOfRange {
        /// Index the caller requested.
        index: u32,
        /// Number of columns in the affected row (`sqlite3_preupdate_count`).
        count: usize,
    },
    /// [`PreUpdateEvent::old_value`] was called during an `INSERT`, where no
    /// pre-image exists.
    #[error("pre-image is not available for INSERT")]
    OldNotAvailableOnInsert,
    /// [`PreUpdateEvent::new_value`] was called during a `DELETE`, where no
    /// post-image exists.
    #[error("post-image is not available for DELETE")]
    NewNotAvailableOnDelete,
    /// `SQLite` returned a non-`OK` result from a pre-update accessor.
    #[error("SQLite pre-update accessor failed: {0}")]
    Sqlite(SqliteErrorCode),
}

/// A single `sqlite3_value` snapshot returned by
/// [`PreUpdateEvent::old_value`] or [`PreUpdateEvent::new_value`].
///
/// The borrowed `sqlite3_value` is only valid until the pre-update callback
/// returns, hence the lifetime parameter is tied to the enclosing
/// [`PreUpdateEvent`] borrow.
pub struct PreUpdateValue<'a> {
    value: *mut sqlite3_value,
    _marker: PhantomData<&'a sqlite3_value>,
}

impl<'a> PreUpdateValue<'a> {
    /// Dynamic type of the underlying `sqlite3_value`.
    #[must_use]
    pub fn column_type(&self) -> PreUpdateColumnType {
        // SAFETY: `self.value` came from `_preupdate_old` / `_new` and is live
        // for the callback frame.
        let ty = unsafe { sqlite3_value_type(self.value) };
        PreUpdateColumnType::from_raw(ty)
    }

    /// True when the column value is `NULL`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.column_type() == PreUpdateColumnType::Null
    }

    /// Read the value as an `i64` (`sqlite3_value_int64`, with SQLite's
    /// coercion rules).
    #[must_use]
    pub fn as_i64(&self) -> i64 {
        // SAFETY: value pointer is live for the callback frame.
        unsafe { sqlite3_value_int64(self.value) }
    }

    /// Read the value as an `f64` (`sqlite3_value_double`).
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        // SAFETY: value pointer is live for the callback frame.
        unsafe { sqlite3_value_double(self.value) }
    }

    /// Read the value as UTF-8 text. `None` for `NULL` or non-UTF-8 bytes.
    /// The slice borrows from `SQLite`'s per-value buffer and lives only
    /// until the callback returns.
    #[must_use]
    pub fn as_text(&self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Read the value as a byte slice. `None` for `NULL` or a zero-length
    /// value with a null buffer. Borrows from `SQLite`'s per-value buffer
    /// and lives only until the callback returns.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        if self.is_null() {
            return None;
        }
        // SAFETY: `self.value` is live for the callback frame.
        let len = unsafe { sqlite3_value_bytes(self.value) };
        let Ok(len) = usize::try_from(len) else {
            return None;
        };
        if len == 0 {
            return Some(&[]);
        }
        // SAFETY: `sqlite3_value_blob` returns `len` readable bytes for the
        // callback duration on non-NULL, non-empty values.
        let ptr = unsafe { sqlite3_value_blob(self.value) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: length matches what SQLite reports.
        let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        Some(slice)
    }
}

impl std::fmt::Debug for PreUpdateValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreUpdateValue")
            .field("column_type", &self.column_type())
            .finish()
    }
}

/// Context passed to the user's pre-update callback. Only valid inside the
/// callback; the lifetime `'a` is bound to the trampoline stack frame.
pub struct PreUpdateEvent<'a> {
    db: *mut sqlite3,
    op: PreUpdateOp,
    database: &'a str,
    table: &'a str,
    old_rowid: i64,
    new_rowid: i64,
    _marker: PhantomData<&'a mut sqlite3>,
}

impl<'a> PreUpdateEvent<'a> {
    /// Operation kind (see [`PreUpdateOp`]).
    #[must_use]
    pub fn op(&self) -> PreUpdateOp {
        self.op
    }

    /// Name of the affected database (`"main"`, `"temp"`, or an `ATTACH` alias).
    #[must_use]
    pub fn database(&self) -> &'a str {
        self.database
    }

    /// Name of the affected table.
    #[must_use]
    pub fn table(&self) -> &'a str {
        self.table
    }

    /// Rowid before the change (undefined for `INSERT`).
    #[must_use]
    pub fn old_rowid(&self) -> i64 {
        self.old_rowid
    }

    /// Rowid after the change (undefined for `DELETE`).
    #[must_use]
    pub fn new_rowid(&self) -> i64 {
        self.new_rowid
    }

    /// Nesting depth. `0` at the top level, `>0` inside a trigger
    /// (`sqlite3_preupdate_depth`).
    #[must_use]
    pub fn depth(&self) -> u32 {
        // SAFETY: only callable inside the pre-update callback, where `self.db`
        // is the connection that fired the event.
        let depth = unsafe { sqlite3_preupdate_depth(self.db) };
        u32::try_from(depth).unwrap_or(0)
    }

    /// Number of columns in the affected row (`sqlite3_preupdate_count`).
    #[must_use]
    pub fn column_count(&self) -> usize {
        // SAFETY: only callable inside the pre-update callback.
        let count = unsafe { sqlite3_preupdate_count(self.db) };
        usize::try_from(count).unwrap_or(0)
    }

    /// Column index for an in-progress `sqlite3_blob_write`, or `None` for
    /// regular DML (`sqlite3_preupdate_blobwrite`, added in `SQLite` 3.36.0).
    #[must_use]
    pub fn blob_write_column(&self) -> Option<u32> {
        // SAFETY: only callable inside the pre-update callback.
        let column = unsafe { sqlite3_preupdate_blobwrite(self.db) };
        u32::try_from(column).ok()
    }

    /// Read the pre-image value at `column`. Only valid on `UPDATE` and
    /// `DELETE`; `INSERT` yields [`PreUpdateError::OldNotAvailableOnInsert`].
    ///
    /// # Errors
    ///
    /// - [`PreUpdateError::ColumnOutOfRange`] if `column >= column_count()`.
    /// - [`PreUpdateError::OldNotAvailableOnInsert`] on `INSERT` operations.
    /// - [`PreUpdateError::Sqlite`] if `SQLite` returns a non-`OK` code.
    pub fn old_value(&self, column: u32) -> Result<PreUpdateValue<'a>, PreUpdateError> {
        if matches!(self.op, PreUpdateOp::Insert) {
            return Err(PreUpdateError::OldNotAvailableOnInsert);
        }
        self.value_at(column, sqlite3_preupdate_old)
    }

    /// Read the post-image value at `column`. Only valid on `UPDATE` and
    /// `INSERT`; `DELETE` yields [`PreUpdateError::NewNotAvailableOnDelete`].
    ///
    /// # Errors
    ///
    /// - [`PreUpdateError::ColumnOutOfRange`] if `column >= column_count()`.
    /// - [`PreUpdateError::NewNotAvailableOnDelete`] on `DELETE` operations.
    /// - [`PreUpdateError::Sqlite`] if `SQLite` returns a non-`OK` code.
    pub fn new_value(&self, column: u32) -> Result<PreUpdateValue<'a>, PreUpdateError> {
        if matches!(self.op, PreUpdateOp::Delete) {
            return Err(PreUpdateError::NewNotAvailableOnDelete);
        }
        self.value_at(column, sqlite3_preupdate_new)
    }

    fn value_at(
        &self,
        column: u32,
        accessor: unsafe extern "C" fn(*mut sqlite3, c_int, *mut *mut sqlite3_value) -> c_int,
    ) -> Result<PreUpdateValue<'a>, PreUpdateError> {
        let count = self.column_count();
        if usize::try_from(column).unwrap_or(usize::MAX) >= count {
            return Err(PreUpdateError::ColumnOutOfRange {
                index: column,
                count,
            });
        }
        let index = c_int::try_from(column).map_err(|_| PreUpdateError::ColumnOutOfRange {
            index: column,
            count,
        })?;
        let mut value: *mut sqlite3_value = ptr::null_mut();
        // SAFETY: `self.db` is the connection that fired the callback and
        // remains live for the callback frame.
        let rc = unsafe { accessor(self.db, index, &mut value) };
        if rc != SQLITE_OK {
            return Err(PreUpdateError::Sqlite(SqliteErrorCode::from_error(rc)));
        }
        if value.is_null() {
            return Err(PreUpdateError::Sqlite(SqliteErrorCode::Error));
        }
        Ok(PreUpdateValue {
            value,
            _marker: PhantomData,
        })
    }
}

impl std::fmt::Debug for PreUpdateEvent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreUpdateEvent")
            .field("op", &self.op)
            .field("database", &self.database)
            .field("table", &self.table)
            .field("old_rowid", &self.old_rowid)
            .field("new_rowid", &self.new_rowid)
            .finish()
    }
}

/// Type-erased boxed closure kept alive while a pre-update hook is
/// registered. Reclaimed on re-registration or when [`PreUpdateHook::drop`]
/// runs.
struct HookBox {
    call: Box<dyn FnMut(PreUpdateEvent<'_>) + Send>,
}

/// RAII guard owning a registered pre-update hook.
///
/// The hook is registered when the guard is created and unregistered when the
/// guard is dropped. The guard **must** be dropped before the
/// `SqliteConnection` it was created from, exactly like [`crate::Session`].
/// Using a guard after its connection has been dropped is undefined behavior.
///
/// The guard is `!Send` and `!Sync`, matching the underlying `SqliteConnection`.
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::PreUpdateHook;
/// assert_send::<PreUpdateHook>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::PreUpdateHook;
/// assert_sync::<PreUpdateHook>();
/// ```
///
/// # Replacement
///
/// `SQLite` allows one pre-update hook per connection. A second
/// [`SqliteSessionExt::on_preupdate`](crate::SqliteSessionExt::on_preupdate)
/// while a guard is alive replaces the callback and drops the older closure;
/// dropping the stale guard afterwards then removes the *new* hook, so keep
/// the freshest guard around.
pub struct PreUpdateHook {
    db: *mut sqlite3,
    box_ptr: *mut HookBox,
    _not_send_or_sync: PhantomData<*const ()>,
}

impl PreUpdateHook {
    /// Register `hook` as the connection's pre-update callback. If the slot
    /// was already occupied (by an earlier hook or a live
    /// [`crate::Session`]), the previous `pCtx` is leaked rather than
    /// reclaimed as a foreign type; see the module-level "Mutual exclusion"
    /// note.
    pub(crate) fn install<F>(conn: &mut SqliteConnection, hook: F) -> Self
    where
        F: FnMut(PreUpdateEvent<'_>) + Send + 'static,
    {
        let boxed = Box::new(HookBox {
            call: Box::new(hook),
        });
        let box_ptr = Box::into_raw(boxed);
        // SAFETY: `with_raw_connection` yields a live `sqlite3*` for the
        // callback duration. We deliberately ignore the previous `pCtx`
        // returned by `sqlite3_preupdate_hook`: if it belongs to `Session`,
        // reclaiming it as `Box<HookBox>` is UB. Leaking is a small memory
        // cost that only bites callers who violate the mutual exclusion.
        let db = unsafe {
            conn.with_raw_connection(|raw| {
                let _prev = sqlite3_preupdate_hook(raw, Some(trampoline), box_ptr.cast::<c_void>());
                raw
            })
        };
        Self {
            db,
            box_ptr,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl Drop for PreUpdateHook {
    fn drop(&mut self) {
        // Swap `None` into the slot to stop callbacks, then read what
        // `SQLite` was holding.
        //
        // SAFETY: `self.db` came from `with_raw_connection` and is still open.
        let prev = unsafe { sqlite3_preupdate_hook(self.db, None, ptr::null_mut()) };
        if prev == self.box_ptr.cast::<c_void>() {
            // We were still the registered owner; reclaim.
            //
            // SAFETY: leaked from `Box::into_raw` in `install`.
            unsafe { drop(Box::from_raw(self.box_ptr)) };
        }
        // Otherwise the slot holds a foreign pointer (a later `PreUpdateHook`,
        // or a `sqlite3_session*` stashed by the session extension).
        // `Box::from_raw` on such a pointer is UB, so we leak `self.box_ptr`.
    }
}

/// C trampoline installed as the `xPreUpdate` callback.
///
/// # Safety
///
/// `user_data` must point to an owned `HookBox` produced by [`PreUpdateHook::install`].
/// `db`, `db_name`, and `table_name` are valid for the callback frame per
/// `SQLite`'s pre-update contract.
unsafe extern "C" fn trampoline(
    user_data: *mut c_void,
    db: *mut sqlite3,
    op: c_int,
    db_name: *const c_char,
    table_name: *const c_char,
    old_rowid: sqlite3_int64,
    new_rowid: sqlite3_int64,
) {
    // SAFETY: SQLite hands back the `pCtx` we installed, which is a live
    // `HookBox` for the callback (guard's Drop runs only after SQLite swapped).
    let hook = unsafe { &mut *(user_data.cast::<HookBox>()) };
    let Some(op) = PreUpdateOp::from_raw(op) else {
        return;
    };
    // SAFETY: `db_name` is valid for the callback frame per SQLite's contract.
    let db_name = unsafe { cstr_lossy(db_name) };
    // SAFETY: `table_name` is valid for the callback frame per SQLite's contract.
    let table = unsafe { cstr_lossy(table_name) };
    let db_name_ref: &str = db_name.as_deref().unwrap_or("");
    let table_ref: &str = table.as_deref().unwrap_or("");

    let event = PreUpdateEvent {
        db,
        op,
        database: db_name_ref,
        table: table_ref,
        old_rowid,
        new_rowid,
        _marker: PhantomData,
    };

    // Catch user panics so unwinding never crosses the FFI boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| (hook.call)(event)));
}

/// Convert a nullable C string into an owned `String`, using lossy UTF-8 on
/// invalid input. Returns `None` for null.
///
/// # Safety
///
/// If `ptr` is non-null it must point to a valid NUL-terminated C string for
/// the duration of this call.
unsafe fn cstr_lossy(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid C string for the call.
    let s = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::{
        cstr_lossy, trampoline, HookBox, PreUpdateColumnType, PreUpdateEvent, PreUpdateOp,
    };
    use std::ffi::{c_char, c_void};
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Directly invoke the trampoline with a leaked `HookBox`, then reclaim it.
    ///
    /// This exercises the raw pointer round-trip, the op decode, and the
    /// closure dispatch without ever touching real `SQLite`. The closure MUST
    /// only read fields on `PreUpdateEvent` that do not deref `self.db`
    /// (`op`, `database`, `table`, `old_rowid`, `new_rowid`).
    unsafe fn drive_trampoline<F>(
        f: F,
        op: i32,
        db_name: *const c_char,
        table_name: *const c_char,
        old_rowid: i64,
        new_rowid: i64,
    ) where
        F: FnMut(PreUpdateEvent<'_>) + Send + 'static,
    {
        let boxed = Box::new(HookBox { call: Box::new(f) });
        let raw = Box::into_raw(boxed);
        // SAFETY: `raw` was returned by `Box::into_raw` above and is not
        // aliased anywhere else. Passing it as `user_data` to `trampoline`
        // matches the pointer type the trampoline expects, and the trailing
        // `Box::from_raw` immediately reclaims the box on the same thread.
        unsafe {
            trampoline(
                raw.cast::<c_void>(),
                ptr::null_mut(),
                op,
                db_name,
                table_name,
                old_rowid,
                new_rowid,
            );
            drop(Box::from_raw(raw));
        }
    }

    #[test]
    fn trampoline_decodes_op_and_invokes_closure() {
        let observed = Arc::new(AtomicU32::new(0));
        let sink = observed.clone();
        // SAFETY: closure only reads non-FFI fields, never derefs `self.db`.
        unsafe {
            drive_trampoline(
                move |event| {
                    assert_eq!(event.op(), PreUpdateOp::Update);
                    assert_eq!(event.database(), "main");
                    assert_eq!(event.table(), "items");
                    assert_eq!(event.old_rowid(), 3);
                    assert_eq!(event.new_rowid(), 7);
                    sink.fetch_add(1, Ordering::SeqCst);
                },
                PreUpdateOp::Update.to_raw(),
                c"main".as_ptr(),
                c"items".as_ptr(),
                3,
                7,
            );
        }
        assert_eq!(observed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn trampoline_ignores_unknown_op_codes() {
        let observed = Arc::new(AtomicU32::new(0));
        let sink = observed.clone();
        // SAFETY: closure never runs, so no field is touched.
        unsafe {
            drive_trampoline(
                move |_| {
                    sink.fetch_add(1, Ordering::SeqCst);
                },
                999,
                c"main".as_ptr(),
                c"items".as_ptr(),
                0,
                0,
            );
        }
        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn trampoline_maps_null_db_and_table_names_to_empty_strings() {
        let observed = Arc::new(AtomicU32::new(0));
        let sink = observed.clone();
        // SAFETY: closure only reads non-FFI fields.
        unsafe {
            drive_trampoline(
                move |event| {
                    assert_eq!(event.database(), "");
                    assert_eq!(event.table(), "");
                    sink.fetch_add(1, Ordering::SeqCst);
                },
                PreUpdateOp::Insert.to_raw(),
                ptr::null(),
                ptr::null(),
                0,
                1,
            );
        }
        assert_eq!(observed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn trampoline_swallows_closure_panics() {
        // SAFETY: closure panics, but `catch_unwind` inside `trampoline`
        // prevents the unwind from crossing the FFI boundary.
        unsafe {
            drive_trampoline(
                |_| panic!("boom"),
                PreUpdateOp::Insert.to_raw(),
                c"main".as_ptr(),
                c"t".as_ptr(),
                0,
                1,
            );
        }
    }

    #[test]
    fn cstr_lossy_returns_none_for_null() {
        // SAFETY: cstr_lossy accepts null explicitly.
        assert_eq!(unsafe { cstr_lossy(ptr::null()) }, None);
    }

    #[test]
    fn cstr_lossy_reads_a_valid_c_string() {
        // SAFETY: `c"hello"` is a valid, NUL-terminated C string with static
        // storage duration.
        let got = unsafe { cstr_lossy(c"hello".as_ptr()) };
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn preupdate_op_roundtrips() {
        for op in [
            PreUpdateOp::Insert,
            PreUpdateOp::Update,
            PreUpdateOp::Delete,
        ] {
            assert_eq!(PreUpdateOp::from_raw(op.to_raw()), Some(op));
        }
        assert_eq!(PreUpdateOp::from_raw(0), None);
        assert_eq!(PreUpdateOp::from_raw(999), None);
    }

    #[test]
    fn preupdate_column_type_from_raw_falls_back_to_null() {
        assert_eq!(
            PreUpdateColumnType::from_raw(super::SQLITE_INTEGER),
            PreUpdateColumnType::Integer,
        );
        assert_eq!(
            PreUpdateColumnType::from_raw(super::SQLITE_FLOAT),
            PreUpdateColumnType::Float,
        );
        assert_eq!(
            PreUpdateColumnType::from_raw(super::SQLITE_TEXT),
            PreUpdateColumnType::Text,
        );
        assert_eq!(
            PreUpdateColumnType::from_raw(super::SQLITE_BLOB),
            PreUpdateColumnType::Blob,
        );
        assert_eq!(
            PreUpdateColumnType::from_raw(super::SQLITE_NULL),
            PreUpdateColumnType::Null,
        );
        // Any unrecognized code falls back to Null, matching the private
        // helper's contract.
        assert_eq!(
            PreUpdateColumnType::from_raw(-42),
            PreUpdateColumnType::Null,
        );
    }
}
