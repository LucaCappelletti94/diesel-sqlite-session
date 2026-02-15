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

#[derive(Insertable)]
#[diesel(table_name = items)]
struct NewItem<'a> {
    id: i32,
    name: &'a str,
    quantity: Option<i32>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = items)]
#[allow(dead_code)]
struct Item {
    id: i32,
    name: String,
    quantity: Option<i32>,
}

#[derive(Insertable)]
#[diesel(table_name = users)]
struct NewUser<'a> {
    id: i32,
    username: Option<&'a str>,
}

#[derive(Insertable)]
#[diesel(table_name = posts)]
struct NewPost<'a> {
    id: i32,
    user_id: Option<i32>,
    content: Option<&'a str>,
}

#[derive(Insertable)]
#[diesel(table_name = tracked)]
struct NewTracked<'a> {
    id: i32,
    val: Option<&'a str>,
}

#[derive(Insertable)]
#[diesel(table_name = untracked)]
struct NewUntracked<'a> {
    id: i32,
    val: Option<&'a str>,
}

/// Helper to create an in-memory connection with a test table.
fn setup_connection() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();
    conn
}

#[test]
fn test_full_replication_workflow() {
    // Source database
    let mut source = setup_connection();

    // Create session and track changes
    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Make changes using Diesel ORM
    diesel::insert_into(items::table)
        .values(&[
            NewItem {
                id: 1,
                name: "Apple",
                quantity: Some(10),
            },
            NewItem {
                id: 2,
                name: "Banana",
                quantity: Some(20),
            },
            NewItem {
                id: 3,
                name: "Cherry",
                quantity: Some(30),
            },
        ])
        .execute(&mut source)
        .unwrap();

    // Generate patchset
    let patchset = session.patchset().unwrap();
    assert!(!patchset.is_empty());

    // Replica database
    let mut replica = setup_connection();
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        0
    );

    // Apply patchset
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Verify replication
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        3
    );

    // Verify data integrity
    let item: Item = items::table
        .filter(items::id.eq(2))
        .select(Item::as_select())
        .first(&mut replica)
        .unwrap();
    assert_eq!(item.name, "Banana");
}

#[test]
fn test_incremental_changes() {
    let mut source = setup_connection();
    let mut replica = setup_connection();

    // First batch of changes
    {
        let mut session = source.create_session().unwrap();
        session.attach::<items::table>().unwrap();

        diesel::insert_into(items::table)
            .values(NewItem {
                id: 1,
                name: "Item1",
                quantity: Some(100),
            })
            .execute(&mut source)
            .unwrap();

        let patchset = session.patchset().unwrap();
        replica
            .apply_patchset(&patchset, |_| ConflictAction::Abort)
            .unwrap();
    }

    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        1
    );

    // Second batch of changes
    {
        let mut session = source.create_session().unwrap();
        session.attach::<items::table>().unwrap();

        diesel::insert_into(items::table)
            .values(NewItem {
                id: 2,
                name: "Item2",
                quantity: Some(200),
            })
            .execute(&mut source)
            .unwrap();

        diesel::update(items::table.filter(items::id.eq(1)))
            .set(items::quantity.eq(150))
            .execute(&mut source)
            .unwrap();

        let patchset = session.patchset().unwrap();
        replica
            .apply_patchset(&patchset, |_| ConflictAction::Abort)
            .unwrap();
    }

    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        2
    );

    // Verify updated value
    let item: Item = items::table
        .filter(items::id.eq(1))
        .select(Item::as_select())
        .first(&mut replica)
        .unwrap();
    assert_eq!(item.quantity, Some(150));
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

    diesel::insert_into(users::table)
        .values(NewUser {
            id: 1,
            username: Some("alice"),
        })
        .execute(&mut source)
        .unwrap();

    diesel::insert_into(posts::table)
        .values(NewPost {
            id: 1,
            user_id: Some(1),
            content: Some("Hello World"),
        })
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

    assert_eq!(
        users::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        1
    );
    assert_eq!(
        posts::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        1
    );
}

#[test]
fn test_changeset_vs_patchset() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Test",
            quantity: Some(50),
        })
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

    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica1)
            .unwrap(),
        1
    );
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica2)
            .unwrap(),
        1
    );
}

#[test]
fn test_conflict_data_existing_row() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Source",
            quantity: Some(100),
        })
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica already has a row with id=1
    let mut replica = setup_connection();
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Replica",
            quantity: Some(999),
        })
        .execute(&mut replica)
        .unwrap();

    // Test Replace behavior
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Replace)
        .unwrap();

    let item: Item = items::table
        .filter(items::id.eq(1))
        .select(Item::as_select())
        .first(&mut replica)
        .unwrap();
    assert_eq!(item.name, "Source");
}

#[test]
fn test_conflict_omit_preserves_original() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Source",
            quantity: Some(100),
        })
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica already has different data
    let mut replica = setup_connection();
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Original",
            quantity: Some(500),
        })
        .execute(&mut replica)
        .unwrap();

    // Omit should preserve the original
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Omit)
        .unwrap();

    let item: Item = items::table
        .filter(items::id.eq(1))
        .select(Item::as_select())
        .first(&mut replica)
        .unwrap();
    assert_eq!(item.name, "Original");
}

#[test]
fn test_delete_replication() {
    let mut source = setup_connection();

    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "ToDelete",
            quantity: Some(1),
        })
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    diesel::delete(items::table.filter(items::id.eq(1)))
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Replica has the row
    let mut replica = setup_connection();
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "ToDelete",
            quantity: Some(1),
        })
        .execute(&mut replica)
        .unwrap();
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        1
    );

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        0
    );
}

#[test]
fn test_session_disable_reenable() {
    let mut conn = setup_connection();

    let mut session = conn.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Enabled by default
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 1,
            name: "Tracked",
            quantity: Some(10),
        })
        .execute(&mut conn)
        .unwrap();
    assert!(!session.is_empty());

    // Disable tracking
    session.set_enabled(false);
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 2,
            name: "NotTracked",
            quantity: Some(20),
        })
        .execute(&mut conn)
        .unwrap();

    // Re-enable
    session.set_enabled(true);
    diesel::insert_into(items::table)
        .values(NewItem {
            id: 3,
            name: "AlsoTracked",
            quantity: Some(30),
        })
        .execute(&mut conn)
        .unwrap();

    let patchset = session.patchset().unwrap();

    // Apply to replica
    let mut replica = setup_connection();
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    // Should have 2 rows (1 and 3), not 3
    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        2
    );

    // Verify row 2 is missing
    let count = items::table
        .filter(items::id.eq(2))
        .count()
        .get_result::<i64>(&mut replica)
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_large_batch_changes() {
    let mut source = setup_connection();

    let mut session = source.create_session().unwrap();
    session.attach::<items::table>().unwrap();

    // Insert many rows using Diesel batch insert
    let new_items: Vec<NewItem> = (0..100)
        .map(|i| NewItem {
            id: i,
            name: "Item",
            quantity: Some(i),
        })
        .collect();

    diesel::insert_into(items::table)
        .values(&new_items)
        .execute(&mut source)
        .unwrap();

    let patchset = session.patchset().unwrap();
    assert!(!patchset.is_empty());

    let mut replica = setup_connection();
    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(
        items::table
            .count()
            .get_result::<i64>(&mut replica)
            .unwrap(),
        100
    );
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

    diesel::insert_into(tracked::table)
        .values(NewTracked {
            id: 1,
            val: Some("yes"),
        })
        .execute(&mut conn)
        .unwrap();

    diesel::insert_into(untracked::table)
        .values(NewUntracked {
            id: 1,
            val: Some("no"),
        })
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
    let tracked_count = tracked::table
        .count()
        .get_result::<i64>(&mut replica)
        .unwrap();
    let untracked_count = untracked::table
        .count()
        .get_result::<i64>(&mut replica)
        .unwrap();

    assert_eq!(tracked_count, 1);
    assert_eq!(untracked_count, 0);
}
