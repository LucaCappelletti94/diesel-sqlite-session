//! Integration tests for diesel-sqlite-session.
//!
//! These tests verify end-to-end functionality of the session extension.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{ConflictAction, SqliteSessionExt};

diesel::table! {
    items (id) {
        id -> Integer,
        name -> Text,
        quantity -> Nullable<Integer>,
    }
}

diesel::table! {
    users (id) {
        id -> Integer,
        username -> Nullable<Text>,
    }
}

diesel::table! {
    posts (id) {
        id -> Integer,
        user_id -> Nullable<Integer>,
        content -> Nullable<Text>,
    }
}

diesel::table! {
    tracked (id) {
        id -> Integer,
        val -> Nullable<Text>,
    }
}

diesel::table! {
    untracked (id) {
        id -> Integer,
        val -> Nullable<Text>,
    }
}

/// Helper to create an in-memory connection with a test table.
fn setup_connection() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();
    conn
}

/// Helper to get row count from a table.
fn count_rows(conn: &mut SqliteConnection, table: &str) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>(&format!("SELECT COUNT(*) FROM {table}"))
        .get_result(conn)
        .unwrap()
}

#[test]
fn test_full_replication_workflow() {
    // Source database
    let mut source = setup_connection();

    // Create session and track changes
    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Make changes
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Apple', 10)")
        .execute(&mut source)
        .unwrap();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (2, 'Banana', 20)")
        .execute(&mut source)
        .unwrap();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (3, 'Cherry', 30)")
        .execute(&mut source)
        .unwrap();

    // Generate patchset
    let patchset = session.patchset().unwrap();
    assert!(!patchset.is_empty());

    // Replica database
    let mut replica = setup_connection();
    assert_eq!(count_rows(&mut replica, "items"), 0);

    // Apply patchset
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Verify replication
    assert_eq!(count_rows(&mut replica, "items"), 3);

    // Verify data integrity
    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM items WHERE id = 2")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "Banana");
}

#[test]
fn test_incremental_changes() {
    let mut source = setup_connection();
    let mut replica = setup_connection();

    // First batch of changes
    {
        let mut session = source.create_session().unwrap();
        session.attach::<items::table>().unwrap();

        sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Item1', 100)")
            .execute(&mut source)
            .unwrap();

        let patchset = session.patchset().unwrap();
        replica
            .apply_patchset(&patchset, |_| ConflictAction::Abort)
            .unwrap();
    }

    assert_eq!(count_rows(&mut replica, "items"), 1);

    // Second batch of changes
    {
        let mut session = source.create_session().unwrap();
        session.attach::<items::table>().unwrap();

        sql_query("INSERT INTO items (id, name, quantity) VALUES (2, 'Item2', 200)")
            .execute(&mut source)
            .unwrap();
        sql_query("UPDATE items SET quantity = 150 WHERE id = 1")
            .execute(&mut source)
            .unwrap();

        let patchset = session.patchset().unwrap();
        replica
            .apply_patchset(&patchset, |_| ConflictAction::Abort)
            .unwrap();
    }

    assert_eq!(count_rows(&mut replica, "items"), 2);

    // Verify updated value
    let qty: i32 =
        diesel::dsl::sql::<diesel::sql_types::Integer>("SELECT quantity FROM items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(qty, 150);
}

#[test]
fn test_multiple_tables() {
    let mut source = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT)")
        .execute(&mut source)
        .unwrap();
    sql_query("CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER, content TEXT)")
        .execute(&mut source)
        .unwrap();

    // Track all tables
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();

    sql_query("INSERT INTO users (id, username) VALUES (1, 'alice')")
        .execute(&mut source)
        .unwrap();
    sql_query("INSERT INTO posts (id, user_id, content) VALUES (1, 1, 'Hello World')")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica
    let mut replica = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT)")
        .execute(&mut replica)
        .unwrap();
    sql_query("CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER, content TEXT)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica, "users"), 1);
    assert_eq!(count_rows(&mut replica, "posts"), 1);
}

#[test]
fn test_changeset_vs_patchset() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Test', 50)")
        .execute(&mut source)
        .unwrap();

    let changeset = session.changeset().unwrap();
    let patchset = session.patchset().unwrap();

    // Both should be non-empty
    assert!(!changeset.is_empty());
    assert!(!patchset.is_empty());

    // Changeset typically contains more data (old values)
    // This isn't always true for INSERTs, but generally holds for UPDATEs
    // For now, just verify both work
    let mut replica1 = setup_connection();
    let mut replica2 = setup_connection();

    replica1
        .apply_changeset(&changeset, |_| ConflictAction::Abort)
        .unwrap();
    replica2
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica1, "items"), 1);
    assert_eq!(count_rows(&mut replica2, "items"), 1);
}

#[test]
fn test_conflict_data_existing_row() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Source', 100)")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica already has a row with id=1
    let mut replica = setup_connection();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Replica', 999)")
        .execute(&mut replica)
        .unwrap();

    // Test Replace behavior
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Replace)
        .unwrap();

    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "Source");
}

#[test]
fn test_conflict_omit_preserves_original() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Source', 100)")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica already has different data
    let mut replica = setup_connection();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Original', 500)")
        .execute(&mut replica)
        .unwrap();

    // Omit should preserve the original
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Omit)
        .unwrap();

    let name: String =
        diesel::dsl::sql::<diesel::sql_types::Text>("SELECT name FROM items WHERE id = 1")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(name, "Original");
}

#[test]
fn test_delete_replication() {
    let mut source = setup_connection();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'ToDelete', 1)")
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    sql_query("DELETE FROM items WHERE id = 1")
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica has the row
    let mut replica = setup_connection();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'ToDelete', 1)")
        .execute(&mut replica)
        .unwrap();
    assert_eq!(count_rows(&mut replica, "items"), 1);

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(count_rows(&mut replica, "items"), 0);
}

#[test]
fn test_session_disable_reenable() {
    let mut conn = setup_connection();

    let mut session = conn.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Enabled by default
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Tracked', 10)")
        .execute(&mut conn)
        .unwrap();
    assert!(!session.is_empty());

    // Disable tracking
    session.set_enabled(false);
    sql_query("INSERT INTO items (id, name, quantity) VALUES (2, 'NotTracked', 20)")
        .execute(&mut conn)
        .unwrap();

    // Re-enable
    session.set_enabled(true);
    sql_query("INSERT INTO items (id, name, quantity) VALUES (3, 'AlsoTracked', 30)")
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Apply to replica
    let mut replica = setup_connection();
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Should have 2 rows (1 and 3), not 3
    assert_eq!(count_rows(&mut replica, "items"), 2);

    // Verify row 2 is missing
    let count: i64 =
        diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM items WHERE id = 2")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_large_batch_changes() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Insert many rows
    for i in 0..100 {
        sql_query(format!(
            "INSERT INTO items (id, name, quantity) VALUES ({i}, 'Item{i}', {i})"
        ))
        .execute(&mut source)
        .unwrap();
    }

    let patchset = session.patchset().unwrap();
    assert!(!patchset.is_empty());

    let mut replica = setup_connection();
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica, "items"), 100);
}

#[test]
fn test_empty_session_produces_empty_output() {
    let mut conn = setup_connection();

    let mut session = conn.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // No changes made
    assert!(session.is_empty());

    let changeset = session.changeset().unwrap();
    let patchset = session.patchset().unwrap();

    assert!(changeset.is_empty());
    assert!(patchset.is_empty());
}

#[test]
fn test_attach_nonexistent_table() {
    let mut conn = setup_connection();

    let mut session = conn.create_session().unwrap();

    // Attaching a non-existent table should succeed (SQLite defers the check)
    // The error would occur when trying to track changes
    let result = session.attach_by_name("nonexistent_table");
    assert!(result.is_ok());
}

#[test]
fn test_selective_table_tracking() {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE tracked (id INTEGER PRIMARY KEY, val TEXT)")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE untracked (id INTEGER PRIMARY KEY, val TEXT)")
        .execute(&mut conn)
        .unwrap();

    let mut session = conn.create_session().unwrap();
    session.attach::<tracked::table>().unwrap();

    sql_query("INSERT INTO tracked (id, val) VALUES (1, 'yes')")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO untracked (id, val) VALUES (1, 'no')")
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica with both tables
    let mut replica = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE tracked (id INTEGER PRIMARY KEY, val TEXT)")
        .execute(&mut replica)
        .unwrap();
    sql_query("CREATE TABLE untracked (id INTEGER PRIMARY KEY, val TEXT)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Only tracked table should have data
    let tracked_count: i64 =
        diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM tracked")
            .get_result(&mut replica)
            .unwrap();
    let untracked_count: i64 =
        diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM untracked")
            .get_result(&mut replica)
            .unwrap();

    assert_eq!(tracked_count, 1);
    assert_eq!(untracked_count, 0);
}
