//! WASM tests for diesel-sqlite-session.
//!
//! These tests run in a headless browser environment using wasm-bindgen-test.

#![cfg(target_arch = "wasm32")]

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{ConflictAction, SqliteSessionExt};
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
