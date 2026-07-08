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

Full API reference and runnable examples for every method live on [docs.rs](https://docs.rs/diesel-sqlite-session), organized by module: pre-update hook, incremental blob I/O, changeset iterator, enhanced apply (`v2` and `v3`), transform helpers (`invert`, `concat`, `Changegroup`), session controls (`diff`, `set_table_filter`, size and rowid tracking), and the `Rebaser` for multi-master convergence. Every SQLite entry point that ships a streamed C sibling has a matching `_strm` method on the Rust side.

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
