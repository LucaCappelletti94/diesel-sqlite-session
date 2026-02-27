//! iOS-oriented smoke entrypoints for running core crate behavior through FFI.

use std::panic::{catch_unwind, AssertUnwindSafe};

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Text};

use crate::{ApplyError, ConflictAction, SessionError, SqliteSessionExt};

const SMOKE_OK: i32 = 0;
const SMOKE_FAILED: i32 = 1;
const SMOKE_PANICKED: i32 = 2;

type SmokeResult = Result<(), SmokeFailure>;

#[derive(Debug)]
enum SmokeFailure {
    Connection(diesel::ConnectionError),
    Diesel(diesel::result::Error),
    Session(SessionError),
    Apply(ApplyError),
    AssertionFailed,
}

impl From<diesel::ConnectionError> for SmokeFailure {
    fn from(value: diesel::ConnectionError) -> Self {
        Self::Connection(value)
    }
}

impl From<diesel::result::Error> for SmokeFailure {
    fn from(value: diesel::result::Error) -> Self {
        Self::Diesel(value)
    }
}

impl From<SessionError> for SmokeFailure {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

impl From<ApplyError> for SmokeFailure {
    fn from(value: ApplyError) -> Self {
        Self::Apply(value)
    }
}

fn assert_true(condition: bool) -> SmokeResult {
    if condition {
        Ok(())
    } else {
        Err(SmokeFailure::AssertionFailed)
    }
}

fn run_case(case: fn() -> SmokeResult) -> i32 {
    match catch_unwind(AssertUnwindSafe(case)) {
        Ok(Ok(())) => SMOKE_OK,
        Ok(Err(err)) => {
            touch_failure(&err);
            SMOKE_FAILED
        }
        Err(_) => SMOKE_PANICKED,
    }
}

fn touch_failure(failure: &SmokeFailure) {
    match failure {
        SmokeFailure::Connection(err) => {
            let _ = err;
        }
        SmokeFailure::Diesel(err) => {
            let _ = err;
        }
        SmokeFailure::Session(err) => {
            let _ = err;
        }
        SmokeFailure::Apply(err) => {
            let _ = err;
        }
        SmokeFailure::AssertionFailed => {}
    }
}

fn smoke_replication_roundtrip_case() -> SmokeResult {
    let mut source = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)").execute(&mut source)?;

    let mut session = source.create_session()?;
    session.attach_by_name("items")?;

    sql_query("INSERT INTO items (id, name) VALUES (1, 'alpha'), (2, 'beta')")
        .execute(&mut source)?;
    let patchset = session.patchset()?;
    assert_true(!patchset.is_empty())?;

    let mut replica = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)").execute(&mut replica)?;

    replica.apply_patchset(&patchset, |_| ConflictAction::Abort)?;

    let count: i64 = sql::<BigInt>("SELECT COUNT(*) FROM items").get_result(&mut replica)?;
    let second_name: String =
        sql::<Text>("SELECT name FROM items WHERE id = 2").get_result(&mut replica)?;

    assert_true(count == 2)?;
    assert_true(second_name == "beta")
}

fn smoke_conflict_abort_case() -> SmokeResult {
    let mut source = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)").execute(&mut source)?;

    let mut session = source.create_session()?;
    session.attach_by_name("docs")?;
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'source')").execute(&mut source)?;
    let patchset = session.patchset()?;

    let mut replica = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)").execute(&mut replica)?;
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'replica')").execute(&mut replica)?;

    let apply_result = replica.apply_patchset(&patchset, |_| ConflictAction::Abort);
    assert_true(matches!(apply_result, Err(ApplyError::ConflictAborted)))?;

    let body: String =
        sql::<Text>("SELECT body FROM docs WHERE id = 1").get_result(&mut replica)?;
    assert_true(body == "replica")
}

fn smoke_invalid_table_name_case() -> SmokeResult {
    let mut conn = SqliteConnection::establish(":memory:")?;
    let mut session = conn.create_session()?;
    let attach_result = session.attach_by_name("bad\0table");
    assert_true(matches!(attach_result, Err(SessionError::InvalidTableName)))
}

fn smoke_conflict_handler_panic_case() -> SmokeResult {
    let mut source = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)").execute(&mut source)?;

    let mut session = source.create_session()?;
    session.attach_by_name("docs")?;
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'source')").execute(&mut source)?;
    let patchset = session.patchset()?;

    let mut replica = SqliteConnection::establish(":memory:")?;
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)").execute(&mut replica)?;
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'replica')").execute(&mut replica)?;

    let apply_result = replica.apply_patchset(&patchset, |_| -> ConflictAction {
        panic!("intentional panic for smoke test")
    });

    assert_true(matches!(
        apply_result,
        Err(ApplyError::ConflictHandlerPanicked)
    ))
}

/// Run replication roundtrip smoke behavior and return a process-safe status code.
#[no_mangle]
pub extern "C" fn diesel_sqlite_session_smoke_replication_roundtrip() -> i32 {
    run_case(smoke_replication_roundtrip_case)
}

/// Run conflict-abort smoke behavior and return a process-safe status code.
#[no_mangle]
pub extern "C" fn diesel_sqlite_session_smoke_conflict_abort() -> i32 {
    run_case(smoke_conflict_abort_case)
}

/// Run invalid table-name validation smoke behavior and return a process-safe status code.
#[no_mangle]
pub extern "C" fn diesel_sqlite_session_smoke_invalid_table_name() -> i32 {
    run_case(smoke_invalid_table_name_case)
}

/// Run panic-handling smoke behavior and return a process-safe status code.
#[no_mangle]
pub extern "C" fn diesel_sqlite_session_smoke_conflict_handler_panic_maps_error() -> i32 {
    run_case(smoke_conflict_handler_panic_case)
}
