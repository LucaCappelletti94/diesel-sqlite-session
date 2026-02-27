//! `SQLite` session management for Diesel connections.

use std::ffi::{c_int, c_void, CString};
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use diesel::internal::table_macro::{Identifier, StaticQueryFragment};
use diesel::SqliteConnection;

use crate::errors::{SessionError, SqliteErrorCode};
use crate::ffi::{
    sqlite3_free, sqlite3_session, sqlite3session_attach, sqlite3session_changeset,
    sqlite3session_create, sqlite3session_delete, sqlite3session_enable, sqlite3session_isempty,
    sqlite3session_patchset, SQLITE_OK,
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
pub struct Session {
    session: *mut sqlite3_session,
    _not_send_or_sync: PhantomData<Rc<()>>,
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
        // for the lifetime of `Session`; passing null tracks all tables per SQLite API.
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
                    // SAFETY: SQLite returned a non-null buffer with `byte_len` bytes;
                    // we copy those bytes immediately into an owned `Vec<u8>`.
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
        // SAFETY: `self.session` is owned by this type and must be released
        // exactly once with `sqlite3session_delete`.
        unsafe {
            sqlite3session_delete(self.session);
        }
    }
}
