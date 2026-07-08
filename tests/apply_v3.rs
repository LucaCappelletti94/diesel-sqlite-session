//! Integration tests for `SqliteSessionExt::apply_changeset_v3_with` and
//! `SqliteSessionExt::apply_changeset_v3_strm_with`.

use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ApplyFlags, ChangesetOp, ChangesetRow, ConflictAction, SqliteSessionExt,
};
use parking_lot::Mutex;

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn create_items(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

fn count_items(conn: &mut SqliteConnection) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM items")
        .get_result(conn)
        .unwrap()
}

fn make_changeset(rows: &[(i64, i64)]) -> Vec<u8> {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    for (id, v) in rows {
        sql_query(format!("INSERT INTO items (id, v) VALUES ({id}, {v})"))
            .execute(&mut conn)
            .unwrap();
    }
    session.changeset().unwrap()
}

#[test]
fn v3_filter_sees_op_and_table() {
    let bytes = make_changeset(&[(1, 10), (2, 20)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);

    let observed: Arc<Mutex<Vec<(ChangesetOp, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            move |row: ChangesetRow<'_>| {
                sink.lock().push((row.op(), row.table().to_owned()));
                true
            },
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds");

    let seen = observed.lock().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen
        .iter()
        .all(|(op, table)| *op == ChangesetOp::Insert && table == "items"));
    assert_eq!(count_items(&mut replica), 2);
}

#[test]
fn v3_filter_can_skip_rows_by_new_value() {
    // Filter out rows whose new `v` is even. The changeset inserts two rows;
    // only the odd one lands on the replica.
    let bytes = make_changeset(&[(1, 11), (2, 20)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);

    replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            |row: ChangesetRow<'_>| {
                if row.op() == ChangesetOp::Insert {
                    let v = row.new_value(1).unwrap().unwrap().as_i64();
                    v % 2 == 1
                } else {
                    true
                }
            },
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds");

    let v: i64 = diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT v FROM items WHERE id = 1")
        .get_result(&mut replica)
        .unwrap();
    assert_eq!(v, 11, "odd row landed");
    assert_eq!(count_items(&mut replica), 1, "even row was filtered out");
}

#[test]
fn v3_filter_reads_primary_key_layout() {
    // Build a changeset with a composite PK.
    let mut source = fresh_connection();
    sql_query("CREATE TABLE composite (a INTEGER, b TEXT NOT NULL, c INTEGER, PRIMARY KEY (a, b))")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO composite (a, b, c) VALUES (1, 'k', 100)")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut replica = fresh_connection();
    sql_query("CREATE TABLE composite (a INTEGER, b TEXT NOT NULL, c INTEGER, PRIMARY KEY (a, b))")
        .execute(&mut replica)
        .unwrap();

    let observed: Arc<Mutex<Vec<Vec<bool>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            move |row: ChangesetRow<'_>| {
                let mut mask = Vec::new();
                for i in 0..u32::try_from(row.column_count()).unwrap() {
                    mask.push(row.is_primary_key(i).unwrap());
                }
                sink.lock().push(mask);
                true
            },
            |_| ConflictAction::Abort,
        )
        .unwrap();

    let masks = observed.lock().clone();
    assert_eq!(masks, vec![vec![true, true, false]]);
}

#[test]
fn v3_filter_returning_false_skips_the_change() {
    let bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            |_| false,
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds even when everything is filtered out");
    assert_eq!(count_items(&mut replica), 0);
}

#[test]
fn v3_filter_is_called_once_per_row() {
    let bytes = make_changeset(&[(1, 10), (2, 20), (3, 30)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);

    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            move |_| {
                sink.fetch_add(1, Ordering::SeqCst);
                true
            },
            |_| ConflictAction::Abort,
        )
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[test]
fn v3_filter_panic_yields_filter_panicked_error() {
    let bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_v3_with(
            &bytes,
            ApplyFlags::empty(),
            |_| panic!("v3 filter boom"),
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(
        matches!(err, diesel_sqlite_session::ApplyError::FilterPanicked),
        "got {err:?}",
    );
}

#[test]
fn v3_strm_applies_a_streamed_changeset() {
    let bytes = make_changeset(&[(1, 10), (2, 20)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);

    replica
        .apply_changeset_v3_strm_with(
            Cursor::new(bytes),
            ApplyFlags::empty(),
            |row: ChangesetRow<'_>| row.table() == "items",
            |_| ConflictAction::Abort,
        )
        .expect("streamed v3 apply succeeds");
    assert_eq!(count_items(&mut replica), 2);
}

#[test]
fn v3_strm_reader_panic_yields_reader_panicked_error() {
    struct PanicRead;
    impl std::io::Read for PanicRead {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            panic!("v3 strm reader boom");
        }
    }

    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_v3_strm_with(
            PanicRead,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(
        matches!(err, diesel_sqlite_session::ApplyError::ReaderPanicked),
        "got {err:?}",
    );
}
