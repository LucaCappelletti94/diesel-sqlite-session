//! WASM tests for diesel-sqlite-session.
//!
//! These tests run in a headless browser environment using wasm-bindgen-test.

#![cfg(target_arch = "wasm32")]

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    BlobError, BlobMode, ConflictAction, PreUpdateColumnType, PreUpdateOp, SqliteSessionExt,
};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

diesel::table! {
    test_items (id) {
        id -> Integer,
        name -> Nullable<Text>,
        value -> Nullable<Integer>,
    }
}

/// Helper to create an in-memory connection.
fn create_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("Failed to create in-memory connection")
}

/// Helper to setup a test table.
fn setup_table(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE test_items (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
        .execute(conn)
        .expect("Failed to create table");
}

/// Helper to get row count.
fn count_rows(conn: &mut SqliteConnection) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM test_items")
        .get_result(conn)
        .expect("Failed to count rows")
}

#[wasm_bindgen_test]
async fn test_session_creation_wasm() {
    let mut conn = create_connection();
    let session = conn.create_session();
    assert!(session.is_ok(), "Session creation should succeed");
}

#[wasm_bindgen_test]
async fn test_attach_table_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    let result = session.attach::<test_items::table>();
    assert!(result.is_ok(), "Attach should succeed");
}

#[wasm_bindgen_test]
async fn test_attach_table_by_name_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    let result = session.attach_by_name("test_items");
    assert!(result.is_ok(), "Attach by name should succeed");
}

#[wasm_bindgen_test]
async fn test_attach_all_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    let result = session.attach_all();
    assert!(result.is_ok(), "Attach all should succeed");
}

#[wasm_bindgen_test]
async fn test_changeset_generation_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'test', 42)")
        .execute(&mut conn)
        .unwrap();

    assert!(!session.is_empty(), "Session should have changes");

    let changeset = session.changeset().unwrap();
    assert!(!changeset.is_empty(), "Changeset should not be empty");
}

#[wasm_bindgen_test]
async fn test_patchset_generation_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'test', 42)")
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();
    assert!(!patchset.is_empty(), "Patchset should not be empty");
}

#[wasm_bindgen_test]
async fn test_apply_patchset_wasm() {
    // Source connection
    let mut source = create_connection();
    setup_table(&mut source);

    let mut session = source.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Item1', 100)")
        .execute(&mut source)
        .unwrap();
    sql_query("INSERT INTO test_items (id, name, value) VALUES (2, 'Item2', 200)")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica connection
    let mut replica = create_connection();
    setup_table(&mut replica);

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica), 2, "Replica should have 2 rows");
}

#[wasm_bindgen_test]
async fn test_apply_changeset_wasm() {
    let mut source = create_connection();
    setup_table(&mut source);

    let mut session = source.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Test', 50)")
        .execute(&mut source)
        .unwrap();

    let changeset = session.changeset().unwrap();

    let mut replica = create_connection();
    setup_table(&mut replica);

    replica
        .apply_changeset(&changeset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica), 1, "Replica should have 1 row");
}

#[wasm_bindgen_test]
async fn test_conflict_replace_wasm() {
    let mut source = create_connection();
    setup_table(&mut source);

    let mut session = source.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Source', 100)")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica with conflicting row
    let mut replica = create_connection();
    setup_table(&mut replica);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Replica', 999)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Replace)
        .unwrap();

    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM test_items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "Source", "Replace should overwrite");
}

#[wasm_bindgen_test]
async fn test_conflict_omit_wasm() {
    let mut source = create_connection();
    setup_table(&mut source);

    let mut session = source.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Source', 100)")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica with conflicting row
    let mut replica = create_connection();
    setup_table(&mut replica);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'Original', 500)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Omit)
        .unwrap();

    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM test_items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "Original", "Omit should preserve original");
}

#[wasm_bindgen_test]
async fn test_update_tracking_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'original', 10)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("UPDATE test_items SET name = 'updated' WHERE id = 1")
        .execute(&mut conn)
        .unwrap();

    assert!(!session.is_empty(), "Session should track update");

    let patchset = session.patchset().unwrap();

    let mut replica = create_connection();
    setup_table(&mut replica);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'original', 10)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM test_items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "updated");
}

#[wasm_bindgen_test]
async fn test_delete_tracking_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'to_delete', 1)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    sql_query("DELETE FROM test_items WHERE id = 1")
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();

    let mut replica = create_connection();
    setup_table(&mut replica);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'to_delete', 1)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica), 0, "Delete should be replicated");
}

#[wasm_bindgen_test]
async fn test_enable_disable_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();

    // Insert while enabled
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'tracked', 10)")
        .execute(&mut conn)
        .unwrap();

    // Disable and insert
    session.set_enabled(false);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (2, 'untracked', 20)")
        .execute(&mut conn)
        .unwrap();

    // Re-enable and insert
    session.set_enabled(true);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (3, 'tracked_again', 30)")
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();

    let mut replica = create_connection();
    setup_table(&mut replica);
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Should have 2 rows (1 and 3, not 2)
    assert_eq!(count_rows(&mut replica), 2);
}

#[wasm_bindgen_test]
async fn preupdate_insert_fires_hook_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let events: Arc<Mutex<Vec<(PreUpdateOp, String, String, i64, usize)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let hook = conn.on_preupdate(move |event| {
        sink.lock().push((
            event.op(),
            event.database().to_owned(),
            event.table().to_owned(),
            event.new_rowid(),
            event.column_count(),
        ));
    });
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'w', 42)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let observed = events.lock().clone();
    assert_eq!(observed.len(), 1);
    let (op, database, table, new_rowid, column_count) = observed[0].clone();
    assert_eq!(op, PreUpdateOp::Insert);
    assert_eq!(database, "main");
    assert_eq!(table, "test_items");
    assert_eq!(new_rowid, 1);
    assert_eq!(column_count, 3);
}

#[wasm_bindgen_test]
async fn preupdate_update_delivers_old_and_new_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'before', 1)")
        .execute(&mut conn)
        .unwrap();

    #[derive(Clone)]
    struct Snapshot {
        old_name: Option<String>,
        new_name: Option<String>,
        old_value: i64,
        new_value: i64,
    }
    let snap: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    let sink = snap.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Update) {
            let old_name = event
                .old_value(1)
                .ok()
                .and_then(|v| v.as_text().map(str::to_owned));
            let new_name = event
                .new_value(1)
                .ok()
                .and_then(|v| v.as_text().map(str::to_owned));
            let old_value = event.old_value(2).map(|v| v.as_i64()).unwrap_or(-1);
            let new_value = event.new_value(2).map(|v| v.as_i64()).unwrap_or(-1);
            *sink.lock() = Some(Snapshot {
                old_name,
                new_name,
                old_value,
                new_value,
            });
        }
    });
    sql_query("UPDATE test_items SET name = 'after', value = 2 WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let s = snap.lock().clone().expect("saw an Update event");
    assert_eq!(s.old_name.as_deref(), Some("before"));
    assert_eq!(s.new_name.as_deref(), Some("after"));
    assert_eq!(s.old_value, 1);
    assert_eq!(s.new_value, 2);
}

#[wasm_bindgen_test]
async fn preupdate_column_type_matches_wasm() {
    let mut conn = create_connection();
    sql_query("CREATE TABLE mixed_wasm (a, b, c, d, e)")
        .execute(&mut conn)
        .unwrap();

    let types: Arc<Mutex<Vec<PreUpdateColumnType>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = types.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            let mut buf = sink.lock();
            for i in 0..u32::try_from(event.column_count()).unwrap() {
                buf.push(event.new_value(i).unwrap().column_type());
            }
        }
    });
    sql_query("INSERT INTO mixed_wasm (a, b, c, d, e) VALUES (7, 'hi', 3.5, x'DEADBEEF', NULL)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let observed = types.lock().clone();
    assert_eq!(
        observed,
        vec![
            PreUpdateColumnType::Integer,
            PreUpdateColumnType::Text,
            PreUpdateColumnType::Float,
            PreUpdateColumnType::Blob,
            PreUpdateColumnType::Null,
        ],
    );
}

#[wasm_bindgen_test]
async fn preupdate_dropping_guard_stops_callback_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    let hook = conn.on_preupdate(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'a', 1)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(hook);
    sql_query("INSERT INTO test_items (id, name, value) VALUES (2, 'b', 2)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[wasm_bindgen_test]
async fn preupdate_then_session_cutover_wasm() {
    let mut conn = create_connection();
    setup_table(&mut conn);

    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    let hook = conn.on_preupdate(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    sql_query("INSERT INTO test_items (id, name, value) VALUES (1, 'x', 1)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(hook);

    let mut session = conn.create_session().unwrap();
    session.attach::<test_items::table>().unwrap();
    sql_query("INSERT INTO test_items (id, name, value) VALUES (2, 'y', 2)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    drop(session);
    assert!(!changeset.is_empty());

    let mut replica = create_connection();
    setup_table(&mut replica);
    replica
        .apply_changeset(&changeset, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_rows(&mut replica), 1);
}

#[wasm_bindgen_test]
async fn blob_read_write_round_trip_wasm() {
    let mut conn = create_connection();
    sql_query("CREATE TABLE photos_wasm (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO photos_wasm (id, data) VALUES (1, zeroblob(8))")
        .execute(&mut conn)
        .unwrap();

    let blob = conn
        .open_blob("main", "photos_wasm", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    assert_eq!(blob.len(), 8);
    blob.write_at(2, b"WASM").expect("write succeeds");
    let mut echo = [0u8; 4];
    blob.read_at(2, &mut echo).expect("read succeeds");
    assert_eq!(&echo, b"WASM");
    blob.close().expect("close succeeds");
}

#[wasm_bindgen_test]
async fn blob_write_read_only_returns_read_only_wasm() {
    let mut conn = create_connection();
    sql_query("CREATE TABLE ro_wasm (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO ro_wasm (id, data) VALUES (1, zeroblob(4))")
        .execute(&mut conn)
        .unwrap();

    let blob = conn
        .open_blob("main", "ro_wasm", "data", 1, BlobMode::ReadOnly)
        .expect("open handle");
    let err = blob.write_at(0, b"x").unwrap_err();
    assert!(matches!(err, BlobError::ReadOnly));
}

#[wasm_bindgen_test]
async fn blob_write_fires_preupdate_with_column_index_wasm() {
    let mut conn = create_connection();
    sql_query("CREATE TABLE bw_wasm (id INTEGER PRIMARY KEY, name TEXT, data BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO bw_wasm (id, name, data) VALUES (1, 'row', zeroblob(4))")
        .execute(&mut conn)
        .unwrap();

    let seen: Arc<Mutex<Vec<(PreUpdateOp, Option<u32>)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let hook = conn.on_preupdate(move |event| {
        sink.lock().push((event.op(), event.blob_write_column()));
    });

    let blob = conn
        .open_blob("main", "bw_wasm", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    blob.write_at(0, b"abcd").expect("write succeeds");
    blob.close().expect("close reports success");
    drop(hook);

    let observed = seen.lock().clone();
    let blob_hit = observed
        .iter()
        .find(|(_, col)| col.is_some())
        .expect("saw a blob-write event");
    assert_eq!(blob_hit.0, PreUpdateOp::Delete);
    assert_eq!(blob_hit.1, Some(2));
}
