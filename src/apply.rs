//! Apply changesets and patchsets to Diesel connections.

use std::ffi::c_int;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use diesel::SqliteConnection;

use crate::errors::{ApplyError, ConflictAction, ConflictType, SqliteErrorCode};
use crate::ffi::{
    sqlite3_changeset_iter, sqlite3changeset_apply, SQLITE_CHANGESET_ABORT, SQLITE_OK,
    SQLITE_TOOBIG,
};

/// Conflict handler callback context.
struct ConflictContext<F> {
    handler: F,
    aborted: bool,
    panicked: bool,
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
    // SAFETY: SQLite invokes this callback with the same context pointer we
    // provided to `sqlite3changeset_apply`.
    let ctx = unsafe { &mut *context.cast::<ConflictContext<F>>() };

    let action = ConflictType::from_raw(conflict_type).map_or(ConflictAction::Abort, |conflict| {
        if let Ok(action) = catch_unwind(AssertUnwindSafe(|| (ctx.handler)(conflict))) {
            action
        } else {
            ctx.panicked = true;
            ConflictAction::Abort
        }
    });

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
        panicked: false,
    };
    let data_len = c_int::try_from(data.len())
        .map_err(|_| ApplyError::ApplyFailed(SqliteErrorCode::from_error(SQLITE_TOOBIG)))?;

    // SAFETY: `with_raw_connection` provides a valid SQLite connection pointer for
    // the callback duration, `data` lives through the FFI call, and `context`
    // points to stack storage that also outlives the call.
    let rc = unsafe {
        conn.with_raw_connection(|raw| {
            sqlite3changeset_apply(
                raw,
                data_len,
                data.as_ptr().cast::<std::ffi::c_void>().cast_mut(),
                None, // xFilter - no filtering
                Some(conflict_callback::<F>),
                ptr::addr_of_mut!(context).cast(),
            )
        })
    };

    if context.panicked {
        return Err(ApplyError::ConflictHandlerPanicked);
    }

    if context.aborted {
        return Err(ApplyError::ConflictAborted);
    }

    if rc != SQLITE_OK && rc != SQLITE_CHANGESET_ABORT {
        return Err(ApplyError::ApplyFailed(SqliteErrorCode::from_error(rc)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn invoke_conflict_callback<F>(context: &mut ConflictContext<F>, conflict_type: i32) -> c_int
    where
        F: Fn(ConflictType) -> ConflictAction,
    {
        // SAFETY: `context` points to valid storage for the callback duration and
        // iterator pointer is null because the callback implementation does not use it.
        unsafe {
            conflict_callback::<F>(
                ptr::addr_of_mut!(*context).cast(),
                conflict_type,
                ptr::null_mut(),
            )
        }
    }

    #[test]
    fn conflict_callback_uses_handler_for_known_conflicts() {
        let mut context = ConflictContext {
            handler: |conflict: ConflictType| {
                if conflict == ConflictType::Data {
                    ConflictAction::Replace
                } else {
                    ConflictAction::Abort
                }
            },
            aborted: false,
            panicked: false,
        };

        let rc = invoke_conflict_callback(&mut context, ConflictType::Data.to_raw());

        assert_eq!(rc, ConflictAction::Replace.to_raw());
        assert!(!context.aborted);
        assert!(!context.panicked);
    }

    #[test]
    fn conflict_callback_aborts_unknown_conflict_codes() {
        let invoked = AtomicBool::new(false);
        let mut context = ConflictContext {
            handler: |_| {
                invoked.store(true, Ordering::SeqCst);
                ConflictAction::Replace
            },
            aborted: false,
            panicked: false,
        };

        let rc = invoke_conflict_callback(&mut context, 999);

        assert_eq!(rc, ConflictAction::Abort.to_raw());
        assert!(context.aborted);
        assert!(!invoked.load(Ordering::SeqCst));
        assert!(!context.panicked);
    }

    #[test]
    fn conflict_callback_marks_panicked_handlers() {
        let mut context = ConflictContext {
            handler: |_| -> ConflictAction {
                panic!("boom");
            },
            aborted: false,
            panicked: false,
        };

        let rc = invoke_conflict_callback(&mut context, ConflictType::Data.to_raw());

        assert_eq!(rc, ConflictAction::Abort.to_raw());
        assert!(context.aborted);
        assert!(context.panicked);
    }
}
