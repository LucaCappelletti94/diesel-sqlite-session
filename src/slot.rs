//! Ownership of the one pre-update callback slot a connection has.
//!
//! `SQLite` gives each connection a single `sqlite3_preupdate_hook` slot and
//! the session extension claims it for itself. `sqlite3session_create` links
//! whatever it finds there into its own session list, and
//! `sqlite3session_delete` walks the slot the same way, so a
//! [`crate::Session`] and a [`crate::PreUpdateHook`] alive at the same time
//! make `SQLite` read a boxed Rust closure as an `sqlite3_session`.
//!
//! Neither side can look at the slot, because `sqlite3_preupdate_hook` only
//! hands back the previous context as a side effect of overwriting it. So the
//! two populations are counted beside it, in the connection's own client-data
//! store, and each side refuses while the other is present. `SQLite` runs the
//! destructor when the connection closes, so nothing here outlives it.
//!
//! Counting rather than flagging, because several sessions on one connection
//! are ordinary and `SQLite` links them, and a second hook may replace a first
//! while both guards are alive.
//!
//! # Concurrency
//!
//! Claiming needs `&mut SqliteConnection`, so no two claims can overlap.
//! Releasing does not, because [`crate::Session`] is [`Send`] and its `Drop`
//! only needs the raw handle, so a release can land on another thread while a
//! claim runs here. Two consequences shape the code below. Every change is a
//! single atomic read-modify-write, so no count is ever lost. And during a
//! claim the other kind's count can only fall, since raising it would need the
//! connection the claimer is holding, so a stale read is always too high and
//! refuses a claim that would in fact have been allowed. Erring toward refusal
//! is the safe direction, which is why the check-then-act does not need to be
//! one atomic step.

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ffi::{sqlite3, sqlite3_get_clientdata, sqlite3_set_clientdata, SQLITE_OK};

/// Key for this crate's record in the connection's client-data store.
const SLOT_KEY: &CStr = c"diesel-sqlite-session:preupdate-slot";

/// How many of each kind hold the slot on one connection.
#[derive(Default)]
struct Users {
    sessions: AtomicUsize,
    hooks: AtomicUsize,
}

/// Why a claim was turned down.
pub(crate) enum SlotDenied {
    /// The other kind already holds the slot.
    Occupied,
    /// `sqlite3_set_clientdata` could not allocate its record.
    OutOfMemory,
}

/// Destructor `SQLite` runs when the connection closes, and on a set call
/// that failed to allocate.
///
/// # Safety
///
/// `ptr` must be the `Box<Users>` leaked by [`users`], which is the only value
/// this crate ever stores under [`SLOT_KEY`].
unsafe extern "C" fn release(ptr: *mut c_void) {
    // SAFETY: the pointer came from `Box::into_raw` in `users` and `SQLite`
    // hands each stored value to this destructor exactly once, so the box is
    // still live and is not reclaimed anywhere else.
    drop(unsafe { Box::from_raw(ptr.cast::<Users>()) });
}

/// The counter record for `db`, created on first use.
///
/// # Safety
///
/// `db` must be a live connection handle, and the caller must hold the
/// `&mut SqliteConnection` it came from, which is what keeps two creations of
/// the record from racing.
unsafe fn users(db: *mut sqlite3) -> Result<*const Users, SlotDenied> {
    // SAFETY: `db` is live per the caller contract and `SLOT_KEY` is static.
    let existing = unsafe { sqlite3_get_clientdata(db, SLOT_KEY.as_ptr()) };
    if !existing.is_null() {
        return Ok(existing.cast::<Users>());
    }

    let fresh = Box::into_raw(Box::new(Users::default()));
    // SAFETY: `db` is live, and `fresh` is a leak this crate owns until
    // `release` reclaims it.
    let rc = unsafe {
        sqlite3_set_clientdata(db, SLOT_KEY.as_ptr(), fresh.cast::<c_void>(), Some(release))
    };
    if rc == SQLITE_OK {
        Ok(fresh)
    } else {
        // `SQLite` ran `release` on `fresh` before returning, so it is gone.
        Err(SlotDenied::OutOfMemory)
    }
}

/// Count one more session against `db`, unless a hook holds the slot.
///
/// # Safety
///
/// `db` must be a live connection handle and the caller must hold the
/// `&mut SqliteConnection` it came from. See the module's concurrency note.
pub(crate) unsafe fn claim_session(db: *mut sqlite3) -> Result<(), SlotDenied> {
    // SAFETY: `db` is live per the caller contract, and the record lives until
    // the connection closes.
    let users = unsafe { &*users(db)? };
    if users.hooks.load(Ordering::Acquire) > 0 {
        return Err(SlotDenied::Occupied);
    }
    users.sessions.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

/// Count one more hook against `db`, unless a session holds the slot.
///
/// # Safety
///
/// Same contract as [`claim_session`].
pub(crate) unsafe fn claim_hook(db: *mut sqlite3) -> Result<(), SlotDenied> {
    // SAFETY: `db` is live per the caller contract, and the record lives until
    // the connection closes.
    let users = unsafe { &*users(db)? };
    if users.sessions.load(Ordering::Acquire) > 0 {
        return Err(SlotDenied::Occupied);
    }
    users.hooks.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

/// Give back a session's hold. Callable from any thread, and from `Drop`.
///
/// # Safety
///
/// `db` must be a live connection handle.
pub(crate) unsafe fn release_session(db: *mut sqlite3) {
    // SAFETY: caller contract.
    unsafe { give_back(db, |users| &users.sessions) }
}

/// Give back a hook's hold. Callable from any thread, and from `Drop`.
///
/// # Safety
///
/// `db` must be a live connection handle.
pub(crate) unsafe fn release_hook(db: *mut sqlite3) {
    // SAFETY: caller contract.
    unsafe { give_back(db, |users| &users.hooks) }
}

/// Shared body of both releases. Reads the record rather than creating one, so
/// it cannot allocate and cannot fail, which is what `Drop` needs. Saturates
/// rather than wrapping: each guard releases exactly once, so an underflow
/// would be a bug in this crate, and wrapping would block the other kind
/// forever.
///
/// # Safety
///
/// `db` must be a live connection handle.
unsafe fn give_back(db: *mut sqlite3, pick: impl FnOnce(&Users) -> &AtomicUsize) {
    // SAFETY: `db` is live per the caller contract.
    let ptr = unsafe { sqlite3_get_clientdata(db, SLOT_KEY.as_ptr()) }.cast::<Users>();
    if ptr.is_null() {
        return;
    }
    // SAFETY: the record is only ever written by `users` and lives until the
    // connection closes, which the caller outlives.
    let users = unsafe { &*ptr };
    let counter = pick(users);
    // Decrement only while non-zero. `fetch_sub` alone would wrap on an
    // unbalanced release and block the other kind forever, and the saturating
    // helpers on atomics postdate this crate's minimum supported Rust.
    let mut held = counter.load(Ordering::Acquire);
    while held > 0 {
        match counter.compare_exchange_weak(held, held - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(seen) => held = seen,
        }
    }
}
