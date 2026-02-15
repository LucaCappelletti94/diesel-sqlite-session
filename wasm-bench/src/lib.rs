//! WASM benchmarks for diesel-sqlite-session.
//!
//! These benchmarks run headlessly in a browser via wasm-bindgen-test.
//!
//! Run with output (for SSH):
//!   cd wasm-bench && wasm-pack test --headless --firefox -- -- --nocapture
//!
//! Run without output:
//!   cd wasm-bench && wasm-pack test --headless --firefox

#![cfg(target_arch = "wasm32")]

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{ConflictAction, SqliteSessionExt};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

diesel::table! {
    items (id) {
        id -> Integer,
        name -> Nullable<Text>,
        value -> Nullable<Integer>,
    }
}

/// Get high-resolution timestamp from Performance API.
fn now() -> f64 {
    web_sys::window()
        .expect("no window")
        .performance()
        .expect("no performance")
        .now()
}

/// Log benchmark output (visible with --nocapture flag).
fn log(msg: &str) {
    wasm_bindgen_test::console_log!("{}", msg);
}

/// Benchmark result.
struct BenchResult {
    name: String,
    iterations: u32,
    mean_ms: f64,
    min_ms: f64,
    max_ms: f64,
    ops_per_sec: f64,
}

impl BenchResult {
    fn print(&self) {
        log(&format!(
            "{:<30} {:>5} iters | mean: {:>8.3}ms | min: {:>8.3}ms | max: {:>8.3}ms | {:>10.1} ops/sec",
            self.name, self.iterations, self.mean_ms, self.min_ms, self.max_ms, self.ops_per_sec
        ));
    }
}

/// Setup a connection with a test table.
fn setup_connection() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
        .execute(&mut conn)
        .unwrap();
    conn
}

/// Run a benchmark with the given closure.
fn bench<F, R>(name: &str, iterations: u32, mut setup: impl FnMut() -> R, mut f: F) -> BenchResult
where
    F: FnMut(R),
{
    let mut times = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let input = setup();
        let start = now();
        f(input);
        let elapsed = now() - start;
        times.push(elapsed);
    }

    let total: f64 = times.iter().sum();
    let mean = total / iterations as f64;
    let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    BenchResult {
        name: name.to_string(),
        iterations,
        mean_ms: mean,
        min_ms: min,
        max_ms: max,
        ops_per_sec: 1000.0 / mean,
    }
}


// ============================================================================
// Benchmarks
// ============================================================================

#[wasm_bindgen_test]
async fn bench_session_creation() {
    log("=== Session Creation Benchmark ===");

    let result = bench("session_creation", 100, setup_connection, |mut conn| {
        let _session = conn.create_session().unwrap();
    });
    result.print();
}

#[wasm_bindgen_test]
async fn bench_attach_table() {
    log("=== Attach Table Benchmark ===");

    let result = bench("attach_table", 100, setup_connection, |mut conn| {
        let mut session = conn.create_session().unwrap();
        session.attach::<items::table>().unwrap();
    });
    result.print();
}

#[wasm_bindgen_test]
async fn bench_patchset_generation() {
    log("=== Patchset Generation Benchmark ===");

    for row_count in [10, 100, 500, 1000] {
        let iterations = match row_count {
            10 => 50,
            100 => 20,
            500 => 10,
            _ => 5,
        };

        let result = bench(
            &format!("patchset_{row_count}_rows"),
            iterations,
            || {
                let mut conn = setup_connection();
                let mut session = conn.create_session().unwrap();
                session.attach::<items::table>().unwrap();
                for i in 0..row_count {
                    sql_query(format!(
                        "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                    ))
                    .execute(&mut conn)
                    .unwrap();
                }
                (conn, session)
            },
            |(_conn, mut session)| {
                let _patchset = session.patchset().unwrap();
            },
        );
        result.print();
    }
}

#[wasm_bindgen_test]
async fn bench_changeset_generation() {
    log("=== Changeset Generation Benchmark ===");

    for row_count in [10, 100, 500, 1000] {
        let iterations = match row_count {
            10 => 50,
            100 => 20,
            500 => 10,
            _ => 5,
        };

        let result = bench(
            &format!("changeset_{row_count}_rows"),
            iterations,
            || {
                let mut conn = setup_connection();
                let mut session = conn.create_session().unwrap();
                session.attach::<items::table>().unwrap();
                for i in 0..row_count {
                    sql_query(format!(
                        "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                    ))
                    .execute(&mut conn)
                    .unwrap();
                }
                (conn, session)
            },
            |(_conn, mut session)| {
                let _changeset = session.changeset().unwrap();
            },
        );
        result.print();
    }
}

#[wasm_bindgen_test]
async fn bench_apply_patchset() {
    log("=== Apply Patchset Benchmark ===");

    for row_count in [10, 100, 500] {
        // Pre-generate patchset
        let patchset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            for i in 0..row_count {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }
            session.patchset().unwrap()
        };

        let iterations = match row_count {
            10 => 50,
            100 => 20,
            _ => 10,
        };

        let result = bench(
            &format!("apply_patchset_{row_count}_rows"),
            iterations,
            || (setup_connection(), patchset.clone()),
            |(mut conn, patchset)| {
                conn.apply_patchset(&patchset, |_| ConflictAction::Abort)
                    .unwrap();
            },
        );
        result.print();
    }
}

#[wasm_bindgen_test]
async fn bench_apply_changeset() {
    log("=== Apply Changeset Benchmark ===");

    for row_count in [10, 100, 500] {
        // Pre-generate changeset
        let changeset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            for i in 0..row_count {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }
            session.changeset().unwrap()
        };

        let iterations = match row_count {
            10 => 50,
            100 => 20,
            _ => 10,
        };

        let result = bench(
            &format!("apply_changeset_{row_count}_rows"),
            iterations,
            || (setup_connection(), changeset.clone()),
            |(mut conn, changeset)| {
                conn.apply_changeset(&changeset, |_| ConflictAction::Abort)
                    .unwrap();
            },
        );
        result.print();
    }
}

#[wasm_bindgen_test]
async fn bench_mixed_operations() {
    log("=== Mixed Operations Benchmark ===");

    let result = bench(
        "mixed_ops_75_changes",
        10,
        || {
            let mut conn = setup_connection();
            // Pre-populate with 50 rows
            for i in 0..50 {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }
            conn
        },
        |mut conn| {
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();

            // 25 inserts
            for i in 50..75 {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'new{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }

            // 25 updates
            for i in 0..25 {
                sql_query(format!(
                    "UPDATE items SET value = {} WHERE id = {}",
                    i * 2,
                    i
                ))
                .execute(&mut conn)
                .unwrap();
            }

            // 25 deletes
            for i in 25..50 {
                sql_query(format!("DELETE FROM items WHERE id = {i}"))
                    .execute(&mut conn)
                    .unwrap();
            }

            let _patchset = session.patchset().unwrap();
        },
    );
    result.print();
}

#[wasm_bindgen_test]
async fn bench_full_replication_workflow() {
    log("=== Full Replication Workflow Benchmark ===");

    let result = bench(
        "full_replication_100_rows",
        10,
        || {},
        |()| {
            // Source
            let mut source = setup_connection();
            let mut session = source.create_session().unwrap();
            session.attach::<items::table>().unwrap();

            for i in 0..100 {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut source)
                .unwrap();
            }

            let patchset = session.patchset().unwrap();

            // Replica
            let mut replica = setup_connection();
            replica
                .apply_patchset(&patchset, |_| ConflictAction::Abort)
                .unwrap();
        },
    );
    result.print();
}

#[wasm_bindgen_test]
async fn bench_patchset_size() {
    log("=== Patchset/Changeset Size Analysis ===");
    log("Rows     | Patchset | Changeset | Bytes/row (patch) | Bytes/row (change)");
    log("---------|----------|-----------|-------------------|-------------------");

    for row_count in [10, 100, 1000, 5000] {
        let mut conn = setup_connection();
        let mut session = conn.create_session().unwrap();
        session.attach::<items::table>().unwrap();

        for i in 0..row_count {
            sql_query(format!(
                "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
            ))
            .execute(&mut conn)
            .unwrap();
        }

        let patchset = session.patchset().unwrap();
        let changeset = session.changeset().unwrap();

        let patch_per_row = patchset.len() as f64 / row_count as f64;
        let change_per_row = changeset.len() as f64 / row_count as f64;

        log(&format!(
            "{:<8} | {:>8} | {:>9} | {:>17.1} | {:>18.1}",
            row_count,
            patchset.len(),
            changeset.len(),
            patch_per_row,
            change_per_row
        ));
    }
}
