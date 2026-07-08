//! Native benchmarks using Criterion.
//!
//! Run with: `cargo bench --bench session_benchmarks`

use std::hint::black_box;
use std::io::Cursor;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    concat_changesets, invert_changeset, ApplyFlags, Changegroup, ChangesetReader, ConflictAction,
    Rebaser, SqliteSessionExt,
};

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

/// Streamed changeset generation vs the buffered variant.
///
/// Same session state and same row counts on both sides. The streamed side
/// writes into a preallocated `Vec<u8>` so allocation cost is comparable.
fn bench_changeset_generation_strm_vs_buffered(c: &mut Criterion) {
    let mut group = c.benchmark_group("changeset_generation_variant");

    for row_count in &[10, 100, 500] {
        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::new("buffered", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();
                    insert_rows(&mut conn, 0, count);
                    black_box(session.changeset().unwrap());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("streamed", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    let mut session = conn.create_session().unwrap();
                    session.attach::<items::table>().unwrap();
                    insert_rows(&mut conn, 0, count);
                    let mut sink: Vec<u8> = Vec::with_capacity(64 * 1024);
                    session.changeset_strm(&mut sink).unwrap();
                    black_box(sink);
                });
            },
        );
    }
    group.finish();
}

/// Streamed apply vs the buffered variant with the same pre-generated
/// changeset, insert-only, no conflicts.
fn bench_apply_changeset_strm_vs_buffered(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_changeset_variant");

    for row_count in &[10, 100, 500] {
        let changeset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, 0, *row_count);
            session.changeset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::new("buffered", row_count),
            &changeset,
            |b, changeset| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    conn.apply_changeset_with(
                        black_box(changeset),
                        ApplyFlags::empty(),
                        |_| true,
                        |_| ConflictAction::Abort,
                    )
                    .unwrap();
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("streamed", row_count),
            &changeset,
            |b, changeset| {
                b.iter(|| {
                    let mut conn = setup_connection();
                    conn.apply_changeset_strm_with(
                        Cursor::new(black_box(changeset.as_slice())),
                        ApplyFlags::empty(),
                        |_| true,
                        |_| ConflictAction::Abort,
                    )
                    .unwrap();
                });
            },
        );
    }
    group.finish();
}

/// v2 apply with a conflict callback that resolves every conflict via
/// `Replace`. The pre-image on the replica differs from the changeset, so
/// the callback fires once per row.
fn bench_apply_changeset_with_conflict_replace(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_changeset_conflict_replace");

    for row_count in &[10, 100, 500] {
        // Source: rows 0..count with value = i.
        let changeset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, 0, *row_count);
            session.changeset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &changeset,
            |b, changeset| {
                b.iter(|| {
                    // Replica: same PKs but different values, so every
                    // incoming insert produces a Conflict.
                    let mut conn = setup_connection();
                    insert_rows(&mut conn, 0, *row_count);
                    conn.apply_changeset_with(
                        black_box(changeset),
                        ApplyFlags::empty(),
                        |_| true,
                        |_| ConflictAction::Replace,
                    )
                    .unwrap();
                });
            },
        );
    }
    group.finish();
}

/// Walk a changeset with `ChangesetReader::next` and touch every column via
/// `new_value`, without applying anything.
fn bench_changeset_reader_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("changeset_reader_iteration");

    for row_count in &[10, 100, 500] {
        let changeset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, 0, *row_count);
            session.changeset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &changeset,
            |b, changeset| {
                b.iter(|| {
                    let mut reader = ChangesetReader::open(black_box(changeset)).unwrap();
                    let mut total: i64 = 0;
                    while let Some(row) = reader.next().unwrap() {
                        // Read id (col 0) as i64; skip nulls elsewhere.
                        if let Ok(Some(v)) = row.new_value(0) {
                            total = total.wrapping_add(v.as_i64());
                        }
                    }
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

/// `invert_changeset` on a pre-built insert-only changeset.
fn bench_invert_changeset(c: &mut Criterion) {
    let mut group = c.benchmark_group("invert_changeset");

    for row_count in &[10, 100, 500] {
        let changeset = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, 0, *row_count);
            session.changeset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &changeset,
            |b, changeset| {
                b.iter(|| {
                    black_box(invert_changeset(black_box(changeset)).unwrap());
                });
            },
        );
    }
    group.finish();
}

/// `concat_changesets` on two disjoint insert-only changesets of the same
/// size.
fn bench_concat_changesets(c: &mut Criterion) {
    let mut group = c.benchmark_group("concat_changesets");

    for row_count in &[10, 100, 500] {
        let a = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, 0, *row_count);
            session.changeset().unwrap()
        };
        let b = {
            let mut conn = setup_connection();
            let mut session = conn.create_session().unwrap();
            session.attach::<items::table>().unwrap();
            insert_rows(&mut conn, *row_count, *row_count * 2);
            session.changeset().unwrap()
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count * 2).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &(a, b),
            |bench, (a, b)| {
                bench.iter(|| {
                    black_box(concat_changesets(black_box(a), black_box(b)).unwrap());
                });
            },
        );
    }
    group.finish();
}

/// Fold 5 disjoint changesets into a `Changegroup` and materialize the
/// merged output.
fn bench_changegroup_fold_five(c: &mut Criterion) {
    let mut group = c.benchmark_group("changegroup_fold_five");

    for per_changeset in &[10, 100, 500] {
        let inputs: Vec<Vec<u8>> = (0..5)
            .map(|i| {
                let start = i * per_changeset;
                let end = (i + 1) * per_changeset;
                let mut conn = setup_connection();
                let mut session = conn.create_session().unwrap();
                session.attach::<items::table>().unwrap();
                insert_rows(&mut conn, start, end);
                session.changeset().unwrap()
            })
            .collect();

        group.throughput(Throughput::Elements(
            u64::try_from(*per_changeset * 5).unwrap(),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(per_changeset),
            &inputs,
            |b, inputs| {
                b.iter(|| {
                    let mut cg = Changegroup::new().unwrap();
                    for input in inputs {
                        cg.add(black_box(input)).unwrap();
                    }
                    black_box(cg.output().unwrap());
                });
            },
        );
    }
    group.finish();
}

/// `Rebaser::rebase` on a pre-built rebase blob.
fn bench_rebaser_rebase(c: &mut Criterion) {
    let mut group = c.benchmark_group("rebaser_rebase");

    for row_count in &[10, 100, 500] {
        // Source A inserts row 0..count with value = i. Replica has the same
        // PKs with different values, so applying A on the replica with
        // `Replace` yields a non-empty rebase blob.
        let (rebase_blob, changeset_b) = {
            let mut src_a = setup_connection();
            let mut sess_a = src_a.create_session().unwrap();
            sess_a.attach::<items::table>().unwrap();
            insert_rows(&mut src_a, 0, *row_count);
            let cs_a = sess_a.changeset().unwrap();
            drop(sess_a);

            let mut replica = setup_connection();
            insert_rows(&mut replica, 0, *row_count);
            let outcome = replica
                .apply_changeset_with(
                    &cs_a,
                    ApplyFlags::empty(),
                    |_| true,
                    |_| ConflictAction::Replace,
                )
                .unwrap();

            // Changeset B is the same shape from a fresh peer.
            let mut src_b = setup_connection();
            let mut sess_b = src_b.create_session().unwrap();
            sess_b.attach::<items::table>().unwrap();
            insert_rows(&mut src_b, 0, *row_count);
            let cs_b = sess_b.changeset().unwrap();

            (outcome.rebase, cs_b)
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));
        group.bench_with_input(
            BenchmarkId::from_parameter(row_count),
            &(rebase_blob, changeset_b),
            |b, (rebase_blob, changeset_b)| {
                b.iter(|| {
                    let mut rebaser = Rebaser::new().unwrap();
                    rebaser.configure(black_box(rebase_blob)).unwrap();
                    black_box(rebaser.rebase(black_box(changeset_b)).unwrap());
                });
            },
        );
    }
    group.finish();
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
              bench_full_replication,
              bench_changeset_generation_strm_vs_buffered,
              bench_apply_changeset_strm_vs_buffered,
              bench_apply_changeset_with_conflict_replace,
              bench_changeset_reader_iteration,
              bench_invert_changeset,
              bench_concat_changesets,
              bench_changegroup_fold_five,
              bench_rebaser_rebase
}

criterion_main!(benches);
