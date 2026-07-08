#![doc = include_str!("../README.md")]
#![warn(clippy::all, clippy::pedantic, clippy::undocumented_unsafe_blocks)]
#![allow(clippy::module_name_repetitions)]

mod apply;
mod apply_v2;
mod blob;
mod changeset;
mod errors;
mod ffi;
mod preupdate;
mod session;
mod streaming;
mod transform;

pub use apply_v2::{ApplyFlags, ApplyOutcome, ConflictInfo};
pub use blob::{BlobError, BlobMode, SqliteBlob};
pub use changeset::{
    ChangesetColumnType, ChangesetError, ChangesetOp, ChangesetReader, ChangesetRow, ChangesetValue,
};
pub use errors::{ApplyError, ConflictAction, ConflictType, SessionError, SqliteErrorCode};
pub use preupdate::{
    PreUpdateColumnType, PreUpdateError, PreUpdateEvent, PreUpdateHook, PreUpdateOp, PreUpdateValue,
};
pub use session::{set_stream_size, stream_size, Session};
pub use transform::{
    concat_changesets, concat_changesets_strm, invert_changeset, invert_changeset_strm,
    Changegroup, Rebaser,
};

use diesel::SqliteConnection;

/// Extension trait adding session capabilities to [`SqliteConnection`].
/// Idiomatic entry point for creating sessions, applying changesets and
/// patchsets, opening blobs, and installing the pre-update hook.
///
/// # Example
///
/// ```
/// use diesel::prelude::*;
/// use diesel_sqlite_session::{SqliteSessionExt, ConflictAction};
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
/// // Create session using extension trait
/// let mut session = conn.create_session().unwrap();
///
/// // Type-safe table attachment
/// session.attach::<users::table>().unwrap();
///
/// // Or attach all tables
/// session.attach_all().unwrap();
///
/// // Or dynamic schema (runtime table name)
/// session.attach_by_name("dynamic_table").unwrap();
///
/// // Generate patchset and apply to another connection
/// let patchset = session.patchset().unwrap();
/// // replica.apply_patchset(&patchset, |_| ConflictAction::Abort).unwrap();
/// ```
pub trait SqliteSessionExt {
    /// Create a new session tracking changes on this connection.
    ///
    /// # Errors
    ///
    /// [`SessionError::CreateFailed`] on any `SQLite` failure.
    fn create_session(&mut self) -> Result<Session, SessionError>;

    /// Apply a changeset to this connection.
    ///
    /// `on_conflict` receives the conflict type and returns the action to
    /// take.
    ///
    /// # Errors
    ///
    /// - [`ApplyError::ApplyFailed`] on any `SQLite` failure.
    /// - [`ApplyError::ConflictAborted`] when the handler returned `Abort`.
    /// - [`ApplyError::ConflictHandlerPanicked`] when the handler panicked.
    fn apply_changeset<F>(&mut self, changeset: &[u8], on_conflict: F) -> Result<(), ApplyError>
    where
        F: Fn(ConflictType) -> ConflictAction;

    /// Apply a patchset to this connection. Patchsets carry only new values,
    /// so they are smaller than changesets but detect conflicts less
    /// precisely.
    ///
    /// `on_conflict` receives the conflict type and returns the action to
    /// take.
    ///
    /// # Example
    ///
    /// ```
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::{SqliteSessionExt, ConflictAction, ConflictType};
    ///
    /// diesel::table! {
    ///     t (id) {
    ///         id -> Integer,
    ///         v -> Integer,
    ///     }
    /// }
    ///
    /// // Create source and generate patchset
    /// let mut source = SqliteConnection::establish(":memory:").unwrap();
    /// diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)")
    ///     .execute(&mut source)
    ///     .unwrap();
    /// let mut session = source.create_session().unwrap();
    /// session.attach::<t::table>().unwrap();
    /// diesel::insert_into(t::table)
    ///     .values((t::id.eq(1), t::v.eq(100)))
    ///     .execute(&mut source)
    ///     .unwrap();
    /// let patchset = session.patchset().unwrap();
    ///
    /// // Apply with conflict handling
    /// let mut replica = SqliteConnection::establish(":memory:").unwrap();
    /// diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)")
    ///     .execute(&mut replica)
    ///     .unwrap();
    /// diesel::insert_into(t::table)
    ///     .values((t::id.eq(1), t::v.eq(999)))
    ///     .execute(&mut replica)
    ///     .unwrap();
    ///
    /// replica.apply_patchset(&patchset, |conflict_type| {
    ///     match conflict_type {
    ///         ConflictType::Data => ConflictAction::Replace,    // Overwrite
    ///         ConflictType::NotFound => ConflictAction::Omit,   // Skip
    ///         ConflictType::Conflict => ConflictAction::Replace,
    ///         _ => ConflictAction::Abort,
    ///     }
    /// }).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Same set as [`apply_changeset`](Self::apply_changeset).
    fn apply_patchset<F>(&mut self, patchset: &[u8], on_conflict: F) -> Result<(), ApplyError>
    where
        F: Fn(ConflictType) -> ConflictAction;

    /// Register a pre-update hook that fires just before every `INSERT`,
    /// `UPDATE`, or `DELETE` on a rowid table. The returned [`PreUpdateHook`]
    /// owns the registration; drop it to detach the callback.
    ///
    /// The hook fires only when `SQLite` is compiled with
    /// `SQLITE_ENABLE_PREUPDATE_HOOK`, which is the same flag the session
    /// extension needs. Mainline Diesel does not expose this hook, so this
    /// crate is the natural home for it.
    ///
    /// The callback receives a [`PreUpdateEvent<'_>`](PreUpdateEvent) bound
    /// to the callback frame. Values returned by `old_value(col)` /
    /// `new_value(col)` borrow from `SQLite`'s per-value buffers, so copy
    /// anything you need into owned types (`String`, `Vec<u8>`, `i64`)
    /// before the closure returns. `blob_write_column()` returns `Some(i)`
    /// when the event was raised by `sqlite3_blob_write`, `None` for regular
    /// DML. `depth()` is `0` at the top level and `>0` inside a trigger.
    /// Panics inside the closure are caught by the trampoline.
    ///
    /// [`PreUpdateHook`] is an RAII guard. `SQLite` allows one hook per
    /// connection, so a second `on_preupdate` while a guard is alive
    /// replaces the callback and silently retires the older guard.
    ///
    /// # Example
    ///
    /// ```
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::{PreUpdateOp, SqliteSessionExt};
    ///
    /// let mut conn = SqliteConnection::establish(":memory:").unwrap();
    /// diesel::sql_query("CREATE TABLE audit (id INTEGER PRIMARY KEY, note TEXT)")
    ///     .execute(&mut conn)
    ///     .unwrap();
    ///
    /// let hook = conn.on_preupdate(|event| match event.op() {
    ///     PreUpdateOp::Insert => {
    ///         let note = event.new_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
    ///         println!("inserted rowid {} note {:?}", event.new_rowid(), note);
    ///     }
    ///     PreUpdateOp::Update => {
    ///         let before = event.old_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
    ///         let after = event.new_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
    ///         println!("update rowid {} {:?} -> {:?}", event.old_rowid(), before, after);
    ///     }
    ///     PreUpdateOp::Delete => {
    ///         println!("delete rowid {}", event.old_rowid());
    ///     }
    /// });
    ///
    /// diesel::sql_query("INSERT INTO audit (note) VALUES ('hello')")
    ///     .execute(&mut conn)
    ///     .unwrap();
    ///
    /// // Drop the guard to detach the callback.
    /// drop(hook);
    /// ```
    fn on_preupdate<F>(&mut self, hook: F) -> PreUpdateHook
    where
        F: FnMut(PreUpdateEvent<'_>) + Send + 'static;

    /// Open an incremental blob handle over a single blob column. Writes
    /// fire the pre-update hook with `op` = [`PreUpdateOp::Delete`](crate::PreUpdateOp)
    /// and [`PreUpdateEvent::blob_write_column`] set to the column index
    /// this handle was opened on.
    ///
    /// # Errors
    ///
    /// Any [`BlobError`] variant surfaced by `sqlite3_blob_open` or argument
    /// validation.
    fn open_blob(
        &mut self,
        database: &str,
        table: &str,
        column: &str,
        rowid: i64,
        mode: BlobMode,
    ) -> Result<SqliteBlob, BlobError>;

    /// Apply a changeset via `sqlite3changeset_apply_v2`. Extends the plain
    /// [`apply_changeset`](Self::apply_changeset) with an [`ApplyFlags`]
    /// bitmask, a per-table filter, and the rebase blob emitted when
    /// conflicts are resolved via [`ConflictAction::Replace`] /
    /// [`ConflictAction::Omit`], carried in the returned [`ApplyOutcome`].
    /// The conflict callback receives a [`ConflictInfo`] view of the row.
    ///
    /// # Example
    ///
    /// ```
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::{ApplyFlags, ConflictAction, ConflictType, SqliteSessionExt};
    ///
    /// # let mut conn = SqliteConnection::establish(":memory:").unwrap();
    /// # diesel::sql_query("CREATE TABLE keep (id INTEGER PRIMARY KEY, v INTEGER)")
    /// #     .execute(&mut conn).unwrap();
    /// # diesel::sql_query("CREATE TABLE audit (id INTEGER PRIMARY KEY, v INTEGER)")
    /// #     .execute(&mut conn).unwrap();
    /// # let changeset: Vec<u8> = vec![];
    /// let outcome = conn.apply_changeset_with(
    ///     &changeset,
    ///     ApplyFlags::INVERT | ApplyFlags::IGNORENOOP,
    ///     |table| table != "audit",
    ///     |info| match info.conflict_type() {
    ///         ConflictType::Data => ConflictAction::Replace,
    ///         _ => ConflictAction::Abort,
    ///     },
    /// )?;
    /// // `outcome.rebase` carries the SQLite-emitted rebase blob when the
    /// // conflict callback resolved anything via Replace or Omit. Empty
    /// // otherwise.
    /// # Ok::<_, diesel_sqlite_session::ApplyError>(())
    /// ```
    ///
    /// The conflict callback receives a [`ConflictInfo`]: `old_value(i)`
    /// (pre-image), `new_value(i)` (post-image), `conflict_value(i)`
    /// (on-disk clashing value), plus `fk_conflicts_count()` for
    /// [`ConflictType::ForeignKey`] conflicts. All accessors are bound to
    /// the callback frame.
    ///
    /// Flags: `NOSAVEPOINT` (skip the wrapping `SAVEPOINT`), `INVERT` (apply
    /// the inverse), `IGNORENOOP` (suppress the conflict callback for
    /// `UPDATE` rows whose replica value already matches the post-image),
    /// `FKNOACTION` (skip `NO ACTION` FK handling on cascades). Compose
    /// with `|`.
    ///
    /// # Errors
    ///
    /// - [`ApplyError::ApplyFailed`] on any `SQLite` failure.
    /// - [`ApplyError::ConflictAborted`] when the handler returned `Abort`.
    /// - [`ApplyError::ConflictHandlerPanicked`] / `FilterPanicked` when the
    ///   corresponding callback panicked.
    fn apply_changeset_with<Filter, Conflict>(
        &mut self,
        changeset: &[u8],
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        Filter: Fn(&str) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction;

    /// Streamed [`apply_changeset_with`](Self::apply_changeset_with) backed
    /// by `sqlite3changeset_apply_v2_strm`. Reads the changeset from any
    /// [`std::io::Read`] in chunks.
    ///
    /// # Errors
    ///
    /// Every [`apply_changeset_with`](Self::apply_changeset_with) variant,
    /// plus [`ApplyError::ReaderIo`] and [`ApplyError::ReaderPanicked`].
    fn apply_changeset_strm_with<R, Filter, Conflict>(
        &mut self,
        reader: R,
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        R: std::io::Read,
        Filter: Fn(&str) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction;

    /// v3 apply where the filter receives the whole [`ChangesetRow`]
    /// (`sqlite3changeset_apply_v3`). Use it when the filter needs op,
    /// values, or the PK layout before deciding.
    ///
    /// # Errors
    ///
    /// Same set as [`apply_changeset_with`](Self::apply_changeset_with),
    /// under the v3 filter shape.
    ///
    /// # Example
    ///
    /// ```
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::{
    ///     ApplyFlags, ChangesetOp, ConflictAction, SqliteSessionExt,
    /// };
    ///
    /// # let mut conn = SqliteConnection::establish(":memory:").unwrap();
    /// # let changeset: Vec<u8> = vec![];
    /// let outcome = conn.apply_changeset_v3_with(
    ///     &changeset,
    ///     ApplyFlags::empty(),
    ///     |row| {
    ///         // Skip deletes on the audit table, admit everything else.
    ///         !(row.table() == "audit" && row.op() == ChangesetOp::Delete)
    ///     },
    ///     |_info| ConflictAction::Replace,
    /// )?;
    /// # Ok::<_, diesel_sqlite_session::ApplyError>(())
    /// ```
    fn apply_changeset_v3_with<Filter, Conflict>(
        &mut self,
        changeset: &[u8],
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        Filter: Fn(ChangesetRow<'_>) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction;

    /// Streamed [`apply_changeset_v3_with`](Self::apply_changeset_v3_with)
    /// backed by `sqlite3changeset_apply_v3_strm`.
    ///
    /// # Errors
    ///
    /// Same set as [`apply_changeset_v3_with`](Self::apply_changeset_v3_with)
    /// plus [`ApplyError::ReaderIo`] and [`ApplyError::ReaderPanicked`].
    fn apply_changeset_v3_strm_with<R, Filter, Conflict>(
        &mut self,
        reader: R,
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        R: std::io::Read,
        Filter: Fn(ChangesetRow<'_>) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction;
}

impl SqliteSessionExt for SqliteConnection {
    #[inline]
    fn create_session(&mut self) -> Result<Session, SessionError> {
        Session::new_internal(self)
    }

    #[inline]
    fn apply_changeset<F>(&mut self, changeset: &[u8], on_conflict: F) -> Result<(), ApplyError>
    where
        F: Fn(ConflictType) -> ConflictAction,
    {
        apply::apply_changeset(self, changeset, on_conflict)
    }

    #[inline]
    fn apply_patchset<F>(&mut self, patchset: &[u8], on_conflict: F) -> Result<(), ApplyError>
    where
        F: Fn(ConflictType) -> ConflictAction,
    {
        apply::apply_patchset(self, patchset, on_conflict)
    }

    #[inline]
    fn on_preupdate<F>(&mut self, hook: F) -> PreUpdateHook
    where
        F: FnMut(PreUpdateEvent<'_>) + Send + 'static,
    {
        PreUpdateHook::install(self, hook)
    }

    #[inline]
    fn open_blob(
        &mut self,
        database: &str,
        table: &str,
        column: &str,
        rowid: i64,
        mode: BlobMode,
    ) -> Result<SqliteBlob, BlobError> {
        SqliteBlob::open_internal(self, database, table, column, rowid, mode)
    }

    #[inline]
    fn apply_changeset_with<Filter, Conflict>(
        &mut self,
        changeset: &[u8],
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        Filter: Fn(&str) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
    {
        apply_v2::apply_changeset_with(self, changeset, flags, filter, on_conflict)
    }

    #[inline]
    fn apply_changeset_strm_with<R, Filter, Conflict>(
        &mut self,
        reader: R,
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        R: std::io::Read,
        Filter: Fn(&str) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
    {
        apply_v2::apply_changeset_strm_with(self, reader, flags, filter, on_conflict)
    }

    #[inline]
    fn apply_changeset_v3_with<Filter, Conflict>(
        &mut self,
        changeset: &[u8],
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        Filter: Fn(ChangesetRow<'_>) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
    {
        apply_v2::apply_changeset_v3_with(self, changeset, flags, filter, on_conflict)
    }

    #[inline]
    fn apply_changeset_v3_strm_with<R, Filter, Conflict>(
        &mut self,
        reader: R,
        flags: ApplyFlags,
        filter: Filter,
        on_conflict: Conflict,
    ) -> Result<ApplyOutcome, ApplyError>
    where
        R: std::io::Read,
        Filter: Fn(ChangesetRow<'_>) -> bool,
        Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
    {
        apply_v2::apply_changeset_v3_strm_with(self, reader, flags, filter, on_conflict)
    }
}
