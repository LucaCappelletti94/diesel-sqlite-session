# diesel-sqlite-session

`SQLite` session extension support for Diesel ORM.

This crate provides `SQLite` [session extension](https://sqlite.org/sessionintro.html) support for Diesel, enabling tracking of database changes and generation of transferable changesets/patchsets for replication, sync, and audit purposes.

> **Note**: This crate requires access to Diesel's raw `SQLite` connection handle via `with_raw_connection`. Until [diesel#4966](https://github.com/diesel-rs/diesel/pull/4966) is merged, you must use a fork that exposes this API:
>
> ```toml
> [dependencies]
> diesel = { git = "https://github.com/LucaCappelletti94/diesel", branch = "sqlite-session-changeset", features = ["sqlite"] }
> ```

## Features

- **Change tracking**: Track INSERT, UPDATE, and DELETE operations on tables
- **Changeset/patchset generation**: Generate compact binary representations of changes
- **Replication support**: Apply changesets/patchsets to replica databases
- **Conflict handling**: Configurable conflict resolution strategies
- **Type-safe API**: Attach tables using Diesel's table types
- **Cross-platform**: Supports native targets and WebAssembly

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
diesel-sqlite-session = { git = "https://github.com/LucaCappelletti94/diesel-sqlite-session" }
# Required until diesel#4966 is merged:
diesel = { git = "https://github.com/LucaCappelletti94/diesel", branch = "sqlite-session-changeset", features = ["sqlite"] }
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

// Create source connection and track changes
let mut source = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    .execute(&mut source).unwrap();

// Create a session and attach the table
let mut session = source.create_session().unwrap();
session.attach::<users::table>().unwrap();

// Make changes
diesel::sql_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
    .execute(&mut source).unwrap();

// Generate patchset
let patchset = session.patchset().unwrap();

// Apply to replica
let mut replica = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    .execute(&mut replica).unwrap();

replica.apply_patchset(&patchset, |_| ConflictAction::Abort).unwrap();
```

## API Overview

### Extension Trait

The `SqliteSessionExt` trait extends `SqliteConnection` with session capabilities:

```rust
use diesel::prelude::*;
use diesel_sqlite_session::{SqliteSessionExt, ConflictAction};

let mut conn = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY)").execute(&mut conn).unwrap();

let mut session = conn.create_session().unwrap();
session.attach_by_name("t").unwrap();
diesel::sql_query("INSERT INTO t VALUES (1)").execute(&mut conn).unwrap();
let patchset = session.patchset().unwrap();

// Apply to another connection
let mut conn2 = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY)").execute(&mut conn2).unwrap();
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
diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").execute(&mut conn).unwrap();

let mut session = conn.create_session().unwrap();

// Type-safe table attachment (recommended)
session.attach::<users::table>().unwrap();

// Or attach all tables
// session.attach_all().unwrap();

// Or dynamic table name (for runtime schemas)
// session.attach_by_name("dynamic_table").unwrap();

// Make some changes
diesel::sql_query("INSERT INTO users VALUES (1, 'Alice')").execute(&mut conn).unwrap();

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

// Create source and generate patchset
let mut source = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)").execute(&mut source).unwrap();
let mut session = source.create_session().unwrap();
session.attach_by_name("t").unwrap();
diesel::sql_query("INSERT INTO t VALUES (1, 100)").execute(&mut source).unwrap();
let patchset = session.patchset().unwrap();

// Apply with conflict handling
let mut replica = SqliteConnection::establish(":memory:").unwrap();
diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)").execute(&mut replica).unwrap();

replica.apply_patchset(&patchset, |conflict_type| {
    match conflict_type {
        ConflictType::Data => ConflictAction::Replace,    // Overwrite
        ConflictType::NotFound => ConflictAction::Omit,   // Skip
        ConflictType::Conflict => ConflictAction::Abort,  // Stop
        _ => ConflictAction::Abort,
    }
}).unwrap();
```

## Changesets vs Patchsets

| Feature | Changeset | Patchset |
|---------|-----------|----------|
| Contains old values | Yes | No |
| Size | Larger | Smaller |
| Conflict detection | Precise | Basic |
| Use case | Full audit trail | Efficient sync |

**Bytes per row** (measured with 3-column table):

- Both formats: ~28-30 bytes/row

## Platform Support

| Platform | Backend | Status |
|----------|---------|--------|
| Linux/macOS/Windows | `libsqlite3-sys` (bundled) | Supported |
| WebAssembly | `sqlite-wasm-rs` | Supported |

## Benchmarks

### Native Performance (Linux `x86_64`)

Benchmarks run using Criterion on native targets with LTO and single codegen unit.

#### Core Operations

| Operation | Time (mean ± std) | Throughput |
|-----------|-------------------|------------|
| Session creation | 8.5 ± 0.03 µs | 118K ops/sec |
| Attach table | 36.4 ± 0.3 µs | 27K ops/sec |

#### Patchset/Changeset Generation

| Rows | Patchset (mean ± std) | Changeset (mean ± std) | Throughput |
|------|------------------------|------------------------|------------|
| 10 | 82 ± 1 µs | 85 ± 1 µs | 120K rows/sec |
| 100 | 285 ± 3 µs | 278 ± 2 µs | 355K rows/sec |
| 500 | 1.19 ± 0.01 ms | 1.16 ± 0.01 ms | 425K rows/sec |

#### Apply Operations

| Rows | Apply Patchset (mean ± std) | Apply Changeset (mean ± std) | Throughput |
|------|------------------------------|------------------------------|------------|
| 10 | 66 ± 1 µs | 64 ± 1 µs | 154K rows/sec |
| 100 | 110 ± 1 µs | 110 ± 1 µs | 910K rows/sec |
| 500 | 358 ± 2 µs | 358 ± 2 µs | 1.4M rows/sec |

#### End-to-End Workflows

| Workflow | Time (mean ± std) |
|----------|-------------------|
| Mixed operations (75 changes) | 323 ± 2 µs |
| Full replication (100 rows) | 395 ± 2 µs |

### Comparison with rusqlite

diesel-sqlite-session performs comparably to rusqlite's session extension:

| Operation | diesel-sqlite-session | rusqlite | Difference |
|-----------|----------------------|----------|------------|
| Session creation | 36.4 ± 0.4 µs | 34.6 ± 0.3 µs | +5% |
| Attach table | 35.6 ± 0.2 µs | 33.9 ± 0.2 µs | +5% |
| Patchset (500 rows) | 1.19 ± 0.01 ms | 1.44 ± 0.01 ms | **-17%** |
| Changeset (500 rows) | 1.19 ± 0.01 ms | 1.46 ± 0.02 ms | **-18%** |
| Apply patchset (500 rows) | 389 ± 5 µs | 379 ± 7 µs | +3% |
| Mixed operations | 327 ± 2 µs | 371 ± 6 µs | **-12%** |
| Full replication | 402 ± 6 µs | 458 ± 6 µs | **-12%** |

#### Interpretation

The session extension FFI calls are identical between diesel-sqlite-session and rusqlite. For session creation and attach operations, there's a small ~5% difference attributable to connection setup.

For data-heavy operations (patchset/changeset generation, mixed operations, full replication), **diesel-sqlite-session is 12-18% faster** due to Diesel's efficient query builder and prepared statement handling.

Performance should not be a factor in choosing between the two—use whichever ORM fits your project.

#### Browser/WASM Support

Both diesel-sqlite-session and rusqlite now support `wasm32-unknown-unknown` (browser WebAssembly) via [sqlite-wasm-rs](https://crates.io/crates/sqlite-wasm-rs). rusqlite added this support in [PR #1769](https://github.com/rusqlite/rusqlite/pull/1769) (December 2025).

However, **rusqlite's session extension does not work in WASM**. The session extension requires `buildtime_bindgen`, which generates native bindings incompatible with WebAssembly. diesel-sqlite-session solves this by providing hand-written FFI bindings that work on both native and WASM targets.

This means diesel-sqlite-session is the only option for:

- **Offline-first web applications** with change tracking and sync
- **Cross-platform replication** between browser, mobile, and server
- **Browser-based collaborative editing** with conflict resolution

### WebAssembly Performance

Benchmarks run using wasm-bindgen-test in headless browsers.

#### Chrome vs Firefox Comparison

| Operation | Chrome (mean ± std) | Firefox (mean ± std) |
|-----------|---------------------|----------------------|
| Session creation | 0.05 ± 0.01 ms | 0.03 ± 0.01 ms |
| Attach table | 0.02 ± 0.01 ms | 0.03 ± 0.01 ms |
| Patchset (100 rows) | 0.40 ± 0.11 ms | 0.48 ± 0.08 ms |
| Patchset (1000 rows) | 1.67 ± 0.07 ms | 2.81 ± 0.10 ms |
| Apply patchset (100 rows) | 0.35 ± 0.03 ms | 0.43 ± 0.01 ms |
| Apply patchset (500 rows) | 1.87 ± 0.59 ms | 1.51 ± 0.11 ms |
| Mixed ops (75 changes) | 1.93 ± 0.27 ms | 2.27 ± 0.01 ms |
| Full replication (100 rows) | 3.25 ± 0.36 ms | 3.78 ± 0.18 ms |

**WASM vs Native**: WebAssembly performance is approximately 8-10x slower than native for most operations. Chrome and Firefox show comparable performance within measurement variance. This overhead is expected due to:

- JavaScript/WASM boundary overhead
- sqlite-wasm-rs overhead compared to native `SQLite`
- Browser sandbox constraints

## Running Benchmarks

```bash
# Native benchmarks (Criterion)
cargo bench --bench session_benchmarks

# Comparison benchmarks (vs rusqlite)
cargo bench --bench comparison_benchmarks

# WASM benchmarks (requires wasm-pack)
cargo install wasm-pack
cd wasm-bench && wasm-pack test --headless --firefox -- -- --nocapture
cd wasm-bench && wasm-pack test --headless --chrome -- -- --nocapture
```

## Use Cases

- **Offline-first applications**: Sync changes when connectivity is restored
- **Multi-master replication**: Propagate changes between database instances
- **Audit logging**: Capture exact changes for compliance
- **Undo/redo systems**: Store changesets for reverting operations
- **Edge computing**: Sync edge databases with central servers

## Related Projects

- **[sqlite-diff-rs](https://github.com/LucaCappelletti94/sqlite-diff-rs)** - Build `SQLite` changesets/patchsets programmatically without requiring `SQLite`. Useful for constructing changesets from other sources (`PostgreSQL` CDC, Debezium, Maxwell) and applying them with diesel-sqlite-session.

## License

MIT
