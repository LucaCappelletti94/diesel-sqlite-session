//! Integration tests for the changeset transform helpers: `invert_changeset`,
//! `concat_changesets`, and `Changegroup`.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    concat_changesets, invert_changeset, ApplyFlags, Changegroup, ChangesetError, ChangesetOp,
    ChangesetReader, ConflictAction, SqliteSessionExt,
};

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

fn item_value(conn: &mut SqliteConnection, id: i64) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>(&format!("SELECT v FROM items WHERE id = {id}"))
        .get_result(conn)
        .unwrap()
}

fn make_insert_changeset(rows: &[(i64, i64)]) -> Vec<u8> {
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
fn invert_flips_insert_into_delete_and_round_trips_via_apply() {
    let bytes = make_insert_changeset(&[(1, 10), (2, 20)]);
    let inverted = invert_changeset(&bytes).expect("invert succeeds");
    assert!(!inverted.is_empty());

    // Iterate the inverted bytes: both rows must show up as DELETE ops.
    let mut reader = ChangesetReader::open(&inverted).expect("open inverted");
    let mut ops: Vec<ChangesetOp> = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        ops.push(row.op());
    }
    assert_eq!(ops, vec![ChangesetOp::Delete, ChangesetOp::Delete]);
    drop(reader);

    // Apply original, then inverted, on a fresh replica: state matches empty.
    let mut replica = fresh_connection();
    create_items(&mut replica);
    replica
        .apply_changeset(&bytes, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_items(&mut replica), 2);
    replica
        .apply_changeset(&inverted, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_items(&mut replica), 0);
}

#[test]
fn invert_empty_changeset_returns_empty_changeset_error() {
    let err = invert_changeset(&[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}

#[test]
fn invert_garbage_changeset_returns_invert_failed() {
    let err = invert_changeset(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap_err();
    assert!(matches!(err, ChangesetError::InvertFailed(_)), "{err:?}");
}

#[test]
fn concat_merges_two_disjoint_changesets() {
    let a = make_insert_changeset(&[(1, 10)]);
    let b = make_insert_changeset(&[(2, 20)]);
    let concatenated = concat_changesets(&a, &b).expect("concat succeeds");

    let mut replica = fresh_connection();
    create_items(&mut replica);
    replica
        .apply_changeset(&concatenated, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_items(&mut replica), 2);
    assert_eq!(item_value(&mut replica, 1), 10);
    assert_eq!(item_value(&mut replica, 2), 20);
}

#[test]
fn concat_empty_side_returns_empty_changeset_error() {
    let a = make_insert_changeset(&[(1, 10)]);
    let err = concat_changesets(&a, &[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
    let err = concat_changesets(&[], &a).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}

#[test]
fn changegroup_aggregates_multiple_changesets() {
    let mut group = Changegroup::new().expect("new group");
    group.add(&make_insert_changeset(&[(1, 10)])).unwrap();
    group.add(&make_insert_changeset(&[(2, 20)])).unwrap();
    group.add(&make_insert_changeset(&[(3, 30)])).unwrap();
    let merged = group.output().expect("output succeeds");
    assert!(!merged.is_empty());

    let mut replica = fresh_connection();
    create_items(&mut replica);
    replica
        .apply_changeset(&merged, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_items(&mut replica), 3);
    assert_eq!(item_value(&mut replica, 1), 10);
    assert_eq!(item_value(&mut replica, 2), 20);
    assert_eq!(item_value(&mut replica, 3), 30);
}

#[test]
fn changegroup_collapses_insert_then_update_into_single_insert() {
    // Session A on a fresh DB: INSERT id=1 v=10.
    let insert_bytes = make_insert_changeset(&[(1, 10)]);

    // Session B on a DB that already has id=1 v=10: UPDATE it to v=99.
    let mut second = fresh_connection();
    create_items(&mut second);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut second)
        .unwrap();
    let mut session = second.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("UPDATE items SET v = 99 WHERE id = 1")
        .execute(&mut second)
        .unwrap();
    let update_bytes = session.changeset().unwrap();
    drop(session);

    let mut group = Changegroup::new().unwrap();
    group.add(&insert_bytes).unwrap();
    group.add(&update_bytes).unwrap();
    let merged = group.output().unwrap();

    // Iterate the merged output: exactly one INSERT with the final value.
    let mut reader = ChangesetReader::open(&merged).unwrap();
    let mut ops: Vec<(ChangesetOp, i64)> = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        let v = row.new_value(1).unwrap().unwrap().as_i64();
        ops.push((row.op(), v));
    }
    assert_eq!(ops, vec![(ChangesetOp::Insert, 99)]);
}

#[test]
fn changegroup_add_empty_changeset_returns_empty_error() {
    let mut group = Changegroup::new().unwrap();
    let err = group.add(&[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}

#[test]
fn changegroup_output_is_idempotent() {
    let mut group = Changegroup::new().unwrap();
    group.add(&make_insert_changeset(&[(1, 10)])).unwrap();
    let first = group.output().unwrap();
    let second = group.output().unwrap();
    assert_eq!(first, second, "output twice must yield the same bytes");
}

#[test]
fn changegroup_set_schema_binds_a_database() {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut group = Changegroup::new().unwrap();
    group
        .set_schema(&mut conn, "main")
        .expect("set_schema succeeds on the main database");

    // Adding a rowid-table changeset must still work with a schema attached.
    group.add(&make_insert_changeset(&[(1, 10)])).unwrap();
    let merged = group.output().unwrap();
    assert!(!merged.is_empty());
}

#[test]
fn changegroup_set_schema_rejects_null_byte_in_name() {
    let mut conn = fresh_connection();
    let mut group = Changegroup::new().unwrap();
    let err = group.set_schema(&mut conn, "ma\0in").unwrap_err();
    assert!(matches!(err, ChangesetError::InvalidSchemaName), "{err:?}");
}

#[test]
fn concat_matches_two_changegroup_add_calls() {
    // Given the same two changesets, concat and Changegroup ought to produce
    // functionally equivalent outputs (byte-equal is not guaranteed by SQLite,
    // so we compare via applied state).
    let a = make_insert_changeset(&[(1, 10)]);
    let b = make_insert_changeset(&[(2, 20)]);

    let concatenated = concat_changesets(&a, &b).unwrap();

    let mut group = Changegroup::new().unwrap();
    group.add(&a).unwrap();
    group.add(&b).unwrap();
    let grouped = group.output().unwrap();

    let mut replica_a = fresh_connection();
    create_items(&mut replica_a);
    replica_a
        .apply_changeset(&concatenated, |_| ConflictAction::Abort)
        .unwrap();

    let mut replica_b = fresh_connection();
    create_items(&mut replica_b);
    replica_b
        .apply_changeset(&grouped, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_items(&mut replica_a), count_items(&mut replica_b));
    assert_eq!(item_value(&mut replica_a, 1), item_value(&mut replica_b, 1));
    assert_eq!(item_value(&mut replica_a, 2), item_value(&mut replica_b, 2));
}

#[test]
fn invert_via_apply_v2_invert_flag_matches_pre_inverted_bytes() {
    // Apply(bytes, INVERT) must have the same replica effect as
    // Apply(invert(bytes), no flag).
    let bytes = make_insert_changeset(&[(1, 10)]);
    let inverted = invert_changeset(&bytes).unwrap();

    let mut replica_a = fresh_connection();
    create_items(&mut replica_a);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut replica_a)
        .unwrap();
    replica_a
        .apply_changeset_with(
            &bytes,
            ApplyFlags::INVERT,
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap();

    let mut replica_b = fresh_connection();
    create_items(&mut replica_b);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut replica_b)
        .unwrap();
    replica_b
        .apply_changeset(&inverted, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_items(&mut replica_a), 0);
    assert_eq!(count_items(&mut replica_b), 0);
}
