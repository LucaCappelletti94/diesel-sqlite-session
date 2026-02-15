//! Native benchmarks using Criterion.
//!
//! Run with: cargo bench --bench session_benchmarks

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{ConflictAction, SqliteSessionExt};
use std::hint::black_box;
use std::time::Duration;

diesel::table! {
    items (id) {
        id -> Integer,
        name -> Nullable<Text>,
        value -> Nullable<Integer>,
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

/// Benchmark session creation.
fn bench_session_creation(c: &mut Criterion) {
    c.bench_function("session_creation", |b| {
        b.iter(|| {
            let mut conn = SqliteConnection::establish(":memory:").unwrap();
            let session = conn.create_session().unwrap();
            black_box(session);
        });
    });
}

/// Benchmark attaching tables.
fn bench_attach_table(c: &mut Criterion) {
    c.bench_function("attach_single_table", |b| {
        b.iter(|| {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            black_box(session);
        });
    });
}

/// Benchmark patchset generation with varying row counts.
fn bench_patchset_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("patchset_generation");

    for row_count in [10, 100, 500].iter() {
        group.throughput(Throughput::Elements(*row_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();

                    for i in 0..count {
                        sql_query(format!(
                            "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                        ))
                        .execute(&mut conn)
                        .unwrap();
                    }
                    let patchset = session.patchset().unwrap();
                    black_box(patchset);
                });
            },
        );
    }
    group.finish();
}

/// Benchmark changeset generation with varying row counts.
fn bench_changeset_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("changeset_generation");

    for row_count in [10, 100, 500].iter() {
        group.throughput(Throughput::Elements(*row_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();

                    for i in 0..count {
                        sql_query(format!(
                            "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                        ))
                        .execute(&mut conn)
                        .unwrap();
                    }
                    let changeset = session.changeset().unwrap();
                    black_box(changeset);
                });
            },
        );
    }
    group.finish();
}

/// Benchmark applying patchsets with varying sizes.
fn bench_apply_patchset(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_patchset");

    for row_count in [10, 100, 500].iter() {
        // Pre-generate the patchset
        let patchset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();

            for i in 0..*row_count {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }
            session.patchset().unwrap()
        };

        group.throughput(Throughput::Elements(*row_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &patchset,
            |b, patchset| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    conn.apply_patchset(black_box(patchset), |_| ConflictAction::Abort)
                        .unwrap();
                });
            },
        );
    }
    group.finish();
}

/// Benchmark mixed operations (INSERT, UPDATE, DELETE).
fn bench_mixed_operations(c: &mut Criterion) {
    c.bench_function("mixed_operations_75", |b| {
        b.iter(|| {
            let mut conn = setup_connection();
            // Pre-populate with 50 rows
            for i in 0..50 {
                sql_query(format!(
                    "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {i})"
                ))
                .execute(&mut conn)
                .unwrap();
            }

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

            let patchset = session.patchset().unwrap();
            black_box(patchset);
        });
    });
}

/// Benchmark full replication workflow.
fn bench_full_replication(c: &mut Criterion) {
    c.bench_function("full_replication_100", |b| {
        b.iter(|| {
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

            black_box(replica);
        });
    });
}

fn fast_config() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(30)
}

criterion_group! {
    name = benches;
    config = fast_config();
    targets = bench_session_creation,
              bench_attach_table,
              bench_patchset_generation,
              bench_changeset_generation,
              bench_apply_patchset,
              bench_mixed_operations,
              bench_full_replication
}

criterion_main!(benches);
