//! The `items` fixture the changeset tests share, and the scenarios the
//! browser tests in `wasm-tests` run too.

#![allow(dead_code)]

pub mod scenarios;

use diesel::prelude::*;
use diesel_sqlite_session::SqliteSessionExt;

diesel::table! {
    items (id) {
        id -> BigInt,
        v -> BigInt,
    }
}

pub fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

pub fn create_items(conn: &mut SqliteConnection) {
    // Diesel has no DDL builder, so the table is created from text.
    diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

pub fn count_items(conn: &mut SqliteConnection) -> i64 {
    items::table.count().get_result(conn).unwrap()
}

/// An INSERT-only changeset for `rows` recorded in a fresh `items` table.
pub fn make_changeset(rows: &[(i64, i64)]) -> Vec<u8> {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    for &(id, v) in rows {
        diesel::insert_into(items::table)
            .values((items::id.eq(id), items::v.eq(v)))
            .execute(&mut conn)
            .unwrap();
    }
    session.changeset().unwrap()
}
