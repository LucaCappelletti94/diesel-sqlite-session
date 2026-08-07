//! Checking that a connection really has a database of a given name.
//!
//! Several `SQLite` entry points take a database name, copy it, and never look
//! it up, so a name that answers to nothing is accepted and only misbehaves
//! later. `sqlite3session_create` and `sqlite3changegroup_schema` both do
//! this. Checking here turns that into an error at the call that carried the
//! name.

use std::ffi::CStr;

use crate::ffi::{sqlite3, sqlite3_txn_state};

/// Whether `name` is a database on `db`.
///
/// `sqlite3_txn_state` answers -1, and only -1, when the name is not a
/// database on the connection. It reports `temp` correctly even before the
/// first temporary table exists, which `sqlite3_db_readonly` does not, because
/// that one resolves through a b-tree `temp` has not been given yet.
///
/// # Safety
///
/// `db` must be a live connection handle.
pub(crate) unsafe fn database_exists(db: *mut sqlite3, name: &CStr) -> bool {
    // SAFETY: `db` is live per the caller contract, and `name` is a
    // NUL-terminated C string that outlives the call.
    unsafe { sqlite3_txn_state(db, name.as_ptr()) >= 0 }
}
