//! Integration tests for sessions opened on a database other than `main`.
//!
//! A session records the database it was opened on, and every table name it is
//! later given resolves against that database. Before `create_session_on`
//! existed there was no way to reach an `ATTACH`ed one, and the failure was
//! silent: `diff` reported success and handed back an empty patchset, or worse,
//! the contents of a same-named table in `main`.
//!
//! One test per invariant. Each test starts with a fresh in-memory connection
//! so state cannot leak between cases.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ChangesetColumnType, ChangesetOp, ChangesetReader, SessionError, SqliteSessionExt,
};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn run(conn: &mut SqliteConnection, statements: &[&str]) {
    for statement in statements {
        sql_query(*statement)
            .execute(conn)
            .unwrap_or_else(|e| panic!("run {statement}: {e}"));
    }
}

/// Every text value carried by the new side of a changeset or patchset, in
/// iteration order. Integer columns are skipped so a row reads as its body.
fn texts(changes: &[u8]) -> Vec<String> {
    let mut reader = ChangesetReader::open(changes).expect("open reader");
    let mut found = Vec::new();
    while let Some(row) = reader.next().expect("advance reader") {
        if row.op() == ChangesetOp::Delete {
            continue;
        }
        for index in 0..u32::try_from(row.column_count()).expect("column count fits u32") {
            let Some(value) = row.new_value(index).expect("read new value") else {
                continue;
            };
            if value.column_type() == ChangesetColumnType::Text {
                found.push(value.as_text().expect("text column decodes").to_owned());
            }
        }
    }
    found
}

#[test]
fn a_session_on_an_attached_database_diffs_its_own_table() {
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "ATTACH DATABASE ':memory:' AS blank",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
            "CREATE TABLE blank.notes (id INTEGER PRIMARY KEY, body TEXT)",
            "INSERT INTO side.notes VALUES (1, 'draft')",
        ],
    );

    let mut session = conn.create_session_on("side").expect("session on side");
    session.attach_by_name("notes").expect("attach notes");
    session.diff("blank", "notes").expect("diff against blank");

    let patchset = session.patchset().expect("patchset");
    assert!(
        !patchset.is_empty(),
        "the attached table holds a row the empty twin does not",
    );
    assert_eq!(texts(&patchset), vec!["draft".to_owned()]);
}

#[test]
fn a_session_on_an_attached_database_ignores_a_same_named_table_in_main() {
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "ATTACH DATABASE ':memory:' AS blank",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
            "CREATE TABLE blank.notes (id INTEGER PRIMARY KEY, body TEXT)",
            "INSERT INTO notes VALUES (9, 'this-row-is-in-main')",
            "INSERT INTO side.notes VALUES (1, 'this-row-is-the-one-wanted')",
        ],
    );

    let mut session = conn.create_session_on("side").expect("session on side");
    session.attach_by_name("notes").expect("attach notes");
    session.diff("blank", "notes").expect("diff against blank");

    let patchset = session.patchset().expect("patchset");
    assert_eq!(
        texts(&patchset),
        vec!["this-row-is-the-one-wanted".to_owned()],
    );
}

#[test]
fn a_session_on_an_attached_database_records_live_writes() {
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
        ],
    );

    let mut session = conn.create_session_on("side").expect("session on side");
    session.attach_all().expect("attach every table");
    run(
        &mut conn,
        &[
            "INSERT INTO notes VALUES (1, 'from-main')",
            "INSERT INTO side.notes VALUES (2, 'from-side')",
        ],
    );

    let changeset = session.changeset().expect("changeset");
    assert_eq!(texts(&changeset), vec!["from-side".to_owned()]);
}

#[test]
fn create_session_still_records_main_only() {
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
        ],
    );

    let mut session = conn.create_session().expect("session on main");
    session.attach_all().expect("attach every table");
    run(
        &mut conn,
        &[
            "INSERT INTO notes VALUES (1, 'from-main')",
            "INSERT INTO side.notes VALUES (2, 'from-side')",
        ],
    );

    let changeset = session.changeset().expect("changeset");
    assert_eq!(texts(&changeset), vec!["from-main".to_owned()]);
}

#[test]
fn a_database_name_resolves_case_insensitively_like_sqlite() {
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
        ],
    );

    let mut session = conn.create_session_on("SIDE").expect("session on SIDE");
    session.attach_all().expect("attach every table");
    run(&mut conn, &["INSERT INTO side.notes VALUES (1, 'draft')"]);

    assert_eq!(
        texts(&session.changeset().expect("changeset")),
        vec!["draft".to_owned()],
    );
}

#[test]
fn a_session_opens_on_temp_before_any_temporary_table_exists() {
    let mut conn = fresh_connection();

    let mut session = conn.create_session_on("temp").expect("session on temp");
    session.attach_all().expect("attach every table");
    run(
        &mut conn,
        &[
            "CREATE TEMPORARY TABLE scratch (id INTEGER PRIMARY KEY, body TEXT)",
            "INSERT INTO scratch VALUES (1, 'jotting')",
        ],
    );

    assert_eq!(
        texts(&session.changeset().expect("changeset")),
        vec!["jotting".to_owned()],
    );
}

#[test]
fn detaching_the_database_under_a_live_session_reports_errors() {
    // Detaching is the likeliest way to invalidate a session opened with
    // `create_session_on`. It must degrade to errors rather than to the silent
    // empty result this whole entry point exists to remove, and the session
    // must still be safe to drop.
    let mut conn = fresh_connection();
    run(
        &mut conn,
        &[
            "ATTACH DATABASE ':memory:' AS side",
            "CREATE TABLE side.notes (id INTEGER PRIMARY KEY, body TEXT)",
        ],
    );

    let mut session = conn.create_session_on("side").expect("session on side");
    session.attach_all().expect("attach every table");
    run(&mut conn, &["INSERT INTO side.notes VALUES (1, 'before')"]);
    run(&mut conn, &["DETACH DATABASE side"]);

    assert!(!session.is_empty(), "the recorded change is still held");
    assert_eq!(session.database(), "side");
    assert!(
        matches!(session.changeset(), Err(SessionError::ChangesetFailed(_))),
        "a changeset over a departed database is an error, not an empty buffer",
    );
    assert!(
        matches!(
            session.diff("main", "notes"),
            Err(SessionError::DiffFailed { .. })
        ),
        "so is a diff",
    );

    drop(session);
    conn.create_session()
        .expect("the slot came back after the session was dropped");
}

#[test]
fn a_session_on_a_database_that_is_not_attached_fails_at_construction() {
    let mut conn = fresh_connection();

    let err = conn.create_session_on("side").unwrap_err();
    assert!(
        matches!(&err, SessionError::UnknownDatabase(name) if name == "side"),
        "{err:?}",
    );
}

#[test]
fn a_database_name_carrying_a_null_byte_is_rejected() {
    let mut conn = fresh_connection();

    let err = conn.create_session_on("si\0de").unwrap_err();
    assert!(matches!(err, SessionError::InvalidDatabaseName), "{err:?}");
}
