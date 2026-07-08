//! `sqlite3changeset_apply_v2` wrapper: adds an [`ApplyFlags`] bitmask, a
//! per-table filter callback, and the rebase blob `SQLite` emits when the
//! conflict callback resolves anything via `Replace` or `Omit`.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use diesel::SqliteConnection;

use crate::changeset::{ChangesetError, ChangesetOp, ChangesetRow, ChangesetValue};
use crate::errors::{ApplyError, ConflictAction, ConflictType, SqliteErrorCode};
use crate::ffi::{
    sqlite3_changeset_iter, sqlite3_free, sqlite3_value, sqlite3changeset_apply_v2,
    sqlite3changeset_apply_v2_strm, sqlite3changeset_apply_v3, sqlite3changeset_apply_v3_strm,
    sqlite3changeset_conflict, sqlite3changeset_fk_conflicts, sqlite3changeset_new,
    sqlite3changeset_old, sqlite3changeset_op, SQLITE_CHANGESETAPPLY_FKNOACTION,
    SQLITE_CHANGESETAPPLY_IGNORENOOP, SQLITE_CHANGESETAPPLY_INVERT,
    SQLITE_CHANGESETAPPLY_NOSAVEPOINT, SQLITE_CHANGESET_ABORT, SQLITE_OK, SQLITE_TOOBIG,
};

/// Flag bitmask for [`SqliteSessionExt::apply_changeset_with`](crate::SqliteSessionExt::apply_changeset_with).
///
/// Compose with `|`. `ApplyFlags::empty()` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ApplyFlags(c_int);

impl ApplyFlags {
    /// `SQLITE_CHANGESETAPPLY_NOSAVEPOINT`. Skip the `SAVEPOINT` `SQLite`
    /// otherwise wraps around the apply. The caller becomes responsible for
    /// transactional atomicity.
    pub const NOSAVEPOINT: Self = Self(SQLITE_CHANGESETAPPLY_NOSAVEPOINT);

    /// `SQLITE_CHANGESETAPPLY_INVERT`. Apply the inverse of the changeset:
    /// every `INSERT` becomes a `DELETE` and vice versa. `UPDATE` rows swap
    /// their old and new values.
    pub const INVERT: Self = Self(SQLITE_CHANGESETAPPLY_INVERT);

    /// `SQLITE_CHANGESETAPPLY_IGNORENOOP`. Do not invoke the conflict
    /// callback for `UPDATE` changes whose replica-side value already equals
    /// the changeset's new value.
    pub const IGNORENOOP: Self = Self(SQLITE_CHANGESETAPPLY_IGNORENOOP);

    /// `SQLITE_CHANGESETAPPLY_FKNOACTION`. Skip the `NO ACTION` foreign-key
    /// handling `SQLite` would otherwise apply on `DELETE` and `UPDATE`.
    pub const FKNOACTION: Self = Self(SQLITE_CHANGESETAPPLY_FKNOACTION);

    /// The empty flag mask.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// The raw `c_int` bit pattern passed to `sqlite3changeset_apply_v2`.
    #[must_use]
    pub const fn bits(self) -> c_int {
        self.0
    }

    /// True iff every flag in `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for ApplyFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for ApplyFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for ApplyFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// View of the offending row handed to the conflict callback of
/// [`SqliteSessionExt::apply_changeset_with`](crate::SqliteSessionExt::apply_changeset_with).
/// All value accessors borrow from the current iterator position and cannot
/// escape the callback frame.
#[derive(Clone, Copy)]
pub struct ConflictInfo<'a> {
    iter: *mut sqlite3_changeset_iter,
    conflict_type: ConflictType,
    op: Option<ChangesetOp>,
    table: &'a str,
    column_count: usize,
    _marker: PhantomData<&'a sqlite3_changeset_iter>,
}

impl<'a> ConflictInfo<'a> {
    /// The `SQLITE_CHANGESET_*` conflict kind that triggered the callback.
    #[must_use]
    pub fn conflict_type(&self) -> ConflictType {
        self.conflict_type
    }

    /// The row's op, or `None` for [`ConflictType::ForeignKey`] callbacks
    /// (which fire with no row selected, so `sqlite3changeset_op` is undefined).
    #[must_use]
    pub fn op(&self) -> Option<ChangesetOp> {
        self.op
    }

    /// The affected table.
    #[must_use]
    pub fn table(&self) -> &'a str {
        self.table
    }

    /// Number of columns in the row.
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    /// Read the pre-image value at `index`.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::OldNotAvailableOnInsert`] on `INSERT` rows.
    /// - [`ChangesetError::ColumnOutOfRange`] when `index >= column_count()`.
    /// - [`ChangesetError::ValueReadFailed`] when `SQLite` refuses.
    pub fn old_value(&self, index: u32) -> Result<Option<ChangesetValue<'a>>, ChangesetError> {
        if matches!(self.op, Some(ChangesetOp::Insert)) {
            return Err(ChangesetError::OldNotAvailableOnInsert);
        }
        self.value_at(index, sqlite3changeset_old)
    }

    /// Read the post-image value at `index`.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::NewNotAvailableOnDelete`] on `DELETE` rows.
    /// - [`ChangesetError::ColumnOutOfRange`] when `index >= column_count()`.
    /// - [`ChangesetError::ValueReadFailed`] when `SQLite` refuses.
    pub fn new_value(&self, index: u32) -> Result<Option<ChangesetValue<'a>>, ChangesetError> {
        if matches!(self.op, Some(ChangesetOp::Delete)) {
            return Err(ChangesetError::NewNotAvailableOnDelete);
        }
        self.value_at(index, sqlite3changeset_new)
    }

    /// Read the on-disk value at `index` that clashes with the change. Only
    /// valid for `Data`, `Conflict`, and `Constraint` conflicts.
    ///
    /// # Errors
    ///
    /// - [`ChangesetError::ColumnOutOfRange`] when `index >= column_count()`.
    /// - [`ChangesetError::ValueReadFailed`] when `SQLite` refuses, typically
    ///   because the conflict type does not carry an on-disk value.
    pub fn conflict_value(&self, index: u32) -> Result<ChangesetValue<'a>, ChangesetError> {
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
        // SAFETY: `self.iter` is the live iterator SQLite handed us for the
        // duration of the conflict callback and `value` is a valid out-pointer.
        let rc = unsafe { sqlite3changeset_conflict(self.iter, idx, &mut value) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ValueReadFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        if value.is_null() {
            return Err(ChangesetError::ValueReadFailed(SqliteErrorCode::Error));
        }
        Ok(ChangesetValue::from_ptr(value))
    }

    /// FK violation count for the current change ([`ConflictType::ForeignKey`] only).
    ///
    /// # Errors
    ///
    /// [`ChangesetError::ValueReadFailed`] if `SQLite` refuses.
    pub fn fk_conflicts_count(&self) -> Result<u32, ChangesetError> {
        let mut n: c_int = 0;
        // SAFETY: `self.iter` is the live iterator.
        let rc = unsafe { sqlite3changeset_fk_conflicts(self.iter, &mut n) };
        if rc != SQLITE_OK {
            return Err(ChangesetError::ValueReadFailed(
                SqliteErrorCode::from_error(rc),
            ));
        }
        Ok(u32::try_from(n).unwrap_or(0))
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
        // SAFETY: `self.iter` is the live iterator and `value` is a valid out-pointer.
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

impl std::fmt::Debug for ConflictInfo<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConflictInfo")
            .field("conflict_type", &self.conflict_type)
            .field("op", &self.op)
            .field("table", &self.table)
            .field("column_count", &self.column_count)
            .finish_non_exhaustive()
    }
}

/// Result of a successful `sqlite3changeset_apply_v2` call.
#[derive(Debug, Clone, Default)]
pub struct ApplyOutcome {
    /// Rebase blob emitted by `SQLite`, empty when no rebase data was produced
    /// (which is the case whenever the conflict callback did not resolve any
    /// conflict via `Replace` or `Omit`).
    pub rebase: Vec<u8>,
}

/// Shared body behind the trait's `apply_changeset_with` method.
pub(crate) fn apply_changeset_with<Filter, Conflict>(
    conn: &mut SqliteConnection,
    changeset: &[u8],
    flags: ApplyFlags,
    filter: Filter,
    on_conflict: Conflict,
) -> Result<ApplyOutcome, ApplyError>
where
    Filter: Fn(&str) -> bool,
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    if changeset.is_empty() {
        return Ok(ApplyOutcome { rebase: Vec::new() });
    }
    let data_len = c_int::try_from(changeset.len())
        .map_err(|_| ApplyError::ApplyFailed(SqliteErrorCode::from_error(SQLITE_TOOBIG)))?;

    let mut ctx = ApplyV2Context {
        filter,
        conflict: on_conflict,
        aborted: false,
        filter_panicked: false,
        conflict_panicked: false,
    };

    let mut rebase_ptr: *mut c_void = ptr::null_mut();
    let mut rebase_len: c_int = 0;

    // SAFETY: `with_raw_connection` yields a live `sqlite3*` and `ctx` outlives
    // the FFI call from this stack frame.
    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply_v2(
                raw,
                data_len,
                changeset.as_ptr().cast::<c_void>().cast_mut(),
                Some(filter_trampoline::<Filter, Conflict>),
                Some(conflict_trampoline::<Filter, Conflict>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
                &mut rebase_ptr,
                &mut rebase_len,
                flags.bits(),
            )
        })
    };

    let mut rebase_bytes = Vec::new();
    if !rebase_ptr.is_null() && rebase_len > 0 {
        let n = usize::try_from(rebase_len).unwrap_or(0);
        // SAFETY: SQLite reports `n` readable bytes at `rebase_ptr`.
        let slice = unsafe { std::slice::from_raw_parts(rebase_ptr.cast::<u8>(), n) };
        rebase_bytes.extend_from_slice(slice);
    }
    if !rebase_ptr.is_null() {
        // SAFETY: `sqlite3_malloc`-allocated buffer.
        unsafe {
            sqlite3_free(rebase_ptr);
        }
    }

    if ctx.filter_panicked {
        return Err(ApplyError::FilterPanicked);
    }
    if ctx.conflict_panicked {
        return Err(ApplyError::ConflictHandlerPanicked);
    }
    if ctx.aborted {
        return Err(ApplyError::ConflictAborted);
    }
    if rc != SQLITE_OK && rc != SQLITE_CHANGESET_ABORT {
        return Err(ApplyError::ApplyFailed(SqliteErrorCode::from_error(rc)));
    }
    Ok(ApplyOutcome {
        rebase: rebase_bytes,
    })
}

/// Streamed `sqlite3changeset_apply_v2_strm` sibling. Reads the changeset in
/// chunks; the rebase blob is still returned as a fully-buffered `Vec<u8>`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_changeset_strm_with<R, Filter, Conflict>(
    conn: &mut SqliteConnection,
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
    let mut ctx = ApplyV2Context {
        filter,
        conflict: on_conflict,
        aborted: false,
        filter_panicked: false,
        conflict_panicked: false,
    };
    let mut input_ctx = crate::streaming::InputContext::new(reader);
    let input_ptr = ptr::addr_of_mut!(input_ctx).cast::<c_void>();

    let mut rebase_ptr: *mut c_void = ptr::null_mut();
    let mut rebase_len: c_int = 0;

    // SAFETY: `with_raw_connection` yields a live `sqlite3*`; both contexts
    // outlive the FFI call from this stack frame.
    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply_v2_strm(
                raw,
                Some(crate::streaming::read_trampoline::<R>),
                input_ptr,
                Some(filter_trampoline::<Filter, Conflict>),
                Some(conflict_trampoline::<Filter, Conflict>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
                &mut rebase_ptr,
                &mut rebase_len,
                flags.bits(),
            )
        })
    };

    let mut rebase_bytes = Vec::new();
    if !rebase_ptr.is_null() && rebase_len > 0 {
        let n = usize::try_from(rebase_len).unwrap_or(0);
        // SAFETY: SQLite reports `n` readable bytes at `rebase_ptr`.
        let slice = unsafe { std::slice::from_raw_parts(rebase_ptr.cast::<u8>(), n) };
        rebase_bytes.extend_from_slice(slice);
    }
    if !rebase_ptr.is_null() {
        // SAFETY: sqlite_malloc-allocated buffer.
        unsafe { sqlite3_free(rebase_ptr) };
    }

    if let Some(err) = input_ctx.error.take() {
        return Err(ApplyError::ReaderIo(err));
    }
    if input_ctx.panicked {
        return Err(ApplyError::ReaderPanicked);
    }
    if ctx.filter_panicked {
        return Err(ApplyError::FilterPanicked);
    }
    if ctx.conflict_panicked {
        return Err(ApplyError::ConflictHandlerPanicked);
    }
    if ctx.aborted {
        return Err(ApplyError::ConflictAborted);
    }
    if rc != SQLITE_OK && rc != SQLITE_CHANGESET_ABORT {
        return Err(ApplyError::ApplyFailed(SqliteErrorCode::from_error(rc)));
    }
    Ok(ApplyOutcome {
        rebase: rebase_bytes,
    })
}

struct ApplyV2Context<Filter, Conflict> {
    filter: Filter,
    conflict: Conflict,
    aborted: bool,
    filter_panicked: bool,
    conflict_panicked: bool,
}

/// C trampoline for `xFilter`.
///
/// # Safety
///
/// `ctx_ptr` must point to a live `ApplyV2Context<Filter, Conflict>`, and
/// `table_ptr` must be null or a valid C string for the call duration.
unsafe extern "C" fn filter_trampoline<Filter, Conflict>(
    ctx_ptr: *mut c_void,
    table_ptr: *const c_char,
) -> c_int
where
    Filter: Fn(&str) -> bool,
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    // SAFETY: `ctx_ptr` is the same pointer we passed to `sqlite3changeset_apply_v2`.
    let ctx = unsafe { &mut *(ctx_ptr.cast::<ApplyV2Context<Filter, Conflict>>()) };
    let table = if table_ptr.is_null() {
        ""
    } else {
        // SAFETY: non-null `table_ptr` is a valid C string per the FFI contract.
        unsafe { CStr::from_ptr(table_ptr) }.to_str().unwrap_or("")
    };
    match catch_unwind(AssertUnwindSafe(|| (ctx.filter)(table))) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => {
            ctx.filter_panicked = true;
            // Filter panic aborts the apply by returning 0 for every remaining
            // table, plus the `filter_panicked` flag surfaces as ApplyError.
            0
        }
    }
}

/// C trampoline for `xConflict`.
///
/// # Safety
///
/// `ctx_ptr` must point to a live `ApplyV2Context<Filter, Conflict>` and
/// `iter` must be the iterator `SQLite` supplies for the current row, both
/// valid for the duration of the call.
unsafe extern "C" fn conflict_trampoline<Filter, Conflict>(
    ctx_ptr: *mut c_void,
    e_conflict: c_int,
    iter: *mut sqlite3_changeset_iter,
) -> c_int
where
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    // SAFETY: `ctx_ptr` matches the type we installed.
    let ctx = unsafe { &mut *(ctx_ptr.cast::<ApplyV2Context<Filter, Conflict>>()) };
    let action = build_and_dispatch(ctx, e_conflict, iter);
    if action == ConflictAction::Abort {
        ctx.aborted = true;
    }
    action.to_raw()
}

fn build_and_dispatch<Filter, Conflict>(
    ctx: &mut ApplyV2Context<Filter, Conflict>,
    e_conflict: c_int,
    iter: *mut sqlite3_changeset_iter,
) -> ConflictAction
where
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    let Some(conflict_type) = ConflictType::from_raw(e_conflict) else {
        return ConflictAction::Abort;
    };

    // FK conflicts fire at the end of apply with no active row, so
    // `sqlite3changeset_op` / `_old` / `_new` / `_pk` are undefined here and
    // only `_fk_conflicts` is legal. Build a minimal `ConflictInfo` and skip.
    let info = if matches!(conflict_type, ConflictType::ForeignKey) {
        ConflictInfo {
            iter,
            conflict_type,
            op: None,
            table: "",
            column_count: 0,
            _marker: PhantomData,
        }
    } else {
        let mut table_ptr: *const c_char = ptr::null();
        let mut n_col: c_int = 0;
        let mut op_code: c_int = 0;
        let mut indirect_int: c_int = 0;
        // SAFETY: `iter` is live for the callback frame per SQLite's contract.
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
            return ConflictAction::Abort;
        }
        let Some(op) = ChangesetOp::from_raw(op_code) else {
            return ConflictAction::Abort;
        };
        let table = if table_ptr.is_null() {
            ""
        } else {
            // SAFETY: `table_ptr` is a valid C string per `sqlite3changeset_op`.
            match unsafe { CStr::from_ptr(table_ptr) }.to_str() {
                Ok(s) => s,
                Err(_) => return ConflictAction::Abort,
            }
        };
        let column_count = usize::try_from(n_col).unwrap_or(0);

        ConflictInfo {
            iter,
            conflict_type,
            op: Some(op),
            table,
            column_count,
            _marker: PhantomData,
        }
    };

    if let Ok(action) = catch_unwind(AssertUnwindSafe(|| (ctx.conflict)(info))) {
        action
    } else {
        ctx.conflict_panicked = true;
        ConflictAction::Abort
    }
}

/// Backing implementation for
/// [`SqliteSessionExt::apply_changeset_v3_with`](crate::SqliteSessionExt::apply_changeset_v3_with).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_changeset_v3_with<Filter, Conflict>(
    conn: &mut SqliteConnection,
    changeset: &[u8],
    flags: ApplyFlags,
    filter: Filter,
    on_conflict: Conflict,
) -> Result<ApplyOutcome, ApplyError>
where
    Filter: Fn(ChangesetRow<'_>) -> bool,
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    if changeset.is_empty() {
        return Ok(ApplyOutcome { rebase: Vec::new() });
    }
    let data_len = c_int::try_from(changeset.len())
        .map_err(|_| ApplyError::ApplyFailed(SqliteErrorCode::from_error(SQLITE_TOOBIG)))?;

    let mut ctx = ApplyV2Context {
        filter,
        conflict: on_conflict,
        aborted: false,
        filter_panicked: false,
        conflict_panicked: false,
    };

    let mut rebase_ptr: *mut c_void = ptr::null_mut();
    let mut rebase_len: c_int = 0;

    // SAFETY: `with_raw_connection` yields a live `sqlite3*`; `ctx` outlives
    // the FFI call from this stack frame.
    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply_v3(
                raw,
                data_len,
                changeset.as_ptr().cast::<c_void>().cast_mut(),
                Some(v3_filter_trampoline::<Filter, Conflict>),
                Some(conflict_trampoline::<Filter, Conflict>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
                &mut rebase_ptr,
                &mut rebase_len,
                flags.bits(),
            )
        })
    };

    finalize_v3(&ctx, rc, rebase_ptr, rebase_len)
}

/// Streamed backing implementation for
/// [`SqliteSessionExt::apply_changeset_v3_strm_with`](crate::SqliteSessionExt::apply_changeset_v3_strm_with).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_changeset_v3_strm_with<R, Filter, Conflict>(
    conn: &mut SqliteConnection,
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
    let mut ctx = ApplyV2Context {
        filter,
        conflict: on_conflict,
        aborted: false,
        filter_panicked: false,
        conflict_panicked: false,
    };
    let mut input_ctx = crate::streaming::InputContext::new(reader);
    let input_ptr = ptr::addr_of_mut!(input_ctx).cast::<c_void>();

    let mut rebase_ptr: *mut c_void = ptr::null_mut();
    let mut rebase_len: c_int = 0;

    // SAFETY: contexts live on this stack frame; callback signatures match.
    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply_v3_strm(
                raw,
                Some(crate::streaming::read_trampoline::<R>),
                input_ptr,
                Some(v3_filter_trampoline::<Filter, Conflict>),
                Some(conflict_trampoline::<Filter, Conflict>),
                ptr::addr_of_mut!(ctx).cast::<c_void>(),
                &mut rebase_ptr,
                &mut rebase_len,
                flags.bits(),
            )
        })
    };

    if let Some(err) = input_ctx.error.take() {
        if !rebase_ptr.is_null() {
            // SAFETY: sqlite_malloc-allocated buffer.
            unsafe { sqlite3_free(rebase_ptr) };
        }
        return Err(ApplyError::ReaderIo(err));
    }
    if input_ctx.panicked {
        if !rebase_ptr.is_null() {
            // SAFETY: sqlite_malloc-allocated buffer.
            unsafe { sqlite3_free(rebase_ptr) };
        }
        return Err(ApplyError::ReaderPanicked);
    }

    finalize_v3(&ctx, rc, rebase_ptr, rebase_len)
}

/// Common post-call bookkeeping shared by the buffered and streamed v3 paths.
fn finalize_v3<Filter, Conflict>(
    ctx: &ApplyV2Context<Filter, Conflict>,
    rc: c_int,
    rebase_ptr: *mut c_void,
    rebase_len: c_int,
) -> Result<ApplyOutcome, ApplyError> {
    let mut rebase_bytes = Vec::new();
    if !rebase_ptr.is_null() && rebase_len > 0 {
        let n = usize::try_from(rebase_len).unwrap_or(0);
        // SAFETY: SQLite reports `n` readable bytes at `rebase_ptr`.
        let slice = unsafe { std::slice::from_raw_parts(rebase_ptr.cast::<u8>(), n) };
        rebase_bytes.extend_from_slice(slice);
    }
    if !rebase_ptr.is_null() {
        // SAFETY: sqlite_malloc-allocated buffer.
        unsafe { sqlite3_free(rebase_ptr) };
    }

    if ctx.filter_panicked {
        return Err(ApplyError::FilterPanicked);
    }
    if ctx.conflict_panicked {
        return Err(ApplyError::ConflictHandlerPanicked);
    }
    if ctx.aborted {
        return Err(ApplyError::ConflictAborted);
    }
    if rc != SQLITE_OK && rc != SQLITE_CHANGESET_ABORT {
        return Err(ApplyError::ApplyFailed(SqliteErrorCode::from_error(rc)));
    }
    Ok(ApplyOutcome {
        rebase: rebase_bytes,
    })
}

/// C trampoline for the v3 `xFilter` (receives a live iterator, not just a
/// table name).
///
/// # Safety
///
/// `ctx_ptr` must point to a live `ApplyV2Context<Filter, Conflict>` and
/// `iter` must be the iterator `SQLite` supplies for the call.
unsafe extern "C" fn v3_filter_trampoline<Filter, Conflict>(
    ctx_ptr: *mut c_void,
    iter: *mut sqlite3_changeset_iter,
) -> c_int
where
    Filter: Fn(ChangesetRow<'_>) -> bool,
    Conflict: Fn(ConflictInfo<'_>) -> ConflictAction,
{
    // SAFETY: `ctx_ptr` matches the type we installed.
    let ctx = unsafe { &mut *(ctx_ptr.cast::<ApplyV2Context<Filter, Conflict>>()) };
    // SAFETY: `iter` is the live iterator SQLite hands us for this row.
    let Ok(row) = (unsafe { ChangesetRow::read_current(iter) }) else {
        return 0;
    };
    match catch_unwind(AssertUnwindSafe(|| (ctx.filter)(row))) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => {
            ctx.filter_panicked = true;
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ApplyFlags;

    #[test]
    fn apply_flags_bitor_composes() {
        let combined = ApplyFlags::INVERT | ApplyFlags::IGNORENOOP;
        assert!(combined.contains(ApplyFlags::INVERT));
        assert!(combined.contains(ApplyFlags::IGNORENOOP));
        assert!(!combined.contains(ApplyFlags::NOSAVEPOINT));
    }

    #[test]
    fn apply_flags_bitor_assign() {
        let mut flags = ApplyFlags::INVERT;
        flags |= ApplyFlags::NOSAVEPOINT;
        assert!(flags.contains(ApplyFlags::INVERT));
        assert!(flags.contains(ApplyFlags::NOSAVEPOINT));
    }

    #[test]
    fn apply_flags_empty_contains_nothing() {
        let empty = ApplyFlags::empty();
        assert!(!empty.contains(ApplyFlags::INVERT));
        assert!(!empty.contains(ApplyFlags::NOSAVEPOINT));
        assert_eq!(empty.bits(), 0);
    }

    #[test]
    fn apply_flags_bitand_intersects() {
        let combined = ApplyFlags::INVERT | ApplyFlags::IGNORENOOP;
        let masked = combined & ApplyFlags::INVERT;
        assert_eq!(masked, ApplyFlags::INVERT);
    }
}
