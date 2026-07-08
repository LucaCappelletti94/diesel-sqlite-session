# diesel-sqlite-session

[![CI](https://github.com/LucaCappelletti94/diesel-sqlite-session/actions/workflows/ci.yml/badge.svg)](https://github.com/LucaCappelletti94/diesel-sqlite-session/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/LucaCappelletti94/diesel-sqlite-session/graph/badge.svg)](https://codecov.io/gh/LucaCappelletti94/diesel-sqlite-session)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

This crate adds `SQLite` [session extension](https://sqlite.org/sessionintro.html) support to the Diesel ORM. It tracks INSERT, UPDATE, and DELETE operations through a `SqliteSessionExt` trait on `SqliteConnection`, exposes the recorded changes as compact binary changesets or patchsets, and applies them to replica databases with configurable conflict resolution. Tables can be attached with type-safe Diesel table types or by runtime name, and the crate works on Linux, macOS, Windows, iOS, Android, and WebAssembly. Typical applications include offline-first apps that sync when connectivity returns, multi-master replication between database instances, audit logging for compliance, undo and redo systems backed by stored changesets, and edge databases that sync with a central server.

> **Note**: Support depends on Diesel's `with_raw_connection` (added in [diesel#4966](https://github.com/diesel-rs/diesel/pull/4966)). Since this is merged but not yet released, use Diesel from the upstream git repo.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
diesel-sqlite-session = { git = "https://github.com/LucaCappelletti94/diesel-sqlite-session" }
diesel = { git = "https://github.com/diesel-rs/diesel", features = ["sqlite"] }
```

## Quick Start

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{SqliteSessionExt, ConflictAction};

diesel::table! {
    users (id) {
        id -> Integer,
        name -> Text,
    }
}

#[derive(Insertable)]
#[diesel(table_name = users)]
struct NewUser<'a> {
    id: i32,
    name: &'a str,
}

// Create source connection and track changes.
let mut source = SqliteConnection::establish(":memory:").unwrap();
// Schema setup still requires SQL. Diesel ORM handles data operations.
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
    .execute(&mut source)
    .unwrap();

// Create a session and attach the table
let mut session = source.create_session().unwrap();
session.attach::<users::table>().unwrap();

// Make changes
diesel::insert_into(users::table)
    .values(NewUser { id: 1, name: "Alice" })
    .execute(&mut source)
    .unwrap();

// Generate patchset
let patchset = session.patchset().unwrap();

// Apply to replica
let mut replica = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
    .execute(&mut replica)
    .unwrap();

replica.apply_patchset(&patchset, |_| ConflictAction::Abort).unwrap();
```

## API Overview

### Extension Trait

The `SqliteSessionExt` trait extends `SqliteConnection` with session capabilities:

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{SqliteSessionExt, ConflictAction};

diesel::table! {
    t (id) {
        id -> Integer,
    }
}

#[derive(Insertable)]
#[diesel(table_name = t)]
struct NewRow {
    id: i32,
}

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
    .execute(&mut conn)
    .unwrap();

let mut session = conn.create_session().unwrap();
session.attach_by_name("t").unwrap();
diesel::insert_into(t::table)
    .values(NewRow { id: 1 })
    .execute(&mut conn)
    .unwrap();
let patchset = session.patchset().unwrap();

// Apply to another connection
let mut conn2 = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
    .execute(&mut conn2)
    .unwrap();
conn2.apply_patchset(&patchset, |_| ConflictAction::Abort).unwrap();
```

### Session Methods

```rust
use diesel::prelude::*;
use diesel_sqlite_session::SqliteSessionExt;

diesel::table! {
    users (id) {
        id -> Integer,
        name -> Nullable<Text>,
    }
}

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    .execute(&mut conn)
    .unwrap();

let mut session = conn.create_session().unwrap();

// Type-safe table attachment (recommended)
session.attach::<users::table>().unwrap();

// Or attach all tables
// session.attach_all().unwrap();

// Or dynamic table name (for runtime schemas)
// session.attach_by_name("dynamic_table").unwrap();

// Make some changes
diesel::insert_into(users::table)
    .values((users::id.eq(1), users::name.eq(Some("Alice"))))
    .execute(&mut conn)
    .unwrap();

// Generate output
let patchset = session.patchset().unwrap();   // Smaller, new values only
let changeset = session.changeset().unwrap(); // Larger, includes old values

// Check state
let has_changes = !session.is_empty();

// Temporarily disable tracking
session.set_enabled(false);
```

### Conflict Handling

When applying changesets/patchsets, conflicts are handled via callback:

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{SqliteSessionExt, ConflictAction, ConflictType};

diesel::table! {
    t (id) {
        id -> Integer,
        v -> Integer,
    }
}

// Create source and generate patchset
let mut source = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)")
    .execute(&mut source)
    .unwrap();
let mut session = source.create_session().unwrap();
session.attach::<t::table>().unwrap();
diesel::insert_into(t::table)
    .values((t::id.eq(1), t::v.eq(100)))
    .execute(&mut source)
    .unwrap();
let patchset = session.patchset().unwrap();

// Apply with conflict handling
let mut replica = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)")
    .execute(&mut replica)
    .unwrap();
diesel::insert_into(t::table)
    .values((t::id.eq(1), t::v.eq(999)))
    .execute(&mut replica)
    .unwrap();

replica.apply_patchset(&patchset, |conflict_type| {
    match conflict_type {
        ConflictType::Data => ConflictAction::Replace,    // Overwrite
        ConflictType::NotFound => ConflictAction::Omit,   // Skip
        ConflictType::Conflict => ConflictAction::Replace,
        _ => ConflictAction::Abort,
    }
}).unwrap();
```

### Pre-update Hook

`sqlite3_preupdate_hook` fires just before every row-level `INSERT`, `UPDATE`, or `DELETE` on a rowid table. It requires `SQLITE_ENABLE_PREUPDATE_HOOK`, the same flag the session extension needs, which is why mainline Diesel cannot expose it and this crate does.

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{PreUpdateOp, SqliteSessionExt};

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE audit (id INTEGER PRIMARY KEY, note TEXT)")
    .execute(&mut conn)
    .unwrap();

let hook = conn.on_preupdate(|event| match event.op() {
    PreUpdateOp::Insert => {
        let note = event.new_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
        println!("inserted rowid {} note {:?}", event.new_rowid(), note);
    }
    PreUpdateOp::Update => {
        let before = event.old_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
        let after = event.new_value(1).ok().and_then(|v| v.as_text().map(str::to_owned));
        println!("update rowid {} {:?} -> {:?}", event.old_rowid(), before, after);
    }
    PreUpdateOp::Delete => {
        println!("delete rowid {}", event.old_rowid());
    }
});

diesel::sql_query("INSERT INTO audit (note) VALUES ('hello')")
    .execute(&mut conn)
    .unwrap();

// Drop the guard to detach the callback.
drop(hook);
```

The callback receives a `PreUpdateEvent<'_>` bound to the callback frame. Values returned by `old_value(col)` / `new_value(col)` borrow from `SQLite`'s per-value buffers, so copy anything you need into owned types (`String`, `Vec<u8>`, `i64`) before the closure returns. `depth()` is `0` at the top level and `>0` inside a trigger. Panics inside the closure are caught by the trampoline.

`PreUpdateHook` is an RAII guard. `SQLite` allows one hook per connection, so a second `on_preupdate` while a guard is alive replaces the callback and silently retires the older guard.

### Incremental Blob I/O

`SqliteBlob` wraps the `sqlite3_blob_*` family. Diesel already ships a read-only handle; this crate adds the read plus write pair so writes can raise the pre-update hook (`blob_write_column` reports the column index the handle was opened on).

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{BlobMode, SqliteSessionExt};

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE photos (id INTEGER PRIMARY KEY, data BLOB)")
    .execute(&mut conn)
    .unwrap();
diesel::sql_query("INSERT INTO photos (id, data) VALUES (1, zeroblob(16))")
    .execute(&mut conn)
    .unwrap();

let blob = conn
    .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
    .unwrap();
assert_eq!(blob.len(), 16);
blob.write_at(4, b"HelloBlob").unwrap();
let mut echo = [0u8; 9];
blob.read_at(4, &mut echo).unwrap();
assert_eq!(&echo, b"HelloBlob");
blob.close().unwrap();
```

`SqliteBlob` is `!Send + !Sync` and RAII with the same "drop before the connection" contract as `Session` and `PreUpdateHook`. `write_at` on a `ReadOnly` handle short-circuits to `BlobError::ReadOnly` without touching `SQLite`. `close(self)` surfaces the result of `sqlite3_blob_close`. `Drop` closes silently.

## Platform Support

| Platform | Backend | Status |
|----------|---------|--------|
| Linux/macOS/Windows | `libsqlite3-sys` (bundled) | Supported, tested in CI |
| Linux ARM64 (native runner) | `libsqlite3-sys` (bundled) | Supported, runtime-tested in CI |
| Linux edge targets (`musl`, `armv7`) | `libsqlite3-sys` (bundled) | Supported, cross-link/build checked in CI |
| Windows ARM64 | `libsqlite3-sys` (bundled) | Supported, link/build checked in CI |
| iOS (simulator + device build) | `libsqlite3-sys` (bundled) | Supported, simulator runtime-tested in CI |
| Android (emulator + target builds) | `libsqlite3-sys` (bundled) | Supported, emulator runtime-tested in CI |
| WebAssembly | `sqlite-wasm-rs` | Supported, tested in CI |

## Benchmarks

Native and WebAssembly performance numbers, a comparison against rusqlite, and instructions for reproducing the measurements are kept in [benches/BENCHMARKS.md](benches/BENCHMARKS.md).

## Related Projects

- **[sqlite-diff-rs](https://github.com/LucaCappelletti94/sqlite-diff-rs)** - Build `SQLite` changesets/patchsets programmatically without requiring `SQLite`. Useful for constructing changesets from other sources (`PostgreSQL` CDC, Debezium, Maxwell) and applying them with diesel-sqlite-session.

## License

MIT
