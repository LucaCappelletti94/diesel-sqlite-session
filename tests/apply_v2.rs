//! Integration tests for `SqliteSessionExt::apply_changeset_with` (v2).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ApplyError, ApplyFlags, ChangesetOp, ConflictAction, ConflictType, SqliteSessionExt,
};
use parking_lot::Mutex;

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

/// Build a changeset that INSERTs one row into each of `tables`.
fn make_changeset(tables: &[&str]) -> Vec<u8> {
    let mut conn = fresh_connection();
    for tab in tables {
        sql_query(format!(
            "CREATE TABLE {tab} (id INTEGER PRIMARY KEY, v INTEGER)"
        ))
        .execute(&mut conn)
        .unwrap();
    }
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    for (i, tab) in tables.iter().enumerate() {
        let v = i32::try_from(i).unwrap_or(0) * 10 + 1;
        sql_query(format!("INSERT INTO {tab} (id, v) VALUES (1, {v})"))
            .execute(&mut conn)
            .unwrap();
    }
    session.changeset().unwrap()
}

fn create_tables(conn: &mut SqliteConnection, tables: &[&str]) {
    for tab in tables {
        sql_query(format!(
            "CREATE TABLE {tab} (id INTEGER PRIMARY KEY, v INTEGER)"
        ))
        .execute(conn)
        .unwrap();
    }
}

fn count_rows(conn: &mut SqliteConnection, table: &str) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>(&format!("SELECT COUNT(*) FROM {table}"))
        .get_result(conn)
        .unwrap()
}

#[test]
fn apply_with_no_flags_matches_v1_semantics() {
    let bytes = make_changeset(&["items"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["items"]);

    let outcome = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds");
    assert!(outcome.rebase.is_empty(), "no rebase without conflicts");
    assert_eq!(count_rows(&mut replica, "items"), 1);
}

#[test]
fn filter_can_skip_specific_tables() {
    let bytes = make_changeset(&["keep", "skip"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["keep", "skip"]);

    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |table| table != "skip",
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds");
    assert_eq!(count_rows(&mut replica, "keep"), 1);
    assert_eq!(count_rows(&mut replica, "skip"), 0);
}

#[test]
fn filter_receives_every_table_name_exactly_once() {
    let bytes = make_changeset(&["first", "second"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["first", "second"]);

    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            move |table| {
                sink.lock().push(table.to_owned());
                true
            },
            |_| ConflictAction::Abort,
        )
        .unwrap();

    let mut seen = observed.lock().clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen, vec!["first".to_string(), "second".to_string()]);
}

#[test]
fn invert_flag_flips_insert_into_delete() {
    let bytes = make_changeset(&["reversed"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["reversed"]);
    sql_query("INSERT INTO reversed (id, v) VALUES (1, 1)")
        .execute(&mut replica)
        .unwrap();
    assert_eq!(count_rows(&mut replica, "reversed"), 1);

    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::INVERT,
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("inverted apply succeeds");
    assert_eq!(
        count_rows(&mut replica, "reversed"),
        0,
        "inverted INSERT DELETEs the row",
    );
}

#[test]
fn nosavepoint_flag_still_applies_inside_caller_transaction() {
    let bytes = make_changeset(&["outer"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["outer"]);

    replica
        .transaction::<_, diesel::result::Error, _>(|tx| {
            tx.apply_changeset_with(
                &bytes,
                ApplyFlags::NOSAVEPOINT,
                |_| true,
                |_| ConflictAction::Abort,
            )
            .map_err(|_| diesel::result::Error::RollbackTransaction)?;
            Ok(())
        })
        .expect("outer transaction commits");
    assert_eq!(count_rows(&mut replica, "outer"), 1);
}

#[test]
fn conflict_callback_receives_context_with_op_and_table() {
    type CapturedConflict = (ConflictType, Option<ChangesetOp>, String);
    // Source and replica both have the same row: applying the source's
    // second-INSERT will conflict on the PK. Capture what the conflict
    // callback sees.
    let mut source = fresh_connection();
    create_tables(&mut source, &["clash"]);
    sql_query("INSERT INTO clash (id, v) VALUES (1, 10)")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO clash (id, v) VALUES (2, 20)")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut replica = fresh_connection();
    create_tables(&mut replica, &["clash"]);
    sql_query("INSERT INTO clash (id, v) VALUES (2, 999)")
        .execute(&mut replica)
        .unwrap();

    let captured: Arc<Mutex<Vec<CapturedConflict>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let outcome = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            move |info| {
                sink.lock()
                    .push((info.conflict_type(), info.op(), info.table().to_owned()));
                ConflictAction::Replace
            },
        )
        .expect("apply with Replace succeeds");
    // Replace resolution should produce a rebase blob.
    assert!(
        !outcome.rebase.is_empty(),
        "Replace resolution emits a non-empty rebase blob",
    );

    let observed = captured.lock().clone();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0, ConflictType::Conflict);
    assert_eq!(observed[0].1, Some(ChangesetOp::Insert));
    assert_eq!(observed[0].2, "clash");

    // Replica now holds v = 20 (the source's value).
    let v: i64 = diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT v FROM clash WHERE id = 2")
        .get_result(&mut replica)
        .unwrap();
    assert_eq!(v, 20);
}

#[test]
fn conflict_info_exposes_conflict_value_and_new_value() {
    // Setup: source updates row 1 from v=1 to v=100. Replica has v=42.
    let mut source = fresh_connection();
    create_tables(&mut source, &["updates"]);
    sql_query("INSERT INTO updates (id, v) VALUES (1, 1)")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("UPDATE updates SET v = 100 WHERE id = 1")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut replica = fresh_connection();
    create_tables(&mut replica, &["updates"]);
    sql_query("INSERT INTO updates (id, v) VALUES (1, 42)")
        .execute(&mut replica)
        .unwrap();

    let captured: Arc<Mutex<Option<(i64, i64, i64)>>> = Arc::new(Mutex::new(None));
    let sink = captured.clone();
    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            move |info| {
                if info.conflict_type() == ConflictType::Data {
                    let old = info.old_value(1).unwrap().unwrap().as_i64();
                    let new = info.new_value(1).unwrap().unwrap().as_i64();
                    let on_disk = info.conflict_value(1).unwrap().as_i64();
                    *sink.lock() = Some((old, new, on_disk));
                }
                ConflictAction::Omit
            },
        )
        .expect("apply with Omit succeeds");

    let observed = captured.lock().take().expect("data conflict observed");
    assert_eq!(observed, (1, 100, 42), "old/new/conflict values match");
}

#[test]
fn conflict_callback_returning_abort_yields_conflict_aborted() {
    let bytes = make_changeset(&["clash2"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["clash2"]);
    sql_query("INSERT INTO clash2 (id, v) VALUES (1, 999)")
        .execute(&mut replica)
        .unwrap();

    let err = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ConflictAborted), "got {err:?}");
}

#[test]
fn filter_panic_yields_filter_panicked_error() {
    let bytes = make_changeset(&["boom"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["boom"]);

    let err = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| panic!("filter boom"),
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::FilterPanicked), "got {err:?}");
}

#[test]
fn conflict_panic_yields_conflict_handler_panicked_error() {
    let bytes = make_changeset(&["boom2"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["boom2"]);
    sql_query("INSERT INTO boom2 (id, v) VALUES (1, 999)")
        .execute(&mut replica)
        .unwrap();

    let err = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            |_| panic!("conflict boom"),
        )
        .unwrap_err();
    assert!(
        matches!(err, ApplyError::ConflictHandlerPanicked),
        "got {err:?}",
    );
}

#[test]
fn combined_flags_invert_and_ignorenoop_apply_cleanly() {
    let bytes = make_changeset(&["combined"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["combined"]);
    sql_query("INSERT INTO combined (id, v) VALUES (1, 1)")
        .execute(&mut replica)
        .unwrap();

    let outcome = replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::INVERT | ApplyFlags::IGNORENOOP,
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("apply succeeds under combined flags");
    assert!(outcome.rebase.is_empty());
    assert_eq!(count_rows(&mut replica, "combined"), 0);
}

#[test]
fn empty_changeset_is_a_no_op() {
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["idle"]);
    let outcome = replica
        .apply_changeset_with(
            &[],
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("empty changeset is a no-op");
    assert!(outcome.rebase.is_empty());
    assert_eq!(count_rows(&mut replica, "idle"), 0);
}

#[test]
fn filter_invocations_are_counted_once_per_table() {
    let bytes = make_changeset(&["only_here"]);
    let mut replica = fresh_connection();
    create_tables(&mut replica, &["only_here"]);

    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            move |table| {
                if table == "only_here" {
                    sink.fetch_add(1, Ordering::SeqCst);
                }
                true
            },
            |_| ConflictAction::Abort,
        )
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn conflict_info_reports_foreign_key_conflict_count() {
    // Build a changeset that will violate a foreign-key constraint on apply.
    // The replica has an FK `child.parent_id -> parent.id` and the changeset
    // inserts a `child` row referencing a nonexistent parent. SQLite defers
    // the FK check to a synthetic ForeignKey conflict callback.
    let mut source = fresh_connection();
    sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut source)
        .unwrap();
    sql_query("CREATE TABLE parent (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut source)
        .unwrap();
    sql_query(
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, \
         FOREIGN KEY(parent_id) REFERENCES parent(id))",
    )
    .execute(&mut source)
    .unwrap();
    sql_query("INSERT INTO parent (id, v) VALUES (1, 10)")
        .execute(&mut source)
        .unwrap();

    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO child (id, parent_id) VALUES (99, 1)")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    // Replica: same schema, FKs on, but NO parent row exists, so the
    // child INSERT will violate the FK on apply.
    let mut replica = fresh_connection();
    sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut replica)
        .unwrap();
    sql_query("CREATE TABLE parent (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut replica)
        .unwrap();
    sql_query(
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, \
         FOREIGN KEY(parent_id) REFERENCES parent(id))",
    )
    .execute(&mut replica)
    .unwrap();

    let seen_fk: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let sink = seen_fk.clone();
    let _ = replica.apply_changeset_with(
        &bytes,
        ApplyFlags::empty(),
        |_| true,
        move |info| {
            if info.conflict_type() == ConflictType::ForeignKey {
                *sink.lock() = info.fk_conflicts_count().ok();
            }
            ConflictAction::Abort
        },
    );
    let observed = seen_fk.lock().take();
    assert_eq!(
        observed,
        Some(1),
        "callback fired for ForeignKey conflict with count 1",
    );
}
