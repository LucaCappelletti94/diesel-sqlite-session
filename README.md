# diesel-sqlite-session

[![CI](https://github.com/LucaCappelletti94/diesel-sqlite-session/actions/workflows/ci.yml/badge.svg)](https://github.com/LucaCappelletti94/diesel-sqlite-session/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/LucaCappelletti94/diesel-sqlite-session/graph/badge.svg)](https://codecov.io/gh/LucaCappelletti94/diesel-sqlite-session)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

`SQLite` [session extension](https://sqlite.org/sessionintro.html) support for Diesel. Track row-level `INSERT`, `UPDATE`, and `DELETE` on a `SqliteConnection`, emit changesets or patchsets, and apply them elsewhere with a conflict callback. Attach tables by Diesel table type or by runtime name. Runs on Linux, macOS, Windows, iOS, Android, and WebAssembly.

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

The callback receives a `PreUpdateEvent<'_>` bound to the callback frame. Values returned by `old_value(col)` / `new_value(col)` borrow from `SQLite`'s per-value buffers, so copy anything you need into owned types (`String`, `Vec<u8>`, `i64`) before the closure returns. `blob_write_column()` returns `Some(i)` when the event was raised by `sqlite3_blob_write`, `None` for regular DML. `depth()` is `0` at the top level and `>0` inside a trigger. Panics inside the closure are caught by the trampoline.

`PreUpdateHook` is an RAII guard; `SQLite` allows one hook per connection, so a second `on_preupdate` while a guard is alive replaces the callback and silently retires the older guard.

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

`SqliteBlob` is `!Send + !Sync` and RAII with the same "drop before the connection" contract as `Session` and `PreUpdateHook`. `write_at` on a `ReadOnly` handle short-circuits to `BlobError::ReadOnly` without touching `SQLite`. `close(self)` surfaces the result of `sqlite3_blob_close`; `Drop` closes silently.

### Changeset Iterator

`ChangesetReader` wraps the `sqlite3changeset_start` / `_next` / `_op` / `_pk` / `_old` / `_new` / `_finalize` family. It is the read side of the blobs `Session::changeset` and `Session::patchset` produce: walk each row and inspect old and new values without applying anything. `open_inverted` walks the inverse (`INSERT` becomes `DELETE`, and vice versa).

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{ChangesetOp, ChangesetReader, SqliteSessionExt};

# let mut conn = SqliteConnection::establish(":memory:").unwrap();
# diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
#     .execute(&mut conn).unwrap();
# let mut session = conn.create_session().unwrap();
# session.attach_all().unwrap();
# diesel::sql_query("INSERT INTO items (id, name) VALUES (1, 'Widget')")
#     .execute(&mut conn).unwrap();
# let changeset = session.changeset().unwrap();
let mut reader = ChangesetReader::open(&changeset).unwrap();
while let Some(row) = reader.next().unwrap() {
    match row.op() {
        ChangesetOp::Insert => {
            let name = row.new_value(1).unwrap().and_then(|v| v.as_text().map(str::to_owned));
            println!("insert into {} name={:?}", row.table(), name);
        }
        ChangesetOp::Update => println!("update on {}", row.table()),
        ChangesetOp::Delete => println!("delete from {}", row.table()),
    }
}
```

`old_value(i)` and `new_value(i)` return `Result<Option<ChangesetValue<'_>>, ChangesetError>`. `Ok(None)` means the column was not touched by an `UPDATE`; `Err(OldNotAvailableOnInsert)` and `Err(NewNotAvailableOnDelete)` cover the op-shape mismatches. `is_primary_key(i)` reports the PK mask for the current row.

### Enhanced Apply

`apply_changeset_with` wraps `sqlite3changeset_apply_v2`, adding three things over `apply_changeset`: an `ApplyFlags` bitmask, a per-table filter callback, and the rebase blob `SQLite` produces when the conflict callback resolves conflicts with `Replace` or `Omit`.

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{ApplyFlags, ConflictAction, ConflictType, SqliteSessionExt};

# let mut conn = SqliteConnection::establish(":memory:").unwrap();
# diesel::sql_query("CREATE TABLE keep (id INTEGER PRIMARY KEY, v INTEGER)")
#     .execute(&mut conn).unwrap();
# diesel::sql_query("CREATE TABLE audit (id INTEGER PRIMARY KEY, v INTEGER)")
#     .execute(&mut conn).unwrap();
# let changeset: Vec<u8> = vec![];
let outcome = conn.apply_changeset_with(
    &changeset,
    ApplyFlags::INVERT | ApplyFlags::IGNORENOOP,
    |table| table != "audit",
    |info| match info.conflict_type() {
        ConflictType::Data => ConflictAction::Replace,
        _ => ConflictAction::Abort,
    },
)?;
// `outcome.rebase` carries the SQLite-emitted rebase blob when the conflict
// callback resolved anything via Replace or Omit. Empty otherwise.
# Ok::<_, diesel_sqlite_session::ApplyError>(())
```

The conflict callback receives a `ConflictInfo`: `old_value(i)` (pre-image), `new_value(i)` (post-image), `conflict_value(i)` (on-disk clashing value), plus `fk_conflicts_count()` for `ForeignKey` conflicts. All accessors are bound to the callback frame.

Flags: `NOSAVEPOINT` (skip the wrapping `SAVEPOINT`), `INVERT` (apply the inverse), `IGNORENOOP` (suppress the conflict callback for `UPDATE` rows whose replica value already matches the post-image), `FKNOACTION` (skip `NO ACTION` FK handling on cascades). Compose with `|`.

`apply_changeset_v3_with` (and its streamed sibling) hands the filter the whole `ChangesetRow` instead of just the table name, so it can inspect op, PK layout, and column values.

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{ApplyFlags, ChangesetOp, ChangesetRow, ConflictAction, SqliteSessionExt};

# let mut conn = SqliteConnection::establish(":memory:").unwrap();
# diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
#     .execute(&mut conn).unwrap();
# let mut session = conn.create_session().unwrap();
# session.attach_all().unwrap();
# diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 11)")
#     .execute(&mut conn).unwrap();
# diesel::sql_query("INSERT INTO items (id, v) VALUES (2, 20)")
#     .execute(&mut conn).unwrap();
# let changeset = session.changeset().unwrap();
# drop(session);
# let mut replica = SqliteConnection::establish(":memory:").unwrap();
# diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
#     .execute(&mut replica).unwrap();
// Only apply rows whose new `v` is odd.
replica.apply_changeset_v3_with(
    &changeset,
    ApplyFlags::empty(),
    |row: ChangesetRow<'_>| {
        if row.op() != ChangesetOp::Insert {
            return true;
        }
        let v = row.new_value(1).unwrap().unwrap().as_i64();
        v % 2 == 1
    },
    |_| ConflictAction::Abort,
)?;
# Ok::<_, diesel_sqlite_session::ApplyError>(())
```

### Transform Helpers

Three standalone helpers cover the read-only transforms `SQLite` supports on changeset blobs:

- `invert_changeset(bytes)` wraps `sqlite3changeset_invert` and returns the inverse (`INSERT` becomes `DELETE`, `UPDATE` swaps old and new). Materializing the inverted bytes is useful as an undo record you can persist.
- `concat_changesets(a, b)` wraps `sqlite3changeset_concat` and merges two changesets over the same schema.
- `Changegroup` wraps `sqlite3changegroup_new/_schema/_add/_output/_delete` and aggregates n changesets, collapsing duplicate ops on the same primary key (`INSERT` then `UPDATE` on the same key becomes a single `INSERT` with the final values).

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{concat_changesets, invert_changeset, Changegroup, SqliteSessionExt};

// Build three real changesets so every helper below has something to work on.
fn snapshot(script: &[&str]) -> Vec<u8> {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn).unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    for stmt in script {
        diesel::sql_query(*stmt).execute(&mut conn).unwrap();
    }
    session.changeset().unwrap()
}

let insert_a = snapshot(&["INSERT INTO items (id, v) VALUES (1, 10)"]);
let insert_b = snapshot(&["INSERT INTO items (id, v) VALUES (2, 20)"]);
let update_a = snapshot(&[
    "INSERT INTO items (id, v) VALUES (1, 10)",
    "UPDATE items SET v = 99 WHERE id = 1",
]);

let _undo = invert_changeset(&insert_a)?;
let _pairwise = concat_changesets(&insert_a, &insert_b)?;

let mut group = Changegroup::new()?;
group.add(&insert_a)?;
group.add(&update_a)?;
let _merged = group.output()?;
# Ok::<_, diesel_sqlite_session::ChangesetError>(())
```

`Changegroup::set_schema` binds a connection so the group can reconcile `WITHOUT ROWID` tables and per-table column types; plain rowid changesets fold in without one. `Changegroup` is `!Send + !Sync`; drop it before the connection that any attached schema refers to.

`Changegroup::add_change` folds a single positioned `ChangesetReader` row into the group, so you can pick individual rows without materializing an intermediate blob.

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{Changegroup, ChangesetReader, SqliteSessionExt};

# let mut conn = SqliteConnection::establish(":memory:").unwrap();
# diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
#     .execute(&mut conn).unwrap();
# let mut session = conn.create_session().unwrap();
# session.attach_all().unwrap();
# diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 10), (2, 20), (3, 30)")
#     .execute(&mut conn).unwrap();
# let bytes = session.changeset().unwrap();
# drop(session);
let mut group = Changegroup::new()?;
let mut reader = ChangesetReader::open(&bytes)?;
while let Some(row) = reader.next()? {
    let id = row.new_value(0)?.unwrap().as_i64();
    if id % 2 == 1 {
        group.add_change(&row)?;
    }
}
let merged = group.output()?;
# assert!(!merged.is_empty());
# Ok::<_, diesel_sqlite_session::ChangesetError>(())
```

### Session Controls

`Session` grew extension methods wrapping the rest of the session extension:

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{set_stream_size, stream_size, SqliteSessionExt};

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
    .execute(&mut conn).unwrap();
// The `diff` demo needs a second, non-empty database to diff against. Attach
// an in-memory DB as `aux` and mirror the schema plus a distinct row.
diesel::sql_query("ATTACH DATABASE ':memory:' AS aux")
    .execute(&mut conn).unwrap();
diesel::sql_query("CREATE TABLE aux.items (id INTEGER PRIMARY KEY, v INTEGER)")
    .execute(&mut conn).unwrap();
diesel::sql_query("INSERT INTO aux.items (id, v) VALUES (2, 200)")
    .execute(&mut conn).unwrap();

let mut session = conn.create_session().unwrap();
session.set_size_tracking(true).unwrap();
session.set_rowid_tracking(true).unwrap();
session.set_indirect(true);
session.set_table_filter(|table| table != "audit_log");
session.attach_all().unwrap();

diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 100)")
    .execute(&mut conn).unwrap();

let est = session.changeset_size();
let mem = session.memory_used();
println!("estimated {est} bytes / holding {mem} bytes in memory");

// Populate the session with the delta between `aux.items` and `main.items`.
session.diff("aux", "items").unwrap();

// Global default streaming chunk size (see the streamed changeset APIs).
let default_chunk = stream_size().unwrap();
set_stream_size(64 * 1024).unwrap();
# set_stream_size(default_chunk).unwrap();
```

`set_indirect(true)` tags subsequent changes as indirect (readable via `ChangesetRow::indirect()`). `set_table_filter(cb)` swaps in a callback consulted by `attach_all` and `diff`, replacing any previous filter. `set_size_tracking(true)` is required before `changeset_size()` returns non-zero. `set_rowid_tracking(true)` enables `WITHOUT ROWID` tracking. `diff(db, table)` populates the session with the delta between an attached database's table and its same-named counterpart in the session's own database. `stream_size` / `set_stream_size` control the module-wide default chunk size used by streamed APIs.

### Rebaser (Multi-master Convergence)

`Rebaser` wraps `sqlite3rebaser_create` / `_configure` / `_rebase` / `_delete`. It rewrites a changeset so it no longer conflicts with one already applied. Pair it with `ApplyOutcome::rebase` from `apply_changeset_with` to close the multi-master sync loop.

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{ApplyFlags, ConflictAction, Rebaser, SqliteSessionExt};

fn open() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn).unwrap();
    conn
}

// Peer A and peer B both write to id=1 with different values.
let mut peer_a = open();
let mut peer_b = open();
let mut session_a = peer_a.create_session().unwrap();
session_a.attach_all().unwrap();
diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
    .execute(&mut peer_a).unwrap();
let changeset_a = session_a.changeset().unwrap();
drop(session_a);

let mut session_b = peer_b.create_session().unwrap();
session_b.attach_all().unwrap();
diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
    .execute(&mut peer_b).unwrap();
let changeset_b = session_b.changeset().unwrap();
drop(session_b);

// Peer B applies A first, Replace-resolves the conflict, and captures the
// rebase blob.
let outcome = peer_b.apply_changeset_with(
    &changeset_a,
    ApplyFlags::empty(),
    |_| true,
    |_| ConflictAction::Replace,
).unwrap();
assert!(!outcome.rebase.is_empty());

// Peer A rewrites its view of B's outbound changeset so it applies cleanly.
let mut rebaser = Rebaser::new()?;
rebaser.configure(&outcome.rebase)?;
let rebased_b = rebaser.rebase(&changeset_b)?;
peer_a.apply_changeset_with(
    &rebased_b,
    ApplyFlags::empty(),
    |_| true,
    |_| ConflictAction::Abort,
).unwrap();
# Ok::<_, diesel_sqlite_session::ChangesetError>(())
```

`Rebaser` is `!Send + !Sync` and RAII. `configure` can be called repeatedly to stack rebase blobs before calling `rebase`.

### Streamed Variants

Every buffered API has a streamed sibling backed by the `_strm` C entry points. They take `std::io::Read` and `std::io::Write` and let `SQLite` pull or push bytes in chunks; pass anything (a `File`, `TcpStream`, `Cursor<Vec<u8>>`, a compressor, etc.).

```rust
use std::io::Cursor;
use diesel::prelude::*;
use diesel_sqlite_session::{
    concat_changesets_strm, invert_changeset_strm, ApplyFlags, Changegroup, ChangesetOp,
    ChangesetReader, ConflictAction, Rebaser, SqliteSessionExt,
};

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
    .execute(&mut conn).unwrap();
let mut session = conn.create_session().unwrap();
session.attach_all().unwrap();
diesel::sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
    .execute(&mut conn).unwrap();

// Session -> Vec through a writer.
let mut streamed = Vec::new();
session.changeset_strm(&mut streamed)?;

// Iterate a streamed input.
let mut reader = ChangesetReader::open_strm(Cursor::new(streamed.clone()))?;
while let Some(row) = reader.next()? {
    assert_eq!(row.op(), ChangesetOp::Insert);
}

// Apply a streamed input onto a replica.
let mut replica = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
    .execute(&mut replica).unwrap();
replica.apply_changeset_strm_with(
    Cursor::new(streamed.clone()),
    ApplyFlags::empty(),
    |_| true,
    |_| ConflictAction::Abort,
)?;

// Transform helpers stream too.
let mut inverted = Vec::new();
invert_changeset_strm(Cursor::new(streamed.clone()), &mut inverted)?;

let mut group = Changegroup::new()?;
group.add_strm(Cursor::new(streamed.clone()))?;
let mut merged = Vec::new();
group.output_strm(&mut merged)?;

// Rebaser too.
let rebaser = Rebaser::new()?;
let mut rewritten = Vec::new();
rebaser.rebase_strm(Cursor::new(streamed), &mut rewritten)?;
# Ok::<_, Box<dyn std::error::Error>>(())
```

Reader / writer `io::Error`s propagate as `SessionError::WriterIo`, `ChangesetError::ReaderIo` / `WriterIo`, and `ApplyError::ReaderIo`. Panics are caught by the trampoline and reported as the matching `*Panicked` variant, so unwinding never crosses the FFI boundary.

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

- **[sqlite-diff-rs](https://github.com/LucaCappelletti94/sqlite-diff-rs)**: build `SQLite` changesets and patchsets programmatically without linking `SQLite`. Useful for constructing changesets from other sources (`PostgreSQL` CDC, Debezium, Maxwell) and applying them with `diesel-sqlite-session`.

## License

MIT
