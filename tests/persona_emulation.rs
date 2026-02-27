//! Subagent-style persona tests emulating real crate consumers.

use std::cell::Cell;

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Text};
use diesel_sqlite_session::{ConflictAction, ConflictType, SqliteSessionExt};

fn count_rows(conn: &mut SqliteConnection, table: &str) -> i64 {
    sql::<BigInt>(&format!("SELECT COUNT(*) FROM {table}"))
        .get_result(conn)
        .unwrap()
}

#[test]
fn subagent_runtime_schema_user_can_sync_with_attach_by_name() {
    let mut source = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, payload TEXT)")
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach_by_name("runtime_events").unwrap();

    sql_query("INSERT INTO runtime_events (id, payload) VALUES (1, 'alpha'), (2, 'beta')")
        .execute(&mut source)
        .unwrap();
    let patchset = session.patchset().unwrap();

    let mut replica = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, payload TEXT)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica, "runtime_events"), 2);
    let payload: String = sql::<Text>("SELECT payload FROM runtime_events WHERE id = 2")
        .get_result(&mut replica)
        .unwrap();
    assert_eq!(payload, "beta");
}

#[test]
fn subagent_selective_sync_user_can_filter_tables_by_runtime_name() {
    let mut source = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .execute(&mut source)
        .unwrap();
    sql_query("CREATE TABLE audit_logs (id INTEGER PRIMARY KEY, message TEXT)")
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach_by_name("orders").unwrap();

    sql_query("INSERT INTO orders (id, amount) VALUES (1, 100)")
        .execute(&mut source)
        .unwrap();
    sql_query("INSERT INTO audit_logs (id, message) VALUES (1, 'internal-only')")
        .execute(&mut source)
        .unwrap();
    let patchset = session.patchset().unwrap();

    let mut replica = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .execute(&mut replica)
        .unwrap();
    sql_query("CREATE TABLE audit_logs (id INTEGER PRIMARY KEY, message TEXT)")
        .execute(&mut replica)
        .unwrap();

    replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();

    assert_eq!(count_rows(&mut replica, "orders"), 1);
    assert_eq!(count_rows(&mut replica, "audit_logs"), 0);
}

#[test]
fn subagent_conflict_policy_user_can_apply_custom_resolution() {
    let mut source = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach_by_name("docs").unwrap();
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'source')")
        .execute(&mut source)
        .unwrap();
    let patchset = session.patchset().unwrap();

    let mut replica = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
        .execute(&mut replica)
        .unwrap();
    sql_query("INSERT INTO docs (id, body) VALUES (1, 'replica')")
        .execute(&mut replica)
        .unwrap();

    let conflicts_seen = Cell::new(0_u32);
    replica
        .apply_patchset(&patchset, |kind| match kind {
            ConflictType::Data | ConflictType::Conflict | ConflictType::Constraint => {
                conflicts_seen.set(conflicts_seen.get() + 1);
                ConflictAction::Replace
            }
            _ => ConflictAction::Abort,
        })
        .unwrap();

    assert!(conflicts_seen.get() > 0);
    let body: String = sql::<Text>("SELECT body FROM docs WHERE id = 1")
        .get_result(&mut replica)
        .unwrap();
    assert_eq!(body, "source");
}
