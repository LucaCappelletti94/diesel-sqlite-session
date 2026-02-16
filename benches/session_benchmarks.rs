//! Native benchmarks using Criterion.
//!
//! Run with: `cargo bench --bench session_benchmarks`

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

#[derive(Insertable)]
#[diesel(table_name = items)]
struct NewItem {
    id: i32,
    name: String,
    value: i32,
}

/// Setup a connection with a test table.
fn setup_connection() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
        .execute(&mut conn)
        .unwrap();
    conn
}

/// Insert rows using ORM DSL.
fn insert_rows(conn: &mut SqliteConnection, start: i32, end: i32) {
    for i in start..end {
        diesel::insert_into(items::table)
            .values(NewItem {
                id: i,
                name: format!("item{i}"),
                value: i,
            })
            .execute(conn)
            .unwrap();
    }
}

/// Update rows using ORM DSL.
fn update_rows(conn: &mut SqliteConnection, count: i32) {
    for i in 0..count {
        diesel::update(items::table.filter(items::id.eq(i)))
            .set(items::value.eq(i * 2))
            .execute(conn)
            .unwrap();
    }
}

/// Delete rows using ORM DSL.
fn delete_rows(conn: &mut SqliteConnection, start: i32, end: i32) {
    for i in start..end {
        diesel::delete(items::table.filter(items::id.eq(i)))
            .execute(conn)
            .unwrap();
    }
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

    for row_count in &[10, 100, 500] {
        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();

                    insert_rows(&mut conn, 0, count);

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

    for row_count in &[10, 100, 500] {
        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();

                    insert_rows(&mut conn, 0, count);

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

    for row_count in &[10, 100, 500] {
        // Pre-generate the patchset
        let patchset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();

            insert_rows(&mut conn, 0, *row_count);

            session.patchset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
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
            insert_rows(&mut conn, 0, 50);

            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();

            // 25 inserts
            insert_rows(&mut conn, 50, 75);

            // 25 updates
            update_rows(&mut conn, 25);

            // 25 deletes
            delete_rows(&mut conn, 25, 50);

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

            insert_rows(&mut source, 0, 100);

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
