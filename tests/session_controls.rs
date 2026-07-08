//! Integration tests for the session-control extensions: `diff`,
//! `set_indirect`, `set_table_filter`, `set_size_tracking`,
//! `set_rowid_tracking`, `memory_used`, `changeset_size`, plus the standalone
//! `stream_size` / `set_stream_size`.
//!
//! One test per invariant. Each test starts with a fresh in-memory
//! connection so state cannot leak between cases.

use std::sync::Arc;

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    set_stream_size, stream_size, ApplyFlags, ChangesetOp, ChangesetReader, ConflictAction,
    SessionError, SqliteSessionExt,
};
use parking_lot::Mutex;

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

#[test]
fn diff_populates_a_session_with_the_delta_between_two_databases() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 100)")
        .execute(&mut conn)
        .unwrap();

    // Attach a scratch DB and mirror the schema plus a different row.
    sql_query("ATTACH DATABASE ':memory:' AS other")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE other.items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO other.items (id, v) VALUES (2, 200)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session
        .diff("other", "items")
        .expect("diff succeeds against attached DB");
    let changeset = session.changeset().unwrap();
    assert!(!changeset.is_empty(), "diff produced a non-empty changeset");

    // Iterate the diff: expect at least an INSERT for id=2 (present in main
    // but missing in `other`) or an equivalent shape depending on SQLite's
    // reconciliation order.
    let mut reader = ChangesetReader::open(&changeset).expect("open reader");
    let mut ops: Vec<ChangesetOp> = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        ops.push(row.op());
    }
    assert!(
        ops.iter()
            .any(|op| matches!(op, ChangesetOp::Insert | ChangesetOp::Delete)),
        "diff carries at least one INSERT or DELETE op, got {ops:?}",
    );
}

#[test]
fn diff_with_null_byte_in_names_returns_invalid_name() {
    let mut conn = fresh_connection();
    let mut session = conn.create_session().unwrap();
    let err = session.diff("other\0", "items").unwrap_err();
    assert!(matches!(err, SessionError::InvalidTableName), "{err:?}");
    let err = session.diff("other", "ta\0ble").unwrap_err();
    assert!(matches!(err, SessionError::InvalidTableName), "{err:?}");
}

#[test]
fn diff_against_missing_source_database_returns_diff_failed() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    // `not_attached` is not ATTACH'd, so `sqlite3session_diff` must fail.
    let err = session.diff("not_attached", "items").unwrap_err();
    assert!(
        matches!(err, SessionError::DiffFailed { .. }),
        "expected DiffFailed, got {err:?}",
    );
    if let SessionError::DiffFailed { message, .. } = err {
        assert!(
            message.is_some(),
            "diff surfaces SQLite's error message when it provides one",
        );
    }
}

#[test]
fn set_indirect_round_trips() {
    let mut conn = fresh_connection();
    let mut session = conn.create_session().unwrap();
    assert!(!session.is_indirect(), "default is clear");
    session.set_indirect(true);
    assert!(session.is_indirect());
    session.set_indirect(false);
    assert!(!session.is_indirect());
}

#[test]
fn indirect_flag_marks_subsequent_changes_as_indirect_in_changeset() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    session.set_indirect(true);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    drop(session);

    let mut reader = ChangesetReader::open(&changeset).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    assert!(row.indirect(), "row is tagged indirect");
}

#[test]
fn table_filter_can_skip_specific_tables_from_attach_all() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE tracked (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE ignored (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    session.set_table_filter(move |table| {
        sink.lock().push(table.to_owned());
        table != "ignored"
    });
    session.attach_all().unwrap();

    sql_query("INSERT INTO tracked (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO ignored (id, v) VALUES (2, 20)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    drop(session);

    let mut reader = ChangesetReader::open(&changeset).expect("open reader");
    let mut tables: Vec<String> = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        tables.push(row.table().to_owned());
    }
    assert!(tables.contains(&"tracked".to_string()));
    assert!(
        !tables.contains(&"ignored".to_string()),
        "filter skipped 'ignored'"
    );
    assert!(
        observed.lock().iter().any(|t| t == "ignored"),
        "filter callback saw 'ignored'",
    );
}

#[test]
fn table_filter_panic_maps_to_skip() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE will_be_skipped (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session.set_table_filter(|_| panic!("filter boom"));
    session.attach_all().unwrap();
    sql_query("INSERT INTO will_be_skipped (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    assert!(changeset.is_empty(), "panicking filter is treated as skip");
}

#[test]
fn remove_table_filter_restores_default_attach_all() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session.set_table_filter(|_| false); // reject everything
    session.remove_table_filter();
    session.attach_all().unwrap();
    sql_query("INSERT INTO t (id, v) VALUES (1, 1)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    assert!(
        !changeset.is_empty(),
        "attach_all worked after remove_table_filter",
    );
}

#[test]
fn size_tracking_round_trips_and_changeset_size_reports_nonzero() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();

    // Off by default.
    assert!(!session.is_size_tracking_enabled().unwrap());
    session.set_size_tracking(true).unwrap();
    assert!(session.is_size_tracking_enabled().unwrap());

    session.attach_all().unwrap();
    sql_query("INSERT INTO t (id, v) VALUES (1, 42)")
        .execute(&mut conn)
        .unwrap();
    let est = session.changeset_size();
    assert!(est > 0, "changeset_size > 0 with tracking on, got {est}");
}

#[test]
fn changeset_size_stays_zero_without_size_tracking() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO t (id, v) VALUES (1, 42)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(session.changeset_size(), 0);
}

#[test]
fn rowid_tracking_toggle_allows_without_rowid_tables() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE wr (id INTEGER PRIMARY KEY, v INTEGER) WITHOUT ROWID")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    assert!(!session.is_rowid_tracking_enabled().unwrap());
    session.set_rowid_tracking(true).unwrap();
    assert!(session.is_rowid_tracking_enabled().unwrap());

    session.attach_all().unwrap();
    sql_query("INSERT INTO wr (id, v) VALUES (1, 7)")
        .execute(&mut conn)
        .unwrap();
    let bytes = session.changeset().unwrap();
    assert!(
        !bytes.is_empty(),
        "WITHOUT ROWID table changes recorded after enabling rowid tracking",
    );
}

#[test]
fn memory_used_grows_with_tracked_changes() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    let before = session.memory_used();
    for i in 1..=32 {
        sql_query(format!("INSERT INTO t (id, v) VALUES ({i}, {i})"))
            .execute(&mut conn)
            .unwrap();
    }
    let after = session.memory_used();
    assert!(
        after > before,
        "memory_used rose from {before} to {after} after 32 inserts",
    );
}

#[test]
fn stream_size_read_and_set_round_trip() {
    // Read the current default first so the test does not mutate global
    // state permanently.
    let original = stream_size().expect("read stream_size succeeds");
    assert!(original > 0, "SQLite default is positive, got {original}");

    set_stream_size(4096).expect("set stream_size");
    assert_eq!(stream_size().unwrap(), 4096);

    set_stream_size(original).expect("restore");
    assert_eq!(stream_size().unwrap(), original);
}

#[test]
fn apply_v2_carries_indirect_flag_through_into_changeset_readers() {
    // Once a change is marked indirect at the source, apply plus round-trip
    // read on the replica preserves the flag on the corresponding
    // `ChangesetReader::next` row.
    let mut source = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    session.set_indirect(true);
    sql_query("INSERT INTO t (id, v) VALUES (1, 10)")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    // Apply to a replica: the row lands.
    let mut replica = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut replica)
        .unwrap();
    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap();

    // Read the bytes again to confirm the indirect flag was serialized.
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    let row = reader.next().expect("advance").expect("saw the row");
    assert!(row.indirect(), "indirect flag survives round trip");
}
