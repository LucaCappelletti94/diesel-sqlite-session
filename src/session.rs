//! `SQLite` session management for Diesel connections.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;

use diesel::internal::table_macro::{Identifier, StaticQueryFragment};
use diesel::SqliteConnection;

use crate::errors::{SessionError, SqliteErrorCode};
use crate::ffi::{
    sqlite3_free, sqlite3_session, sqlite3session_attach, sqlite3session_changeset,
    sqlite3session_changeset_size, sqlite3session_changeset_strm, sqlite3session_config,
    sqlite3session_create, sqlite3session_delete, sqlite3session_diff, sqlite3session_enable,
    sqlite3session_indirect, sqlite3session_isempty, sqlite3session_memory_used,
    sqlite3session_object_config, sqlite3session_patchset, sqlite3session_patchset_strm,
    sqlite3session_table_filter, SQLITE_OK, SQLITE_SESSION_CONFIG_STRMSIZE,
    SQLITE_SESSION_OBJCONFIG_ROWID, SQLITE_SESSION_OBJCONFIG_SIZE,
};

/// A session tracking changes on a Diesel `SQLite` connection.
///
/// Sessions allow you to track changes made to the database and generate
/// changesets or patchsets that can be applied to other databases.
///
/// # Safety
///
/// The session internally holds a raw pointer to the `SQLite` database handle.
/// You must ensure that the session is dropped before the connection it was
/// created from. Using a session after its connection has been dropped is
/// undefined behavior.
///
/// # Threading
///
/// `Session` is intentionally neither [`Send`] nor [`Sync`]. Session handles
/// are bound to `SQLite` connection state and must stay on the originating thread.
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::Session;
/// assert_send::<Session>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::Session;
/// assert_sync::<Session>();
/// ```
///
/// # Example
///
/// ```no_run
/// use diesel::prelude::*;
/// use diesel_sqlite_session::SqliteSessionExt;
///
/// diesel::table! {
///     users (id) {
///         id -> Integer,
///         name -> Text,
///     }
/// }
///
/// let mut conn = SqliteConnection::establish(":memory:").unwrap();
///
/// diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
///     .execute(&mut conn)
///     .unwrap();
///
/// let mut session = conn.create_session().unwrap();
/// session.attach::<users::table>().unwrap();
///
/// diesel::sql_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
///     .execute(&mut conn)
///     .unwrap();
///
/// let patchset = session.patchset().unwrap();
/// assert!(!patchset.is_empty());
/// ```
#[allow(clippy::struct_field_names)]
pub struct Session {
    session: *mut sqlite3_session,
    /// Owned closure kept alive while the filter is registered. Double-boxed
    /// so `pCtx` gets a stable heap address that survives moves of `Session`.
    table_filter: Option<Box<FilterBox>>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

struct FilterBox {
    call: Box<dyn FnMut(&str) -> bool + Send>,
}

type SessionExportFn =
    unsafe extern "C" fn(*mut sqlite3_session, *mut c_int, *mut *mut c_void) -> c_int;
const MAIN_DB_NAME: &std::ffi::CStr = c"main";

impl Session {
    /// Internal constructor - called by `SqliteSessionExt::create_session`.
    ///
    /// The session will track changes made to the "main" database.
    ///
    /// # Safety
    ///
    /// The returned session holds a raw pointer to the connection's `SQLite` handle.
    /// You must ensure the session is dropped before the connection.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::CreateFailed` if `SQLite` fails to create the session.
    pub(crate) fn new_internal(conn: &mut SqliteConnection) -> Result<Self, SessionError> {
        // SAFETY: `with_raw_connection` provides a valid SQLite handle for the duration
        // of the callback, and `MAIN_DB_NAME` is a static NUL-terminated C string.
        let session = unsafe {
            conn.with_raw_connection(|raw| {
                let mut session: *mut sqlite3_session = ptr::null_mut();
                let rc = sqlite3session_create(raw, MAIN_DB_NAME.as_ptr(), &mut session);
                if rc != SQLITE_OK {
                    return Err(SessionError::CreateFailed(SqliteErrorCode::from_error(rc)));
                }
                Ok(session)
            })
        }?;

        Ok(Self {
            session,
            table_filter: None,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Attach a table to track using a Diesel table type.
    ///
    /// This provides type-safe table attachment using Diesel's table macro types.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::SqliteSessionExt;
    ///
    /// diesel::table! {
    ///     users (id) {
    ///         id -> Integer,
    ///         name -> Text,
    ///     }
    /// }
    ///
    /// let mut conn = SqliteConnection::establish(":memory:").unwrap();
    /// let mut session = conn.create_session().unwrap();
    /// session.attach::<users::table>().unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `SessionError::AttachFailed` if `SQLite` fails to attach the table.
    pub fn attach<T>(&mut self) -> Result<(), SessionError>
    where
        T: StaticQueryFragment<Component = Identifier<'static>>,
    {
        let table_name: &'static str = T::STATIC_COMPONENT.0;
        self.attach_by_name(table_name)
    }

    /// Attach ALL tables to track.
    ///
    /// This will track changes to any table in the database.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::AttachFailed` if `SQLite` fails to attach.
    pub fn attach_all(&mut self) -> Result<(), SessionError> {
        // SAFETY: `self.session` is created by `sqlite3session_create` and remains valid
        // for the lifetime of `Session`. Passing null tracks all tables per SQLite API.
        let rc = unsafe { sqlite3session_attach(self.session, ptr::null()) };

        if rc != SQLITE_OK {
            return Err(SessionError::AttachFailed(SqliteErrorCode::from_error(rc)));
        }

        Ok(())
    }

    /// Attach a table by name.
    ///
    /// Use this for dynamic schemas where the table name is determined at runtime.
    /// For static table names, prefer [`attach`](Self::attach) with a Diesel table type.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::InvalidTableName` if the table name contains a null byte.
    /// Returns `SessionError::AttachFailed` if `SQLite` fails to attach the table.
    pub fn attach_by_name(&mut self, table: &str) -> Result<(), SessionError> {
        let c_name = CString::new(table).map_err(|_| SessionError::InvalidTableName)?;
        // SAFETY: `self.session` is a live session handle and `c_name` is a valid
        // NUL-terminated table name for the duration of this call.
        let rc = unsafe { sqlite3session_attach(self.session, c_name.as_ptr()) };

        if rc != SQLITE_OK {
            return Err(SessionError::AttachFailed(SqliteErrorCode::from_error(rc)));
        }

        Ok(())
    }

    /// Generate a changeset from tracked changes.
    ///
    /// A changeset contains all information needed to recreate the changes,
    /// including both old and new values for updated rows.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::ChangesetFailed` if `SQLite` fails to generate the changeset.
    pub fn changeset(&mut self) -> Result<Vec<u8>, SessionError> {
        self.export_changes(sqlite3session_changeset, SessionError::ChangesetFailed)
    }

    /// Generate a patchset from tracked changes.
    ///
    /// A patchset is similar to a changeset but only contains the primary key
    /// and new values for updated rows (not the old values). Patchsets are
    /// smaller but cannot detect conflicts as precisely.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::PatchsetFailed` if `SQLite` fails to generate the patchset.
    pub fn patchset(&mut self) -> Result<Vec<u8>, SessionError> {
        self.export_changes(sqlite3session_patchset, SessionError::PatchsetFailed)
    }

    /// Stream a changeset into `writer` (`sqlite3session_changeset_strm`).
    /// Lets callers pipe the bytes SQLite would otherwise pack into an owned
    /// buffer through a `File`, `TcpStream`, or any other writer.
    ///
    /// # Errors
    ///
    /// [`SessionError::ChangesetFailed`] on any `SQLite` failure, including
    /// `SQLITE_IOERR` from the trampoline when the writer errored or panicked.
    pub fn changeset_strm<W>(&mut self, writer: W) -> Result<(), SessionError>
    where
        W: std::io::Write,
    {
        self.export_strm(
            writer,
            sqlite3session_changeset_strm,
            SessionError::ChangesetFailed,
        )
    }

    /// Stream a patchset into `writer` (`sqlite3session_patchset_strm`).
    ///
    /// # Errors
    ///
    /// [`SessionError::PatchsetFailed`] on any `SQLite` failure, including
    /// `SQLITE_IOERR` from the trampoline when the writer errored or panicked.
    pub fn patchset_strm<W>(&mut self, writer: W) -> Result<(), SessionError>
    where
        W: std::io::Write,
    {
        self.export_strm(
            writer,
            sqlite3session_patchset_strm,
            SessionError::PatchsetFailed,
        )
    }

    fn export_strm<W>(
        &mut self,
        writer: W,
        export_fn: unsafe extern "C" fn(
            *mut sqlite3_session,
            Option<
                unsafe extern "C" fn(
                    *mut std::ffi::c_void,
                    *const std::ffi::c_void,
                    std::ffi::c_int,
                ) -> std::ffi::c_int,
            >,
            *mut std::ffi::c_void,
        ) -> std::ffi::c_int,
        map_error: fn(SqliteErrorCode) -> SessionError,
    ) -> Result<(), SessionError>
    where
        W: std::io::Write,
    {
        let mut ctx = crate::streaming::OutputContext::new(writer);
        let ptr = std::ptr::addr_of_mut!(ctx).cast::<std::ffi::c_void>();
        // SAFETY: `self.session` outlives the call, and `ctx` lives on this
        // stack frame so `ptr` is stable throughout.
        let rc = unsafe {
            export_fn(
                self.session,
                Some(crate::streaming::write_trampoline::<W>),
                ptr,
            )
        };
        if let Some(err) = ctx.error.take() {
            return Err(SessionError::WriterIo(err));
        }
        if ctx.panicked {
            return Err(SessionError::WriterPanicked);
        }
        if rc != SQLITE_OK {
            return Err(map_error(SqliteErrorCode::from_error(rc)));
        }
        Ok(())
    }

    /// Check if the session has recorded any changes.
    ///
    /// Returns `true` if no changes have been recorded, `false` otherwise.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // SAFETY: `self.session` is a valid handle owned by this `Session`.
        unsafe { sqlite3session_isempty(self.session) != 0 }
    }

    /// Enable or disable change tracking.
    ///
    /// When disabled, changes are not recorded. This can be useful for
    /// temporarily suspending tracking during bulk operations.
    #[inline]
    pub fn set_enabled(&mut self, enabled: bool) {
        // SAFETY: `self.session` is a valid handle owned by this `Session`.
        unsafe {
            sqlite3session_enable(self.session, i32::from(enabled));
        }
    }

    /// Populate this session with the delta between `table` in
    /// `from_database` and the same-named table on the connection's own
    /// database (`sqlite3session_diff`). The filter installed by
    /// [`set_table_filter`](Self::set_table_filter), if any, is consulted.
    ///
    /// # Errors
    ///
    /// - [`SessionError::InvalidTableName`] if either name contains a null byte.
    /// - [`SessionError::DiffFailed`] on any `SQLite`-reported error, carrying
    ///   any message SQLite wrote into `pzErrMsg`.
    pub fn diff(&mut self, from_database: &str, table: &str) -> Result<(), SessionError> {
        let c_from = CString::new(from_database).map_err(|_| SessionError::InvalidTableName)?;
        let c_table = CString::new(table).map_err(|_| SessionError::InvalidTableName)?;
        let mut err_msg: *mut c_char = ptr::null_mut();
        // SAFETY: `self.session` is a live handle, both `CString`s outlive the
        // call, and `err_msg` is a valid out-pointer.
        let rc = unsafe {
            sqlite3session_diff(
                self.session,
                c_from.as_ptr(),
                c_table.as_ptr(),
                &mut err_msg,
            )
        };
        let message = if err_msg.is_null() {
            None
        } else {
            // SAFETY: `err_msg` is a NUL-terminated C string sqlite_malloc'd.
            let owned = unsafe { CStr::from_ptr(err_msg) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: `sqlite3_malloc`-allocated buffer.
            unsafe { sqlite3_free(err_msg.cast::<c_void>()) };
            Some(owned)
        };
        if rc != SQLITE_OK {
            return Err(SessionError::DiffFailed {
                code: SqliteErrorCode::from_error(rc),
                message,
            });
        }
        Ok(())
    }

    /// Set the "indirect" flag: subsequent changes are tagged as indirect
    /// (`sqlite3session_indirect`; read back via
    /// [`ChangesetRow::indirect`](crate::ChangesetRow::indirect)).
    pub fn set_indirect(&mut self, indirect: bool) {
        // SAFETY: `self.session` is a live handle.
        unsafe {
            sqlite3session_indirect(self.session, i32::from(indirect));
        }
    }

    /// Read the current "indirect" flag without changing it.
    #[must_use]
    pub fn is_indirect(&self) -> bool {
        // Passing -1 queries the current flag without changing it.
        // SAFETY: `self.session` is a live handle.
        unsafe { sqlite3session_indirect(self.session, -1) != 0 }
    }

    /// Register a filter consulted before each auto-attached table
    /// (`sqlite3session_table_filter`). Called from
    /// [`attach_all`](Self::attach_all) and [`diff`](Self::diff); return
    /// `true` to track the table, `false` to skip. Explicit
    /// [`attach`](Self::attach) / [`attach_by_name`](Self::attach_by_name)
    /// bypass the filter. Installing a new filter replaces the previous one;
    /// panics inside the callback are caught by the trampoline and reported
    /// to `SQLite` as "skip this table".
    pub fn set_table_filter<F>(&mut self, filter: F)
    where
        F: FnMut(&str) -> bool + Send + 'static,
    {
        let boxed = Box::new(FilterBox {
            call: Box::new(filter),
        });
        let ptr: *mut c_void = ptr::addr_of!(*boxed).cast::<c_void>().cast_mut();
        // SAFETY: `self.session` outlives the call; `boxed` is stored on
        // `self.table_filter` before returning, so `ptr` stays valid. The
        // old Box is dropped only after SQLite has switched callbacks.
        unsafe {
            sqlite3session_table_filter(self.session, Some(filter_trampoline), ptr);
        }
        self.table_filter = Some(boxed);
    }

    /// Remove any table filter previously installed with
    /// [`set_table_filter`](Self::set_table_filter).
    pub fn remove_table_filter(&mut self) {
        // SAFETY: `self.session` is a live handle.
        unsafe {
            sqlite3session_table_filter(self.session, None, ptr::null_mut());
        }
        self.table_filter = None;
    }

    /// Enable or disable per-session size tracking
    /// (`SQLITE_SESSION_OBJCONFIG_SIZE`). Required before
    /// [`changeset_size`](Self::changeset_size) reports non-zero.
    ///
    /// # Errors
    ///
    /// [`SessionError::ObjectConfigFailed`] if `SQLite` refuses the option.
    pub fn set_size_tracking(&mut self, enabled: bool) -> Result<(), SessionError> {
        let mut val: c_int = i32::from(enabled);
        object_config(self.session, SQLITE_SESSION_OBJCONFIG_SIZE, &mut val)
    }

    /// Read the current size-tracking setting.
    ///
    /// # Errors
    ///
    /// [`SessionError::ObjectConfigFailed`] if `SQLite` refuses the option.
    pub fn is_size_tracking_enabled(&self) -> Result<bool, SessionError> {
        let mut val: c_int = -1;
        object_config(self.session, SQLITE_SESSION_OBJCONFIG_SIZE, &mut val)?;
        Ok(val != 0)
    }

    /// Enable or disable tracking of `WITHOUT ROWID` tables
    /// (`SQLITE_SESSION_OBJCONFIG_ROWID`).
    ///
    /// # Errors
    ///
    /// [`SessionError::ObjectConfigFailed`] if `SQLite` refuses the option.
    pub fn set_rowid_tracking(&mut self, enabled: bool) -> Result<(), SessionError> {
        let mut val: c_int = i32::from(enabled);
        object_config(self.session, SQLITE_SESSION_OBJCONFIG_ROWID, &mut val)
    }

    /// Read the current `WITHOUT ROWID` tracking setting.
    ///
    /// # Errors
    ///
    /// [`SessionError::ObjectConfigFailed`] if `SQLite` refuses the option.
    pub fn is_rowid_tracking_enabled(&self) -> Result<bool, SessionError> {
        let mut val: c_int = -1;
        object_config(self.session, SQLITE_SESSION_OBJCONFIG_ROWID, &mut val)?;
        Ok(val != 0)
    }

    /// Bytes of memory currently held by this session's change log
    /// (`sqlite3session_memory_used`).
    #[must_use]
    pub fn memory_used(&self) -> u64 {
        // SAFETY: `self.session` is a live handle.
        let n = unsafe { sqlite3session_memory_used(self.session) };
        u64::try_from(n).unwrap_or(0)
    }

    /// Estimated bytes the changeset for this session would occupy
    /// (`sqlite3session_changeset_size`). Accurate only after
    /// [`set_size_tracking(true)`](Self::set_size_tracking).
    #[must_use]
    pub fn changeset_size(&self) -> u64 {
        // SAFETY: `self.session` is a live handle.
        let n = unsafe { sqlite3session_changeset_size(self.session) };
        u64::try_from(n).unwrap_or(0)
    }

    fn export_changes(
        &mut self,
        export_fn: SessionExportFn,
        map_error: fn(SqliteErrorCode) -> SessionError,
    ) -> Result<Vec<u8>, SessionError> {
        let mut size: c_int = 0;
        let mut buffer: *mut c_void = ptr::null_mut();

        // SAFETY: `export_fn` is one of SQLite's session export functions and
        // receives valid out-pointers to write size and buffer.
        let rc = unsafe { export_fn(self.session, &mut size, &mut buffer) };
        if rc != SQLITE_OK {
            return Err(map_error(SqliteErrorCode::from_error(rc)));
        }

        let result = if size <= 0 || buffer.is_null() {
            Ok(Vec::new())
        } else {
            usize::try_from(size)
                .map_err(|_| map_error(SqliteErrorCode::Unknown(size)))
                .map(|byte_len| {
                    // SAFETY: SQLite returned a non-null buffer with `byte_len` bytes,
                    // and we copy those bytes immediately into an owned `Vec<u8>`.
                    let bytes =
                        unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), byte_len) };
                    bytes.to_vec()
                })
        };

        if !buffer.is_null() {
            // SAFETY: SQLite allocates export buffers with sqlite3_malloc-family APIs
            // and requires release via `sqlite3_free`.
            unsafe { sqlite3_free(buffer) };
        }

        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Unhook the filter first so no callback fires against a half-dead
        // handle. Matches the pattern used elsewhere in this crate.
        // SAFETY: `self.session` is a live handle owned by this `Session`.
        unsafe {
            sqlite3session_table_filter(self.session, None, ptr::null_mut());
        }
        // SAFETY: `self.session` must be released exactly once via
        // `sqlite3session_delete`.
        unsafe {
            sqlite3session_delete(self.session);
        }
    }
}

/// Change the module-wide default streaming chunk size for streamed
/// changeset APIs (`sqlite3session_config` + `SQLITE_SESSION_CONFIG_STRMSIZE`).
/// Only `size > 0` is applied; other values are treated as a query by SQLite.
///
/// # Errors
///
/// [`SessionError::ConfigFailed`] if `SQLite` refuses the option.
pub fn set_stream_size(size: i32) -> Result<(), SessionError> {
    let mut val: c_int = size;
    config(SQLITE_SESSION_CONFIG_STRMSIZE, &mut val)
}

/// Read the current module-level default streaming chunk size.
///
/// # Errors
///
/// [`SessionError::ConfigFailed`] if `SQLite` refuses the option.
pub fn stream_size() -> Result<i32, SessionError> {
    let mut val: c_int = 0;
    config(SQLITE_SESSION_CONFIG_STRMSIZE, &mut val)?;
    Ok(val)
}

/// Shared body for `sqlite3session_config` calls.
fn config(op: c_int, val: &mut c_int) -> Result<(), SessionError> {
    // SAFETY: `sqlite3session_config` treats the pointer as an `int*` for the
    // duration of the call.
    let rc = unsafe { sqlite3session_config(op, ptr::addr_of_mut!(*val).cast::<c_void>()) };
    if rc != SQLITE_OK {
        return Err(SessionError::ConfigFailed(SqliteErrorCode::from_error(rc)));
    }
    Ok(())
}

/// Shared body for `sqlite3session_object_config` setters and getters.
fn object_config(
    session: *mut sqlite3_session,
    op: c_int,
    val: &mut c_int,
) -> Result<(), SessionError> {
    // SAFETY: `session` is a live handle and `val` is a valid pointer to an
    // `int` for the duration of the call.
    let rc = unsafe {
        sqlite3session_object_config(session, op, ptr::addr_of_mut!(*val).cast::<c_void>())
    };
    if rc != SQLITE_OK {
        return Err(SessionError::ObjectConfigFailed(
            SqliteErrorCode::from_error(rc),
        ));
    }
    Ok(())
}

/// C trampoline for `sqlite3session_table_filter`.
///
/// # Safety
///
/// `ctx` must point to a live `FilterBox` and `table_ptr` must either be null
/// or a valid C string for the duration of the call.
unsafe extern "C" fn filter_trampoline(ctx: *mut c_void, table_ptr: *const c_char) -> c_int {
    // SAFETY: `ctx` is the pointer we handed to `sqlite3session_table_filter`,
    // pointing at a `FilterBox` we still own.
    let filter = unsafe { &mut *(ctx.cast::<FilterBox>()) };
    let table = if table_ptr.is_null() {
        ""
    } else {
        // SAFETY: non-null `table_ptr` is a valid C string per the FFI contract.
        unsafe { CStr::from_ptr(table_ptr) }.to_str().unwrap_or("")
    };
    match catch_unwind(AssertUnwindSafe(|| (filter.call)(table))) {
        Ok(true) => 1,
        Ok(false) | Err(_) => 0,
    }
}
