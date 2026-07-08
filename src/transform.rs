//! Changeset transform helpers: `invert` (undo), `concat` (pairwise merge),
//! and the `Changegroup` n-way merge. Wraps `sqlite3changeset_invert`,
//! `sqlite3changeset_concat`, and the `sqlite3changegroup_*` family, plus the
//! `sqlite3rebaser_*` bindings used by [`Rebaser`].

use std::ffi::{c_int, c_void, CString};
use std::marker::PhantomData;
use std::ptr;

use diesel::SqliteConnection;

use crate::changeset::{ChangesetError, ChangesetRow};
use crate::errors::SqliteErrorCode;
use crate::ffi::{
    sqlite3_changegroup, sqlite3_free, sqlite3_rebaser, sqlite3changegroup_add,
    sqlite3changegroup_add_change, sqlite3changegroup_add_strm, sqlite3changegroup_delete,
    sqlite3changegroup_new, sqlite3changegroup_output, sqlite3changegroup_output_strm,
    sqlite3changegroup_schema, sqlite3changeset_concat, sqlite3changeset_concat_strm,
    sqlite3changeset_invert, sqlite3changeset_invert_strm, sqlite3rebaser_configure,
    sqlite3rebaser_create, sqlite3rebaser_delete, sqlite3rebaser_rebase,
    sqlite3rebaser_rebase_strm, SQLITE_OK,
};

/// Produce the inverse of `bytes` (`sqlite3changeset_invert`). Every `INSERT`
/// becomes a `DELETE` and vice versa; `UPDATE` rows swap old and new.
///
/// # Errors
///
/// - [`ChangesetError::EmptyChangeset`] if `bytes` is empty.
/// - [`ChangesetError::LengthOverflow`] if `bytes.len()` overflows `i32`.
/// - [`ChangesetError::InvertFailed`] if `SQLite` reports an error.
pub fn invert_changeset(bytes: &[u8]) -> Result<Vec<u8>, ChangesetError> {
    if bytes.is_empty() {
        return Err(ChangesetError::EmptyChangeset);
    }
    let len = c_int::try_from(bytes.len())
        .map_err(|_| ChangesetError::LengthOverflow { value: bytes.len() })?;

    let mut out_ptr: *mut c_void = ptr::null_mut();
    let mut out_len: c_int = 0;
    // SAFETY: `sqlite3changeset_invert` treats `bytes` as read-only and
    // allocates the output through `sqlite3_malloc`.
    let rc = unsafe {
        sqlite3changeset_invert(
            len,
            bytes.as_ptr().cast::<c_void>(),
            &mut out_len,
            &mut out_ptr,
        )
    };
    if rc != SQLITE_OK {
        if !out_ptr.is_null() {
            // SAFETY: `sqlite3_malloc`-allocated buffer.
            unsafe { sqlite3_free(out_ptr) };
        }
        return Err(ChangesetError::InvertFailed(SqliteErrorCode::from_error(
            rc,
        )));
    }
    let owned = copy_and_free(out_ptr, out_len);
    Ok(owned)
}

/// Concatenate two changesets over the same schema
/// (`sqlite3changeset_concat`). The result is the deterministic merge of
/// `a` followed by `b`.
///
/// # Errors
///
/// - [`ChangesetError::EmptyChangeset`] if either input is empty.
/// - [`ChangesetError::LengthOverflow`] if either length overflows `i32`.
/// - [`ChangesetError::ConcatFailed`] if `SQLite` reports an error, typically
///   a schema mismatch between the two changesets.
pub fn concat_changesets(a: &[u8], b: &[u8]) -> Result<Vec<u8>, ChangesetError> {
    if a.is_empty() || b.is_empty() {
        return Err(ChangesetError::EmptyChangeset);
    }
    let n_a =
        c_int::try_from(a.len()).map_err(|_| ChangesetError::LengthOverflow { value: a.len() })?;
    let n_b =
        c_int::try_from(b.len()).map_err(|_| ChangesetError::LengthOverflow { value: b.len() })?;

    let mut out_ptr: *mut c_void = ptr::null_mut();
    let mut out_len: c_int = 0;
    // SAFETY: `sqlite3changeset_concat` treats both inputs as read-only and
    // allocates the output through `sqlite3_malloc`.
    let rc = unsafe {
        sqlite3changeset_concat(
            n_a,
            a.as_ptr().cast::<c_void>().cast_mut(),
            n_b,
            b.as_ptr().cast::<c_void>().cast_mut(),
            &mut out_len,
            &mut out_ptr,
        )
    };
    if rc != SQLITE_OK {
        if !out_ptr.is_null() {
            // SAFETY: sqlite_malloc-allocated buffer.
            unsafe { sqlite3_free(out_ptr) };
        }
        return Err(ChangesetError::ConcatFailed(SqliteErrorCode::from_error(
            rc,
        )));
    }
    Ok(copy_and_free(out_ptr, out_len))
}

/// Streamed [`invert_changeset`] backed by `sqlite3changeset_invert_strm`.
/// `SQLite` pulls from `reader` and pushes to `writer` in chunks.
///
/// # Errors
///
/// - [`ChangesetError::ReaderIo`] when `reader` returns an [`std::io::Error`].
/// - [`ChangesetError::ReaderPanicked`] when the reader panics.
/// - [`ChangesetError::WriterIo`] when `writer` returns an [`std::io::Error`].
/// - [`ChangesetError::WriterPanicked`] when the writer panics.
/// - [`ChangesetError::InvertFailed`] on any other `SQLite`-reported error.
pub fn invert_changeset_strm<R, W>(reader: R, writer: W) -> Result<(), ChangesetError>
where
    R: std::io::Read,
    W: std::io::Write,
{
    let mut input_ctx = crate::streaming::InputContext::new(reader);
    let mut output_ctx = crate::streaming::OutputContext::new(writer);
    // SAFETY: both contexts live on this stack frame for the whole call.
    let rc = unsafe {
        sqlite3changeset_invert_strm(
            Some(crate::streaming::read_trampoline::<R>),
            ptr::addr_of_mut!(input_ctx).cast::<c_void>(),
            Some(crate::streaming::write_trampoline::<W>),
            ptr::addr_of_mut!(output_ctx).cast::<c_void>(),
        )
    };
    surface_stream_errors_invert(input_ctx, output_ctx, rc)
}

fn surface_stream_errors_invert<R, W>(
    mut input_ctx: crate::streaming::InputContext<R>,
    mut output_ctx: crate::streaming::OutputContext<W>,
    rc: c_int,
) -> Result<(), ChangesetError>
where
    R: std::io::Read,
    W: std::io::Write,
{
    if let Some(err) = input_ctx.error.take() {
        return Err(ChangesetError::ReaderIo(err));
    }
    if input_ctx.panicked {
        return Err(ChangesetError::ReaderPanicked);
    }
    if let Some(err) = output_ctx.error.take() {
        return Err(ChangesetError::WriterIo(err));
    }
    if output_ctx.panicked {
        return Err(ChangesetError::WriterPanicked);
    }
    if rc != SQLITE_OK {
        return Err(ChangesetError::InvertFailed(SqliteErrorCode::from_error(
            rc,
        )));
    }
    Ok(())
}

/// Streamed [`concat_changesets`] backed by `sqlite3changeset_concat_strm`.
/// Merges the changesets read from `reader_a` and `reader_b` into `writer`.
///
/// # Errors
///
/// Same as [`invert_changeset_strm`], except the `SQLite`-reported failure
/// maps to [`ChangesetError::ConcatFailed`].
pub fn concat_changesets_strm<A, B, W>(
    reader_a: A,
    reader_b: B,
    writer: W,
) -> Result<(), ChangesetError>
where
    A: std::io::Read,
    B: std::io::Read,
    W: std::io::Write,
{
    let mut in_a = crate::streaming::InputContext::new(reader_a);
    let mut in_b = crate::streaming::InputContext::new(reader_b);
    let mut out = crate::streaming::OutputContext::new(writer);
    // SAFETY: all three contexts live on this stack frame for the whole call.
    let rc = unsafe {
        sqlite3changeset_concat_strm(
            Some(crate::streaming::read_trampoline::<A>),
            ptr::addr_of_mut!(in_a).cast::<c_void>(),
            Some(crate::streaming::read_trampoline::<B>),
            ptr::addr_of_mut!(in_b).cast::<c_void>(),
            Some(crate::streaming::write_trampoline::<W>),
            ptr::addr_of_mut!(out).cast::<c_void>(),
        )
    };
    if let Some(err) = in_a.error.take().or_else(|| in_b.error.take()) {
        return Err(ChangesetError::ReaderIo(err));
    }
    if in_a.panicked || in_b.panicked {
        return Err(ChangesetError::ReaderPanicked);
    }
    if let Some(err) = out.error.take() {
        return Err(ChangesetError::WriterIo(err));
    }
    if out.panicked {
        return Err(ChangesetError::WriterPanicked);
    }
    if rc != SQLITE_OK {
        return Err(ChangesetError::ConcatFailed(SqliteErrorCode::from_error(
            rc,
        )));
    }
    Ok(())
}

/// N-way merge of changesets over the same schema (`sqlite3changegroup_*`).
/// Duplicate ops on the same primary key are collapsed (e.g. `INSERT` then
/// `UPDATE` on the same key becomes one `INSERT` with the final values).
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::Changegroup;
/// assert_send::<Changegroup>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::Changegroup;
/// assert_sync::<Changegroup>();
/// ```
pub struct Changegroup {
    ptr: *mut sqlite3_changegroup,
    _not_send_or_sync: PhantomData<*const ()>,
}

impl Changegroup {
    /// Create an empty changegroup.
    ///
    /// # Errors
    ///
    /// [`ChangesetError::ChangegroupCreateFailed`] on allocation failure.
    pub fn new() -> Result<Self, ChangesetError> {
        let mut ptr: *mut sqlite3_changegroup = ptr::null_mut();
        // SAFETY: `sqlite3changegroup_new` writes an owned allocation into
        // `ptr` and returns `SQLITE_OK` on success.
        let rc = unsafe { sqlite3changegroup_new(&mut ptr) };
        if rc != SQLITE_OK {
            if !ptr.is_null() {
                // SAFETY: hand back partial allocation for freeing.
                unsafe { sqlite3changegroup_delete(ptr) };
            }
            return Err(ChangesetError::ChangegroupCreateFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        if ptr.is_null() {
            return Err(ChangesetError::ChangegroupCreateFailed(
                SqliteErrorCode::Error,
            ));
        }
        Ok(Self {
            ptr,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Attach a schema so the group understands per-table PKs. Required
    /// before folding in `WITHOUT ROWID` tables or reconciling column types.
    /// Plain rowid changesets fold in without a schema.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::InvalidSchemaName`] if `database` contains a null
    ///   byte.
    /// - [`ChangesetError::ChangegroupSchemaFailed`] if `SQLite` reports an
    ///   error.
    pub fn set_schema(
        &mut self,
        conn: &mut SqliteConnection,
        database: &str,
    ) -> Result<(), ChangesetError> {
        let c_name = CString::new(database).map_err(|_| ChangesetError::InvalidSchemaName)?;
        // SAFETY: `self.ptr` is a live changegroup; `c_name` outlives the
        // call from this stack frame.
        let rc = unsafe {
            conn.with_raw_connection(|raw| {
                sqlite3changegroup_schema(self.ptr, raw, c_name.as_ptr())
            })
        };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ChangegroupSchemaFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }

    /// Fold `changeset` into the group.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::EmptyChangeset`] if `changeset` is empty.
    /// - [`ChangesetError::LengthOverflow`] if `changeset.len()` overflows
    ///   `i32`.
    /// - [`ChangesetError::ChangegroupAddFailed`] if `SQLite` reports an
    ///   error (typically a schema mismatch with previously-added changesets).
    pub fn add(&mut self, changeset: &[u8]) -> Result<(), ChangesetError> {
        if changeset.is_empty() {
            return Err(ChangesetError::EmptyChangeset);
        }
        let len = c_int::try_from(changeset.len()).map_err(|_| ChangesetError::LengthOverflow {
            value: changeset.len(),
        })?;
        // SAFETY: `sqlite3changegroup_add` treats `changeset` as read-only.
        let rc = unsafe {
            sqlite3changegroup_add(
                self.ptr,
                len,
                changeset.as_ptr().cast::<c_void>().cast_mut(),
            )
        };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ChangegroupAddFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }

    /// Fold a single positioned row from a
    /// [`ChangesetReader`](crate::ChangesetReader) into the group
    /// (`sqlite3changegroup_add_change`). The per-op counterpart to
    /// [`add`](Self::add): step through a changeset and hand only the rows
    /// you want to the group.
    ///
    /// ```
    /// use diesel::prelude::*;
    /// use diesel_sqlite_session::{Changegroup, ChangesetReader, SqliteSessionExt};
    ///
    /// # let mut conn = SqliteConnection::establish(":memory:").unwrap();
    /// # diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
    /// #     .execute(&mut conn).unwrap();
    /// # let mut session = conn.create_session().unwrap();
    /// # session.attach_all().unwrap();
    /// # diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 10), (2, 20), (3, 30)")
    /// #     .execute(&mut conn).unwrap();
    /// # let bytes = session.changeset().unwrap();
    /// # drop(session);
    /// // Fold only rows with an odd id into the group.
    /// let mut group = Changegroup::new()?;
    /// let mut reader = ChangesetReader::open(&bytes)?;
    /// while let Some(row) = reader.next()? {
    ///     let id = row.new_value(0)?.unwrap().as_i64();
    ///     if id % 2 == 1 {
    ///         group.add_change(&row)?;
    ///     }
    /// }
    /// let merged = group.output()?;
    /// # assert!(!merged.is_empty());
    /// # Ok::<_, diesel_sqlite_session::ChangesetError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::ChangegroupAddFailed`] on any `SQLite`-reported
    ///   error. `SQLITE_ERROR` here means the iterator was opened inverted
    ///   or `sqlite3changegroup_add_change` refused the change (invalid
    ///   position, or schema mismatch when no `set_schema` was configured).
    pub fn add_change(&mut self, row: &ChangesetRow<'_>) -> Result<(), ChangesetError> {
        // SAFETY: `self.ptr` is a live changegroup; `row.as_raw_iter()`
        // returns the iterator that produced `row` in `ChangesetReader::next`,
        // which only hands out rows on a valid entry.
        let rc = unsafe { sqlite3changegroup_add_change(self.ptr, row.as_raw_iter()) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ChangegroupAddFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }

    /// Serialize the merged changeset. Idempotent: calling `output` twice
    /// yields the same buffer.
    ///
    /// # Errors
    ///
    /// [`ChangesetError::ChangegroupOutputFailed`] if `SQLite` reports an
    /// error.
    pub fn output(&mut self) -> Result<Vec<u8>, ChangesetError> {
        let mut out_ptr: *mut c_void = ptr::null_mut();
        let mut out_len: c_int = 0;
        // SAFETY: `sqlite3changegroup_output` allocates via `sqlite3_malloc`.
        let rc = unsafe { sqlite3changegroup_output(self.ptr, &mut out_len, &mut out_ptr) };
        if rc != SQLITE_OK {
            if !out_ptr.is_null() {
                // SAFETY: `sqlite3_malloc`-allocated buffer.
                unsafe { sqlite3_free(out_ptr) };
            }
            return Err(ChangesetError::ChangegroupOutputFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(copy_and_free(out_ptr, out_len))
    }

    /// Streamed [`add`](Self::add) backed by `sqlite3changegroup_add_strm`.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::ReaderIo`] / [`ChangesetError::ReaderPanicked`]
    ///   for stream failures.
    /// - [`ChangesetError::ChangegroupAddFailed`] for any other
    ///   `SQLite`-reported error.
    pub fn add_strm<R>(&mut self, reader: R) -> Result<(), ChangesetError>
    where
        R: std::io::Read,
    {
        let mut ctx = crate::streaming::InputContext::new(reader);
        // SAFETY: `ctx` lives on this stack frame for the whole call.
        let rc = unsafe {
            sqlite3changegroup_add_strm(
                self.ptr,
                Some(crate::streaming::read_trampoline::<R>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
            )
        };
        if let Some(err) = ctx.error.take() {
            return Err(ChangesetError::ReaderIo(err));
        }
        if ctx.panicked {
            return Err(ChangesetError::ReaderPanicked);
        }
        if rc != SQLITE_OK {
            return Err(ChangesetError::ChangegroupAddFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }

    /// Streamed [`output`](Self::output) backed by `sqlite3changegroup_output_strm`.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::WriterIo`] / [`ChangesetError::WriterPanicked`]
    ///   for stream failures.
    /// - [`ChangesetError::ChangegroupOutputFailed`] for any other
    ///   `SQLite`-reported error.
    pub fn output_strm<W>(&mut self, writer: W) -> Result<(), ChangesetError>
    where
        W: std::io::Write,
    {
        let mut ctx = crate::streaming::OutputContext::new(writer);
        // SAFETY: `ctx` lives on this stack frame for the whole call.
        let rc = unsafe {
            sqlite3changegroup_output_strm(
                self.ptr,
                Some(crate::streaming::write_trampoline::<W>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
            )
        };
        if let Some(err) = ctx.error.take() {
            return Err(ChangesetError::WriterIo(err));
        }
        if ctx.panicked {
            return Err(ChangesetError::WriterPanicked);
        }
        if rc != SQLITE_OK {
            return Err(ChangesetError::ChangegroupOutputFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }
}

impl Drop for Changegroup {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` came from `sqlite3changegroup_new` and no other
        // path frees it.
        unsafe {
            sqlite3changegroup_delete(self.ptr);
        }
    }
}

impl std::fmt::Debug for Changegroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changegroup").finish_non_exhaustive()
    }
}

/// Rewrite a changeset so it no longer conflicts with an already-applied one
/// (wraps `sqlite3rebaser_create`, `_configure`, `_rebase`, `_delete`).
///
/// # Multi-master workflow
///
/// 1. Replica applies changeset A via
///    [`SqliteSessionExt::apply_changeset_with`](crate::SqliteSessionExt::apply_changeset_with).
///    Any conflict resolved with [`Replace`](crate::ConflictAction::Replace)
///    or [`Omit`](crate::ConflictAction::Omit) makes
///    [`ApplyOutcome::rebase`](crate::ApplyOutcome::rebase) non-empty.
/// 2. The peer that produced A receives the rebase blob, creates a
///    [`Rebaser`], `configure`s it with those bytes, then `rebase`s its own
///    outbound changeset before shipping it elsewhere.
/// 3. The rebased changeset applies cleanly against destinations that had
///    already resolved the earlier conflict.
///
/// [`Rebaser`] is `!Send + !Sync`, like every other RAII handle here.
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// use diesel_sqlite_session::Rebaser;
/// assert_send::<Rebaser>();
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// use diesel_sqlite_session::Rebaser;
/// assert_sync::<Rebaser>();
/// ```
pub struct Rebaser {
    ptr: *mut sqlite3_rebaser,
    _not_send_or_sync: PhantomData<*const ()>,
}

impl Rebaser {
    /// Allocate a new empty rebaser (`sqlite3rebaser_create`).
    ///
    /// # Errors
    ///
    /// [`ChangesetError::RebaserCreateFailed`] on allocation failure.
    pub fn new() -> Result<Self, ChangesetError> {
        let mut ptr: *mut sqlite3_rebaser = ptr::null_mut();
        // SAFETY: `sqlite3rebaser_create` writes an owned allocation on OK.
        let rc = unsafe { sqlite3rebaser_create(&mut ptr) };
        if rc != SQLITE_OK {
            if !ptr.is_null() {
                // SAFETY: hand back partial allocation for freeing.
                unsafe { sqlite3rebaser_delete(ptr) };
            }
            return Err(ChangesetError::RebaserCreateFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        if ptr.is_null() {
            return Err(ChangesetError::RebaserCreateFailed(SqliteErrorCode::Error));
        }
        Ok(Self {
            ptr,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Feed a rebase blob from
    /// [`ApplyOutcome::rebase`](crate::ApplyOutcome::rebase). Stack multiple
    /// calls to extend the rebaser with more resolutions.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::EmptyChangeset`] if `rebase` is empty. `SQLite`
    ///   rejects an empty buffer, but the pre-flight check gives a clearer
    ///   error.
    /// - [`ChangesetError::LengthOverflow`] if `rebase.len()` overflows
    ///   `i32`.
    /// - [`ChangesetError::RebaserConfigureFailed`] on any `SQLite`-reported
    ///   error.
    pub fn configure(&mut self, rebase: &[u8]) -> Result<(), ChangesetError> {
        if rebase.is_empty() {
            return Err(ChangesetError::EmptyChangeset);
        }
        let len = c_int::try_from(rebase.len()).map_err(|_| ChangesetError::LengthOverflow {
            value: rebase.len(),
        })?;
        // SAFETY: `self.ptr` is a live rebaser and `rebase` is a valid slice.
        let rc =
            unsafe { sqlite3rebaser_configure(self.ptr, len, rebase.as_ptr().cast::<c_void>()) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::RebaserConfigureFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }

    /// Rewrite `changeset` so it no longer conflicts with the rebase blobs
    /// installed via [`configure`](Self::configure). Returns the rewritten
    /// bytes.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::EmptyChangeset`] if `changeset` is empty.
    /// - [`ChangesetError::LengthOverflow`] if `changeset.len()` overflows
    ///   `i32`.
    /// - [`ChangesetError::RebaserRebaseFailed`] on any `SQLite`-reported
    ///   error.
    pub fn rebase(&self, changeset: &[u8]) -> Result<Vec<u8>, ChangesetError> {
        if changeset.is_empty() {
            return Err(ChangesetError::EmptyChangeset);
        }
        let n_in =
            c_int::try_from(changeset.len()).map_err(|_| ChangesetError::LengthOverflow {
                value: changeset.len(),
            })?;
        let mut out_ptr: *mut c_void = ptr::null_mut();
        let mut out_len: c_int = 0;
        // SAFETY: `sqlite3rebaser_rebase` treats `changeset` as read-only.
        let rc = unsafe {
            sqlite3rebaser_rebase(
                self.ptr,
                n_in,
                changeset.as_ptr().cast::<c_void>(),
                &mut out_len,
                &mut out_ptr,
            )
        };
        if rc != SQLITE_OK {
            if !out_ptr.is_null() {
                // SAFETY: `sqlite3_malloc`-allocated buffer.
                unsafe { sqlite3_free(out_ptr) };
            }
            return Err(ChangesetError::RebaserRebaseFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(copy_and_free(out_ptr, out_len))
    }

    /// Streamed [`rebase`](Self::rebase) backed by `sqlite3rebaser_rebase_strm`.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::ReaderIo`] / [`ChangesetError::ReaderPanicked`]
    ///   for reader failures.
    /// - [`ChangesetError::WriterIo`] / [`ChangesetError::WriterPanicked`]
    ///   for writer failures.
    /// - [`ChangesetError::RebaserRebaseFailed`] for any other
    ///   `SQLite`-reported error.
    pub fn rebase_strm<R, W>(&self, reader: R, writer: W) -> Result<(), ChangesetError>
    where
        R: std::io::Read,
        W: std::io::Write,
    {
        let mut input_ctx = crate::streaming::InputContext::new(reader);
        let mut output_ctx = crate::streaming::OutputContext::new(writer);
        // SAFETY: both contexts live on this stack frame for the whole call.
        let rc = unsafe {
            sqlite3rebaser_rebase_strm(
                self.ptr,
                Some(crate::streaming::read_trampoline::<R>),
                ptr::addr_of_mut!(input_ctx).cast::<c_void>(),
                Some(crate::streaming::write_trampoline::<W>),
                ptr::addr_of_mut!(output_ctx).cast::<c_void>(),
            )
        };
        if let Some(err) = input_ctx.error.take() {
            return Err(ChangesetError::ReaderIo(err));
        }
        if input_ctx.panicked {
            return Err(ChangesetError::ReaderPanicked);
        }
        if let Some(err) = output_ctx.error.take() {
            return Err(ChangesetError::WriterIo(err));
        }
        if output_ctx.panicked {
            return Err(ChangesetError::WriterPanicked);
        }
        if rc != SQLITE_OK {
            return Err(ChangesetError::RebaserRebaseFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(())
    }
}

impl Drop for Rebaser {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` came from `sqlite3rebaser_create` and no other
        // path frees it.
        unsafe {
            sqlite3rebaser_delete(self.ptr);
        }
    }
}

impl std::fmt::Debug for Rebaser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rebaser").finish_non_exhaustive()
    }
}

/// Copy `len` bytes from `ptr` into an owned `Vec<u8>` and release the
/// `SQLite` allocation. Handles null and zero-length inputs.
fn copy_and_free(ptr: *mut c_void, len: c_int) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    let n = usize::try_from(len).unwrap_or(0);
    let bytes = if n == 0 {
        Vec::new()
    } else {
        // SAFETY: SQLite reports `n` readable bytes at `ptr`.
        let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), n) };
        slice.to_vec()
    };
    // SAFETY: `sqlite3_malloc`-allocated buffer.
    unsafe { sqlite3_free(ptr) };
    bytes
}
