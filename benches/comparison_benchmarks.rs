//! Comparative benchmarks: diesel-sqlite-session vs rusqlite.
//!
//! This benchmark compares the performance of diesel-sqlite-session against
//! rusqlite's native session extension support to measure any overhead.
//!
//! Run with: `cargo bench --bench comparison_benchmarks`

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::Duration;

// ============================================================================
// diesel-sqlite-session implementation
// ============================================================================
mod diesel_impl {
    use diesel::prelude::*;
    use diesel::sql_query;
    use diesel_sqlite_session::{ConflictAction, Session, SqliteSessionExt};

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

    pub fn setup_connection() -> SqliteConnection {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
            .execute(&mut conn)
            .unwrap();
        conn
    }

    pub fn create_session(conn: &mut SqliteConnection) -> Session {
        conn.create_session().unwrap()
    }

    pub fn attach_table(session: &mut Session) {
        session.attach::<items::table>().unwrap();
    }

    pub fn insert_rows(conn: &mut SqliteConnection, count: i32) {
        insert_rows_range(conn, 0, count);
    }

    pub fn insert_rows_range(conn: &mut SqliteConnection, start: i32, end: i32) {
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

    pub fn update_rows(conn: &mut SqliteConnection, count: i32) {
        for i in 0..count {
            diesel::update(items::table.filter(items::id.eq(i)))
                .set(items::value.eq(i * 2))
                .execute(conn)
                .unwrap();
        }
    }

    pub fn delete_rows(conn: &mut SqliteConnection, start: i32, end: i32) {
        for i in start..end {
            diesel::delete(items::table.filter(items::id.eq(i)))
                .execute(conn)
                .unwrap();
        }
    }

    pub fn generate_patchset(session: &mut Session) -> Vec<u8> {
        session.patchset().unwrap()
    }

    pub fn generate_changeset(session: &mut Session) -> Vec<u8> {
        session.changeset().unwrap()
    }

    pub fn apply_patchset_to_conn(conn: &mut SqliteConnection, patchset: &[u8]) {
        conn.apply_patchset(patchset, |_| ConflictAction::Abort)
            .unwrap();
    }

    pub fn apply_changeset_to_conn(conn: &mut SqliteConnection, changeset: &[u8]) {
        conn.apply_changeset(changeset, |_| ConflictAction::Abort)
            .unwrap();
    }

    pub fn is_empty(session: &Session) -> bool {
        session.is_empty()
    }
}

// ============================================================================
// rusqlite implementation
// ============================================================================
mod rusqlite_impl {
    use rusqlite::session::{ConflictAction, ConflictType, Session};
    use rusqlite::Connection;

    pub fn setup_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)",
            [],
        )
        .unwrap();
        conn
    }

    pub fn insert_rows(conn: &Connection, count: i32) {
        insert_rows_range(conn, 0, count);
    }

    pub fn insert_rows_range(conn: &Connection, start: i32, end: i32) {
        for i in start..end {
            conn.execute(
                "INSERT INTO items (id, name, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![i, format!("item{i}"), i],
            )
            .unwrap();
        }
    }

    pub fn update_rows(conn: &Connection, count: i32) {
        for i in 0..count {
            conn.execute(
                "UPDATE items SET value = ?1 WHERE id = ?2",
                rusqlite::params![i * 2, i],
            )
            .unwrap();
        }
    }

    pub fn delete_rows(conn: &Connection, start: i32, end: i32) {
        for i in start..end {
            conn.execute("DELETE FROM items WHERE id = ?1", rusqlite::params![i])
                .unwrap();
        }
    }

    /// Create session, insert rows, and generate patchset - all in one to handle lifetime constraints.
    pub fn create_session_and_generate_patchset(conn: &Connection, row_count: i32) -> Vec<u8> {
        let mut session = Session::new(conn).unwrap();
        session.attach(Some("items")).unwrap();
        insert_rows(conn, row_count);
        // Use streaming API to get bytes
        let mut output = Vec::new();
        session.patchset_strm(&mut output).unwrap();
        output
    }

    /// Create session, insert rows, and generate changeset - all in one to handle lifetime constraints.
    pub fn create_session_and_generate_changeset(conn: &Connection, row_count: i32) -> Vec<u8> {
        let mut session = Session::new(conn).unwrap();
        session.attach(Some("items")).unwrap();
        insert_rows(conn, row_count);
        // Use streaming API to get bytes
        let mut output = Vec::new();
        session.changeset_strm(&mut output).unwrap();
        output
    }

    /// Apply changeset bytes using `apply_strm`.
    pub fn apply_changeset_to_conn(conn: &Connection, changeset_bytes: &[u8]) {
        conn.apply_strm(
            &mut &changeset_bytes[..],
            None::<fn(&str) -> bool>,
            |_: ConflictType, _| ConflictAction::SQLITE_CHANGESET_ABORT,
        )
        .unwrap();
    }

    pub fn is_empty_benchmark(conn: &Connection) -> bool {
        let session = Session::new(conn).unwrap();
        session.is_empty()
    }
}

// ============================================================================
// Comparison Benchmarks
// ============================================================================

fn bench_session_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_creation");

    group.bench_function("diesel-sqlite-session", |b| {
        b.iter(|| {
            let mut conn = diesel_impl::setup_connection();
            let session = diesel_impl::create_session(&mut conn);
            black_box(session);
        });
    });

    group.bench_function("rusqlite", |b| {
        b.iter(|| {
            let conn = rusqlite_impl::setup_connection();
            let session = rusqlite::session::Session::new(&conn).unwrap();
            black_box(session);
        });
    });

    group.finish();
}

fn bench_attach_table(c: &mut Criterion) {
    let mut group = c.benchmark_group("attach_table");

    group.bench_function("diesel-sqlite-session", |b| {
        b.iter(|| {
            let mut conn = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);
            black_box(session);
        });
    });

    group.bench_function("rusqlite", |b| {
        b.iter(|| {
            let conn = rusqlite_impl::setup_connection();
            let mut session = rusqlite::session::Session::new(&conn).unwrap();
            session.attach(Some("items")).unwrap();
            black_box(session);
        });
    });

    group.finish();
}

fn bench_patchset_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("patchset_generation");

    for row_count in &[10, 100, 500] {
        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));

        group.bench_with_input(
            BenchmarkId::new("diesel-sqlite-session", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = diesel_impl::setup_connection();
                    let mut session = diesel_impl::create_session(&mut conn);
                    diesel_impl::attach_table(&mut session);
                    diesel_impl::insert_rows(&mut conn, count);
                    let patchset = diesel_impl::generate_patchset(&mut session);
                    black_box(patchset);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rusqlite", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let conn = rusqlite_impl::setup_connection();
                    let patchset =
                        rusqlite_impl::create_session_and_generate_patchset(&conn, count);
                    black_box(patchset);
                });
            },
        );
    }
    group.finish();
}

fn bench_changeset_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("changeset_generation");

    for row_count in &[10, 100, 500] {
        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));

        group.bench_with_input(
            BenchmarkId::new("diesel-sqlite-session", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let mut conn = diesel_impl::setup_connection();
                    let mut session = diesel_impl::create_session(&mut conn);
                    diesel_impl::attach_table(&mut session);
                    diesel_impl::insert_rows(&mut conn, count);
                    let changeset = diesel_impl::generate_changeset(&mut session);
                    black_box(changeset);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rusqlite", row_count),
            row_count,
            |b, &count| {
                b.iter(|| {
                    let conn = rusqlite_impl::setup_connection();
                    let changeset =
                        rusqlite_impl::create_session_and_generate_changeset(&conn, count);
                    black_box(changeset);
                });
            },
        );
    }
    group.finish();
}

fn bench_apply_patchset(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_patchset");

    for row_count in &[10, 100, 500] {
        // Pre-generate patchsets
        let diesel_patchset = {
            let mut conn = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);
            diesel_impl::insert_rows(&mut conn, *row_count);
            diesel_impl::generate_patchset(&mut session)
        };

        let rusqlite_patchset = {
            let conn = rusqlite_impl::setup_connection();
            rusqlite_impl::create_session_and_generate_patchset(&conn, *row_count)
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));

        group.bench_with_input(
            BenchmarkId::new("diesel-sqlite-session", row_count),
            &diesel_patchset,
            |b, patchset| {
                b.iter(|| {
                    let mut conn = diesel_impl::setup_connection();
                    diesel_impl::apply_patchset_to_conn(&mut conn, patchset);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rusqlite", row_count),
            &rusqlite_patchset,
            |b, patchset| {
                b.iter(|| {
                    let conn = rusqlite_impl::setup_connection();
                    rusqlite_impl::apply_changeset_to_conn(&conn, patchset);
                });
            },
        );
    }
    group.finish();
}

fn bench_apply_changeset(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_changeset");

    for row_count in &[10, 100, 500] {
        // Pre-generate changesets
        let diesel_changeset = {
            let mut conn = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);
            diesel_impl::insert_rows(&mut conn, *row_count);
            diesel_impl::generate_changeset(&mut session)
        };

        let rusqlite_changeset = {
            let conn = rusqlite_impl::setup_connection();
            rusqlite_impl::create_session_and_generate_changeset(&conn, *row_count)
        };

        group.throughput(Throughput::Elements(u64::try_from(*row_count).unwrap()));

        group.bench_with_input(
            BenchmarkId::new("diesel-sqlite-session", row_count),
            &diesel_changeset,
            |b, changeset| {
                b.iter(|| {
                    let mut conn = diesel_impl::setup_connection();
                    diesel_impl::apply_changeset_to_conn(&mut conn, changeset);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rusqlite", row_count),
            &rusqlite_changeset,
            |b, changeset| {
                b.iter(|| {
                    let conn = rusqlite_impl::setup_connection();
                    rusqlite_impl::apply_changeset_to_conn(&conn, changeset);
                });
            },
        );
    }
    group.finish();
}

fn bench_is_empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_empty");

    group.bench_function("diesel-sqlite-session/empty", |b| {
        b.iter(|| {
            let mut conn = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);
            black_box(diesel_impl::is_empty(&session));
        });
    });

    group.bench_function("rusqlite/empty", |b| {
        b.iter(|| {
            let conn = rusqlite_impl::setup_connection();
            black_box(rusqlite_impl::is_empty_benchmark(&conn));
        });
    });

    group.bench_function("diesel-sqlite-session/with_changes", |b| {
        b.iter(|| {
            let mut conn = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);
            diesel_impl::insert_rows(&mut conn, 10);
            black_box(diesel_impl::is_empty(&session));
        });
    });

    group.bench_function("rusqlite/with_changes", |b| {
        b.iter(|| {
            let conn = rusqlite_impl::setup_connection();
            rusqlite_impl::insert_rows(&conn, 10);
            black_box(rusqlite_impl::is_empty_benchmark(&conn));
        });
    });

    group.finish();
}

fn bench_mixed_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_operations");

    group.bench_function("diesel-sqlite-session", |b| {
        b.iter(|| {
            let mut conn = diesel_impl::setup_connection();
            diesel_impl::insert_rows(&mut conn, 50);

            let mut session = diesel_impl::create_session(&mut conn);
            diesel_impl::attach_table(&mut session);

            // 25 inserts (ids 50-74)
            diesel_impl::insert_rows_range(&mut conn, 50, 75);

            // 25 updates
            diesel_impl::update_rows(&mut conn, 25);

            // 25 deletes
            diesel_impl::delete_rows(&mut conn, 25, 50);

            let patchset = diesel_impl::generate_patchset(&mut session);
            black_box(patchset);
        });
    });

    group.bench_function("rusqlite", |b| {
        b.iter(|| {
            let conn = rusqlite_impl::setup_connection();
            rusqlite_impl::insert_rows(&conn, 50);

            let mut session = rusqlite::session::Session::new(&conn).unwrap();
            session.attach(Some("items")).unwrap();

            // 25 inserts (ids 50-74)
            rusqlite_impl::insert_rows_range(&conn, 50, 75);

            // 25 updates
            rusqlite_impl::update_rows(&conn, 25);

            // 25 deletes
            rusqlite_impl::delete_rows(&conn, 25, 50);

            // Use streaming API to get bytes
            let mut patchset = Vec::new();
            session.patchset_strm(&mut patchset).unwrap();
            black_box(patchset);
        });
    });

    group.finish();
}

fn bench_full_replication_workflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_replication_workflow");
    group.throughput(Throughput::Elements(100));

    group.bench_function("diesel-sqlite-session", |b| {
        b.iter(|| {
            // Source
            let mut source = diesel_impl::setup_connection();
            let mut session = diesel_impl::create_session(&mut source);
            diesel_impl::attach_table(&mut session);
            diesel_impl::insert_rows(&mut source, 100);
            let patchset = diesel_impl::generate_patchset(&mut session);

            // Replica
            let mut replica = diesel_impl::setup_connection();
            diesel_impl::apply_patchset_to_conn(&mut replica, &patchset);

            black_box(replica)
        });
    });

    group.bench_function("rusqlite", |b| {
        b.iter(|| {
            // Source
            let source = rusqlite_impl::setup_connection();
            let patchset = rusqlite_impl::create_session_and_generate_patchset(&source, 100);

            // Replica
            let replica = rusqlite_impl::setup_connection();
            rusqlite_impl::apply_changeset_to_conn(&replica, &patchset);

            black_box(replica)
        });
    });

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
              bench_apply_changeset,
              bench_is_empty,
              bench_mixed_operations,
              bench_full_replication_workflow
}

criterion_main!(benches);
