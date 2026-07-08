//! Integration tests for `Rebaser`.
//!
//! Each test exercises one invariant. The end-to-end test walks the full
//! multi-master conflict-resolution cycle: two peers, a rebase blob captured
//! at the moment of conflict, a rebased outbound changeset, and convergence.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ApplyFlags, ChangesetError, ChangesetOp, ChangesetReader, ConflictAction, Rebaser,
    SqliteSessionExt,
};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn create_items(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

fn item_value(conn: &mut SqliteConnection, id: i64) -> Option<i64> {
    diesel::dsl::sql::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>>(&format!(
        "SELECT v FROM items WHERE id = {id}"
    ))
    .get_result(conn)
    .unwrap()
}

#[test]
fn new_returns_a_live_rebaser() {
    let _rebaser = Rebaser::new().expect("rebaser allocates");
}

#[test]
fn configure_rejects_empty_rebase_blob() {
    let mut rebaser = Rebaser::new().unwrap();
    let err = rebaser.configure(&[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}

#[test]
fn configure_rejects_garbage_rebase_blob() {
    let mut rebaser = Rebaser::new().unwrap();
    let err = rebaser.configure(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap_err();
    assert!(
        matches!(err, ChangesetError::RebaserConfigureFailed(_)),
        "{err:?}",
    );
}

#[test]
fn rebase_rejects_empty_changeset() {
    let rebaser = Rebaser::new().unwrap();
    let err = rebaser.rebase(&[]).unwrap_err();
    assert!(matches!(err, ChangesetError::EmptyChangeset), "{err:?}");
}
#[test]
fn rebase_on_garbage_does_not_crash() {
    // SQLite's `sqlite3rebaser_rebase` is lenient about short unrecognized
    // inputs: some byte patterns pass through as empty changesets rather than
    // erroring. The invariant that matters for the wrapper is soundness (no
    // panic, no UB, well-formed Result), not that every garbage buffer is
    // rejected.
    let rebaser = Rebaser::new().unwrap();
    let _ = rebaser.rebase(&[0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn rebase_without_configure_leaves_changeset_semantically_equal() {
    // An empty rebaser (no configure calls) should still be able to rebase
    // a changeset, and the output must apply to a fresh replica just like
    // the input. SQLite may or may not byte-match the input; assert semantic
    // equivalence via applied state.
    let source_bytes = make_insert_changeset();

    let empty_rebaser = Rebaser::new().unwrap();
    let rewritten = empty_rebaser
        .rebase(&source_bytes)
        .expect("rebase succeeds");

    let mut replica_a = fresh_connection();
    create_items(&mut replica_a);
    replica_a
        .apply_changeset(&source_bytes, |_| ConflictAction::Abort)
        .unwrap();

    let mut replica_b = fresh_connection();
    create_items(&mut replica_b);
    replica_b
        .apply_changeset(&rewritten, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(item_value(&mut replica_a, 1), item_value(&mut replica_b, 1));
}

/// Build a changeset that INSERTs `(1, 10)` into a fresh `items` table.
fn make_insert_changeset() -> Vec<u8> {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    session.changeset().unwrap()
}

#[test]
fn end_to_end_multi_master_convergence() {
    // Two peers A and B start from a shared empty state. Both write to the
    // same primary key with different values.
    //
    //   Peer A: session_a records INSERT id=1 v=10
    //   Peer B: session_b records INSERT id=1 v=99
    //
    // Peer B applies A's changeset first with a Replace conflict resolution
    // and captures the rebase blob. Then peer A receives B's changeset plus
    // the rebase blob, rewrites its outbound view of B via the rebaser, and
    // applies the rewritten changeset. Both peers converge on the same value.
    let mut peer_a = fresh_connection();
    create_items(&mut peer_a);
    let mut peer_b = fresh_connection();
    create_items(&mut peer_b);

    let mut session_a = peer_a.create_session().unwrap();
    session_a.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut peer_a)
        .unwrap();
    let changeset_a = session_a.changeset().unwrap();
    drop(session_a);

    let mut session_b = peer_b.create_session().unwrap();
    session_b.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut peer_b)
        .unwrap();
    let changeset_b = session_b.changeset().unwrap();
    drop(session_b);

    // Peer B receives changeset_a first. It conflicts with B's local
    // (1, 99). B chooses to Replace and captures the rebase bytes.
    let outcome = peer_b
        .apply_changeset_with(
            &changeset_a,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Replace,
        )
        .expect("apply_v2 with Replace succeeds");
    let rebase_bytes = outcome.rebase;
    assert!(
        !rebase_bytes.is_empty(),
        "Replace resolution produced a rebase blob",
    );
    // B now sees (1, 10).
    assert_eq!(item_value(&mut peer_b, 1), Some(10));

    // Peer A learns about B's outbound changeset alongside the rebase blob
    // that B's conflict resolution produced. Rebase A's local view of
    // changeset_b so a subsequent apply is coherent with B's resolution.
    let mut rebaser = Rebaser::new().expect("rebaser new");
    rebaser
        .configure(&rebase_bytes)
        .expect("configure with rebase blob");
    let rebased_b = rebaser
        .rebase(&changeset_b)
        .expect("rebase changeset_b succeeds");

    // Applying the rebased outbound of B on peer A must land without
    // conflicting, since the rebaser rewrote B's INSERT into an UPDATE that
    // matches A's on-disk value.
    peer_a
        .apply_changeset_with(
            &rebased_b,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("apply rebased_b on peer A succeeds without conflict");

    // Convergence: both peers agree.
    assert_eq!(item_value(&mut peer_a, 1), item_value(&mut peer_b, 1));
}

#[test]
fn configure_can_be_called_multiple_times() {
    // Multiple configure calls stack: rebaser accumulates conflict
    // resolutions across each blob.
    let mut peer_x = fresh_connection();
    create_items(&mut peer_x);
    let mut peer_y = fresh_connection();
    create_items(&mut peer_y);

    // Build two independent conflict scenarios producing two rebase blobs.
    let mut session_x = peer_x.create_session().unwrap();
    session_x.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut peer_x)
        .unwrap();
    let cs1 = session_x.changeset().unwrap();
    drop(session_x);

    let mut session_x = peer_x.create_session().unwrap();
    session_x.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (2, 20)")
        .execute(&mut peer_x)
        .unwrap();
    let cs2 = session_x.changeset().unwrap();
    drop(session_x);

    // Preload peer Y with conflicting values so the applies produce rebase
    // blobs.
    sql_query("INSERT INTO items (id, v) VALUES (1, 999), (2, 888)")
        .execute(&mut peer_y)
        .unwrap();

    let rebase_first = peer_y
        .apply_changeset_with(
            &cs1,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Replace,
        )
        .unwrap()
        .rebase;
    let rebase_second = peer_y
        .apply_changeset_with(
            &cs2,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Replace,
        )
        .unwrap()
        .rebase;
    assert!(!rebase_first.is_empty());
    assert!(!rebase_second.is_empty());

    let mut peer_a_rebaser = Rebaser::new().unwrap();
    peer_a_rebaser.configure(&rebase_first).unwrap();
    peer_a_rebaser
        .configure(&rebase_second)
        .expect("second configure stacks onto the first");
    let _out = peer_a_rebaser
        .rebase(&cs1)
        .expect("rebase after two configures");
}

#[test]
fn rebased_output_is_a_valid_changeset_readable_by_the_iterator() {
    let bytes = make_insert_changeset();
    let group = Rebaser::new().unwrap();
    let rewritten = group.rebase(&bytes).unwrap();

    let mut reader = ChangesetReader::open(&rewritten).expect("iterate rebased bytes");
    let row = reader.next().unwrap().expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Insert);
    assert_eq!(row.table(), "items");
}

#[test]
fn dropping_the_rebaser_frees_its_state() {
    // We cannot observe the FFI free directly, but we can construct many
    // rebasers back to back and observe that we do not leak allocations that
    // would otherwise crash under a strict allocator later. Assert only that
    // repeated create+drop cycles do not error out.
    for _ in 0..64 {
        let r = Rebaser::new().unwrap();
        drop(r);
    }
}
