# diesel-sqlite-session

SQLite session extension support for Diesel ORM.

This crate provides SQLite [session extension](https://sqlite.org/sessionintro.html) support for Diesel, enabling tracking of database changes and generation of transferable changesets/patchsets for replication, sync, and audit purposes.

> **Note**: This crate requires access to Diesel's raw SQLite connection handle via `with_raw_connection`. Until [diesel#4966](https://github.com/diesel-rs/diesel/pull/4966) is merged, you must use a fork that exposes this API:
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

### Native Performance (Linux x86_64)

Benchmarks run using Criterion on native targets.

#### Core Operations

| Operation | Time | Throughput |
|-----------|------|------------|
| Session creation | 8.6 µs | 116K ops/sec |
| Attach table | 37 µs | 27K ops/sec |

#### Patchset/Changeset Generation

| Rows | Patchset | Changeset | Throughput |
|------|----------|-----------|------------|
| 10 | 85 µs | 86 µs | 117K rows/sec |
| 100 | 343 µs | 338 µs | 292K rows/sec |
| 500 | 1.54 ms | 1.51 ms | 325K rows/sec |

#### Apply Operations

| Rows | Apply Patchset | Apply Changeset | Throughput |
|------|----------------|-----------------|------------|
| 10 | 63 µs | 63 µs | 156K rows/sec |
| 100 | 110 µs | 110 µs | 910K rows/sec |
| 500 | 360 µs | 360 µs | 1.4M rows/sec |

#### End-to-End Workflows

| Workflow | Time |
|----------|------|
| Mixed operations (75 changes) | 383 µs |
| Full replication (100 rows) | 459 µs |

### Comparison with rusqlite

diesel-sqlite-session adds minimal overhead compared to using rusqlite's session extension directly:

| Operation | diesel-sqlite-session | rusqlite | Overhead |
|-----------|----------------------|----------|----------|
| Session creation | 39 µs | 37 µs | +5% |
| Attach table | 41 µs | 37 µs | +10% |
| Patchset (500 rows) | 1.56 ms | 1.42 ms | +10% |
| Apply patchset (500 rows) | 360 µs | 348 µs | +3% |

#### Interpretation

**Where does the overhead come from?**

The 5-10% overhead is entirely attributable to Diesel's connection abstraction layer. When you call `conn.create_session()`, diesel-sqlite-session must access the raw SQLite connection handle through Diesel's `with_raw_connection` API. This indirection adds a small but measurable cost compared to rusqlite's direct access.

**Is this overhead significant?**

In practice, **no**. Consider a typical replication workflow:

- Generating a patchset for 500 rows takes ~1.5ms with diesel-sqlite-session
- The "extra" cost vs rusqlite is ~140µs (0.14ms)
- A single network round-trip to a cloud database is typically 1-10ms
- Disk I/O for persisting data adds additional milliseconds

The Diesel overhead represents less than 1% of total latency in any realistic deployment scenario. You gain type-safe table attachment, seamless integration with Diesel queries, and a consistent API—well worth the microseconds.

**When might you prefer rusqlite directly?**

- You're not already using Diesel in your project
- You need absolute minimum latency in a tight loop (rare)
- You're building a low-level replication library rather than an application

#### Browser/WASM Support

Both diesel-sqlite-session and rusqlite now support `wasm32-unknown-unknown` (browser WebAssembly) via [sqlite-wasm-rs](https://crates.io/crates/sqlite-wasm-rs). rusqlite added this support in [PR #1769](https://github.com/rusqlite/rusqlite/pull/1769) (December 2025).

However, **rusqlite's session extension does not work in WASM**. The session extension requires `buildtime_bindgen`, which generates native bindings incompatible with WebAssembly. diesel-sqlite-session solves this by providing hand-written FFI bindings that work on both native and WASM targets.

This means diesel-sqlite-session is the only option for:

- **Offline-first web applications** with change tracking and sync
- **Cross-platform replication** between browser, mobile, and server
- **Browser-based collaborative editing** with conflict resolution

### WebAssembly Performance (Firefox)

Benchmarks run using wasm-bindgen-test in headless Firefox.

| Operation | Time | Throughput |
|-----------|------|------------|
| Session creation | 0.03 ms | 33K ops/sec |
| Attach table | 0.025 ms | 40K ops/sec |
| Patchset (100 rows) | 0.41 ms | 2.4K ops/sec |
| Patchset (1000 rows) | 2.92 ms | 342 ops/sec |
| Apply patchset (100 rows) | 0.57 ms | 1.75K ops/sec |
| Full replication (100 rows) | 3.37 ms | 296 ops/sec |

**WASM vs Native**: WebAssembly performance is approximately 3-5x slower than native for most operations. This is expected due to:

- JavaScript/WASM boundary overhead
- sqlite-wasm-rs overhead compared to native SQLite
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
```

## Use Cases

- **Offline-first applications**: Sync changes when connectivity is restored
- **Multi-master replication**: Propagate changes between database instances
- **Audit logging**: Capture exact changes for compliance
- **Undo/redo systems**: Store changesets for reverting operations
- **Edge computing**: Sync edge databases with central servers

## Related Projects

- **[sqlite-diff-rs](https://github.com/LucaCappelletti94/sqlite-diff-rs)** - Build SQLite changesets/patchsets programmatically without requiring SQLite. Useful for constructing changesets from other sources (PostgreSQL CDC, Debezium, Maxwell) and applying them with diesel-sqlite-session.

## License

MIT

## Contributing

Contributions welcome! Please ensure:

- All tests pass (`cargo test`)
- Code is formatted (`cargo fmt`)
- No clippy warnings (`cargo clippy`)
