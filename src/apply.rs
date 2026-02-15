//! Apply changesets and patchsets to Diesel connections.

use std::ffi::c_int;
use std::ptr;

use diesel::SqliteConnection;

use crate::errors::{ApplyError, ConflictAction, ConflictType, SqliteErrorCode};
use crate::ffi::{
    sqlite3_changeset_iter, sqlite3changeset_apply, SQLITE_CHANGESET_ABORT, SQLITE_OK,
};

/// Conflict handler callback context.
struct ConflictContext<F> {
    handler: F,
    aborted: bool,
}

/// External C callback for conflict handling.
///
/// # Safety
///
/// This function is called by `SQLite` with valid pointers.
unsafe extern "C" fn conflict_callback<F>(
    context: *mut std::ffi::c_void,
    conflict_type: c_int,
    _iter: *mut sqlite3_changeset_iter,
) -> c_int
where
    F: Fn(ConflictType) -> ConflictAction,
{
    let ctx = unsafe { &mut *context.cast::<ConflictContext<F>>() };

    let conflict = ConflictType::from_raw(conflict_type).unwrap_or(ConflictType::Constraint);
    let action = (ctx.handler)(conflict);

    if action == ConflictAction::Abort {
        ctx.aborted = true;
    }

    action.to_raw()
}

/// Apply a changeset to a Diesel connection.
///
/// A changeset contains complete information about changes, including old
/// values for conflict detection.
///
/// This is an internal function. Use `SqliteSessionExt::apply_changeset` instead.
#[inline]
pub(crate) fn apply_changeset<F>(
    conn: &mut SqliteConnection,
    changeset: &[u8],
    on_conflict: F,
) -> Result<(), ApplyError>
where
    F: Fn(ConflictType) -> ConflictAction,
{
    apply_impl(conn, changeset, on_conflict)
}

/// Apply a patchset to a Diesel connection.
///
/// A patchset contains only new values (not old values), making it smaller
/// but with less precise conflict detection.
///
/// This is an internal function. Use `SqliteSessionExt::apply_patchset` instead.
#[inline]
pub(crate) fn apply_patchset<F>(
    conn: &mut SqliteConnection,
    patchset: &[u8],
    on_conflict: F,
) -> Result<(), ApplyError>
where
    F: Fn(ConflictType) -> ConflictAction,
{
    apply_impl(conn, patchset, on_conflict)
}

/// Internal implementation for applying both changesets and patchsets.
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn apply_impl<F>(conn: &mut SqliteConnection, data: &[u8], on_conflict: F) -> Result<(), ApplyError>
where
    F: Fn(ConflictType) -> ConflictAction,
{
    if data.is_empty() {
        return Ok(());
    }

    let mut context = ConflictContext {
        handler: on_conflict,
        aborted: false,
    };

    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply(
                raw,
                data.len() as c_int,
                data.as_ptr().cast::<std::ffi::c_void>().cast_mut(),
                None, // xFilter - no filtering
                Some(conflict_callback::<F>),
                ptr::addr_of_mut!(context).cast(),
            )
        })
    };

    if context.aborted {
        return Err(ApplyError::ConflictAborted);
    }

    if rc != SQLITE_OK && rc != SQLITE_CHANGESET_ABORT {
        return Err(ApplyError::ApplyFailed(SqliteErrorCode::from_error(rc)));
    }

    Ok(())
}
