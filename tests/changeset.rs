//! Integration tests for the changeset iterator wrapper.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ChangesetColumnType, ChangesetError, ChangesetOp, ChangesetReader, SqliteSessionExt,
};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn make_changeset<F>(setup_sql: &[&str], mutate: F) -> Vec<u8>
where
    F: FnOnce(&mut SqliteConnection),
{
    let mut conn = fresh_connection();
    for stmt in setup_sql {
        sql_query(*stmt).execute(&mut conn).unwrap();
    }
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    mutate(&mut conn);
    session.changeset().unwrap()
}

#[test]
fn iterating_an_insert_changeset_yields_new_values_only() {
    let bytes = make_changeset(
        &["CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)"],
        |conn| {
            sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Widget', 42)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Insert);
    assert_eq!(row.table(), "items");
    assert_eq!(row.column_count(), 3);
    assert!(!row.indirect());

    // Old values are unavailable on INSERT.
    assert!(matches!(
        row.old_value(0).unwrap_err(),
        ChangesetError::OldNotAvailableOnInsert,
    ));

    let id = row.new_value(0).unwrap().expect("id present");
    assert_eq!(id.column_type(), ChangesetColumnType::Integer);
    assert_eq!(id.as_i64(), 1);
    let name = row.new_value(1).unwrap().expect("name present");
    assert_eq!(name.as_text(), Some("Widget"));
    let qty = row.new_value(2).unwrap().expect("quantity present");
    assert_eq!(qty.as_i64(), 42);

    // Iterator is exhausted.
    assert!(reader.next().expect("terminate").is_none());
}

#[test]
fn iterating_an_update_changeset_reports_unchanged_columns_as_none() {
    let bytes = make_changeset(
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)",
            "INSERT INTO items (id, name, quantity) VALUES (1, 'Widget', 10)",
        ],
        |conn| {
            sql_query("UPDATE items SET quantity = 55 WHERE id = 1")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Update);

    // Primary key column carries old value (SQLite always records the PK).
    let id_old = row.old_value(0).unwrap().expect("id old value");
    assert_eq!(id_old.as_i64(), 1);
    // Name did not change: both old and new sides report None.
    assert!(
        row.old_value(1).unwrap().is_none(),
        "unchanged name -> None"
    );
    assert!(
        row.new_value(1).unwrap().is_none(),
        "unchanged name -> None"
    );
    // Quantity changed: both sides carry the pre-image and post-image.
    let qty_old = row.old_value(2).unwrap().expect("quantity old");
    let qty_new = row.new_value(2).unwrap().expect("quantity new");
    assert_eq!(qty_old.as_i64(), 10);
    assert_eq!(qty_new.as_i64(), 55);
}

#[test]
fn iterating_a_delete_changeset_yields_old_values_only() {
    let bytes = make_changeset(
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)",
            "INSERT INTO items (id, name, quantity) VALUES (1, 'Doomed', 99)",
        ],
        |conn| {
            sql_query("DELETE FROM items WHERE id = 1")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Delete);

    assert!(matches!(
        row.new_value(0).unwrap_err(),
        ChangesetError::NewNotAvailableOnDelete,
    ));
    let name = row.old_value(1).unwrap().expect("name old value");
    assert_eq!(name.as_text(), Some("Doomed"));
    let qty = row.old_value(2).unwrap().expect("quantity old value");
    assert_eq!(qty.as_i64(), 99);
}

#[test]
fn iterating_reports_primary_key_layout() {
    let bytes = make_changeset(
        &["CREATE TABLE composite (a INTEGER, b TEXT NOT NULL, c INTEGER, PRIMARY KEY (a, b))"],
        |conn| {
            sql_query("INSERT INTO composite (a, b, c) VALUES (1, 'k', 100)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Insert);
    assert_eq!(row.column_count(), 3);
    assert!(row.is_primary_key(0).unwrap(), "column a is part of PK");
    assert!(row.is_primary_key(1).unwrap(), "column b is part of PK");
    assert!(
        !row.is_primary_key(2).unwrap(),
        "column c is NOT part of PK",
    );
}

#[test]
fn is_primary_key_out_of_range_returns_column_out_of_range() {
    let bytes = make_changeset(
        &["CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)"],
        |conn| {
            sql_query("INSERT INTO t (v) VALUES (1)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    match row.is_primary_key(9) {
        Err(ChangesetError::ColumnOutOfRange { index, count }) => {
            assert_eq!(index, 9);
            assert_eq!(count, 2);
        }
        other => panic!("expected ColumnOutOfRange, got {other:?}"),
    }
}

#[test]
fn new_value_out_of_range_returns_column_out_of_range() {
    let bytes = make_changeset(
        &["CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)"],
        |conn| {
            sql_query("INSERT INTO t (v) VALUES (1)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let row = reader.next().expect("advance").expect("saw a row");
    match row.new_value(9) {
        Err(ChangesetError::ColumnOutOfRange { index, count }) => {
            assert_eq!(index, 9);
            assert_eq!(count, 2);
        }
        other => panic!("expected ColumnOutOfRange, got {other:?}"),
    }
}

#[test]
fn iterator_walks_multiple_ops_in_order() {
    let bytes = make_changeset(
        &["CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)"],
        |conn| {
            sql_query("INSERT INTO t (id, v) VALUES (1, 10), (2, 20), (3, 30)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let mut rowids = Vec::new();
    while let Some(row) = reader.next().expect("advance") {
        assert_eq!(row.op(), ChangesetOp::Insert);
        rowids.push(row.new_value(0).unwrap().expect("id").as_i64());
    }
    assert_eq!(rowids, vec![1, 2, 3]);
}

#[test]
fn iterator_covers_multiple_tables() {
    let bytes = make_changeset(
        &[
            "CREATE TABLE first (id INTEGER PRIMARY KEY, v INTEGER)",
            "CREATE TABLE second (id INTEGER PRIMARY KEY, v INTEGER)",
        ],
        |conn| {
            sql_query("INSERT INTO first (id, v) VALUES (1, 10)")
                .execute(conn)
                .unwrap();
            sql_query("INSERT INTO second (id, v) VALUES (7, 70)")
                .execute(conn)
                .unwrap();
        },
    );

    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let mut tables: Vec<String> = Vec::new();
    while let Some(row) = reader.next().expect("advance") {
        tables.push(row.table().to_owned());
    }
    tables.sort();
    assert_eq!(tables, vec!["first".to_string(), "second".to_string()]);
}

#[test]
fn inverted_reader_swaps_insert_and_delete() {
    let bytes = make_changeset(
        &["CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)"],
        |conn| {
            sql_query("INSERT INTO t (id, v) VALUES (1, 10)")
                .execute(conn)
                .unwrap();
        },
    );

    // Normal iteration observes INSERT.
    let mut reader = ChangesetReader::open(&bytes).expect("open reader");
    let normal_op = reader.next().unwrap().expect("saw a row").op();
    assert_eq!(normal_op, ChangesetOp::Insert);
    drop(reader);

    // Inverted iteration observes DELETE.
    let mut reader = ChangesetReader::open_inverted(&bytes).expect("open inverted reader");
    let inverted_row = reader.next().expect("advance").expect("saw a row");
    assert_eq!(inverted_row.op(), ChangesetOp::Delete);
    // The inverted DELETE carries the old values that used to be new.
    let id = inverted_row.old_value(0).unwrap().expect("id");
    assert_eq!(id.as_i64(), 1);
    let v = inverted_row.old_value(1).unwrap().expect("v");
    assert_eq!(v.as_i64(), 10);
}

#[test]
fn open_on_empty_buffer_returns_empty_changeset_error() {
    let err = ChangesetReader::open(&[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}
#[test]
fn advancing_a_garbage_changeset_returns_next_failed() {
    // `sqlite3changeset_start` may accept a syntactically minimal buffer,
    // deferring corruption detection to `sqlite3changeset_next`. Assert that
    // the wrapper still surfaces an error, whichever call happens to catch
    // the corruption first.
    let bytes = [0xDEu8, 0xAD, 0xBE, 0xEF];
    let opened = ChangesetReader::open(&bytes);
    match opened {
        Err(ChangesetError::StartFailed(_)) => {}
        Err(other) => panic!("expected StartFailed or NextFailed, got {other:?}"),
        Ok(mut reader) => match reader.next() {
            Err(ChangesetError::NextFailed(_)) => {}
            other => panic!("expected NextFailed on garbage, got {other:?}"),
        },
    }
}
