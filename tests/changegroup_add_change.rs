//! Integration tests for `Changegroup::add_change`, which folds a single
//! positioned row from a `ChangesetReader` into the group without
//! re-serializing the whole changeset.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ApplyFlags, Changegroup, ChangesetOp, ChangesetReader, ConflictAction, SqliteSessionExt,
};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn create_items(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

fn record_changeset(rows: &[(i64, i64)]) -> Vec<u8> {
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
fn add_change_folds_selected_rows() {
    // Source records four INSERTs, we only fold the two whose id is even.
    let bytes = record_changeset(&[(1, 10), (2, 20), (3, 30), (4, 40)]);

    let mut group = Changegroup::new().unwrap();
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    while let Some(row) = reader.next().unwrap() {
        let id = row.new_value(0).unwrap().unwrap().as_i64();
        if id % 2 == 0 {
            group.add_change(&row).unwrap();
        }
    }
    let out = group.output().unwrap();
    drop(reader);
    assert!(!out.is_empty());

    // Apply the folded changeset to a fresh replica and confirm only the
    // even-id rows landed.
    let mut replica = fresh_connection();
    create_items(&mut replica);
    replica
        .apply_changeset_with(
            &out,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap();

    let ids: Vec<i64> =
        diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT id FROM items ORDER BY id")
            .load(&mut replica)
            .unwrap();
    assert_eq!(ids, vec![2, 4]);
}

#[test]
fn add_change_preserves_op_kinds() {
    // Build a source that INSERTs, UPDATEs, and DELETEs.
    let mut conn = fresh_connection();
    create_items(&mut conn);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10), (2, 20), (3, 30)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (4, 40)")
        .execute(&mut conn)
        .unwrap();
    sql_query("UPDATE items SET v = 200 WHERE id = 2")
        .execute(&mut conn)
        .unwrap();
    sql_query("DELETE FROM items WHERE id = 3")
        .execute(&mut conn)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut group = Changegroup::new().unwrap();
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    let mut ops = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        ops.push(row.op());
        group.add_change(&row).unwrap();
    }
    let out = group.output().unwrap();
    drop(reader);
    assert!(ops.contains(&ChangesetOp::Insert));
    assert!(ops.contains(&ChangesetOp::Update));
    assert!(ops.contains(&ChangesetOp::Delete));

    // Round-trip: reading the folded changeset back must recover the same
    // op multiset.
    let mut round = ChangesetReader::open(&out).unwrap();
    let mut round_ops = Vec::new();
    while let Some(row) = round.next().unwrap() {
        round_ops.push(row.op());
    }
    ops.sort_by_key(|op| *op as i32);
    round_ops.sort_by_key(|op| *op as i32);
    assert_eq!(ops, round_ops);
}

#[test]
fn add_change_equivalent_to_add_when_all_rows_are_kept() {
    let bytes = record_changeset(&[(1, 10), (2, 20), (3, 30)]);

    // Reference group folds the whole changeset at once.
    let mut reference = Changegroup::new().unwrap();
    reference.add(&bytes).unwrap();
    let reference_out = reference.output().unwrap();

    // Streamed-row group folds one row at a time.
    let mut per_row = Changegroup::new().unwrap();
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    while let Some(row) = reader.next().unwrap() {
        per_row.add_change(&row).unwrap();
    }
    let per_row_out = per_row.output().unwrap();
    drop(reader);

    // Byte equality would rely on SQLite emitting identical framing, which
    // is an implementation detail. Instead, apply both and assert the
    // resulting rows match.
    let mut r1 = fresh_connection();
    create_items(&mut r1);
    r1.apply_changeset_with(
        &reference_out,
        ApplyFlags::empty(),
        |_| true,
        |_| ConflictAction::Abort,
    )
    .unwrap();

    let mut r2 = fresh_connection();
    create_items(&mut r2);
    r2.apply_changeset_with(
        &per_row_out,
        ApplyFlags::empty(),
        |_| true,
        |_| ConflictAction::Abort,
    )
    .unwrap();

    let rows1: Vec<(i64, i64)> = diesel::dsl::sql::<(
        diesel::sql_types::BigInt,
        diesel::sql_types::BigInt,
    )>("SELECT id, v FROM items ORDER BY id")
    .load(&mut r1)
    .unwrap();
    let rows2: Vec<(i64, i64)> = diesel::dsl::sql::<(
        diesel::sql_types::BigInt,
        diesel::sql_types::BigInt,
    )>("SELECT id, v FROM items ORDER BY id")
    .load(&mut r2)
    .unwrap();
    assert_eq!(rows1, rows2);
}

#[test]
fn add_change_rejects_inverted_iterator() {
    // Inverted iterators are explicitly rejected by SQLite for add_change,
    // regardless of position.
    let bytes = record_changeset(&[(1, 10)]);
    let mut group = Changegroup::new().unwrap();
    let mut reader = ChangesetReader::open_inverted(&bytes).unwrap();
    let row = reader.next().unwrap().expect("row available");
    let err = group.add_change(&row).unwrap_err();
    assert!(
        matches!(
            err,
            diesel_sqlite_session::ChangesetError::ChangegroupAddFailed(_)
        ),
        "got {err:?}",
    );
}
