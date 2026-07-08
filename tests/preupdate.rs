//! Integration tests pinning the pre-update hook behavior contract.
//!
//! Every test isolates one invariant of the wrapper on top of
//! `sqlite3_preupdate_hook`. Values pulled from `PreUpdateEvent` and
//! `PreUpdateValue` are copied into owned types inside the callback frame,
//! since both borrow from the transient `sqlite3_value` buffers `SQLite`
//! destroys when the callback returns.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    PreUpdateColumnType, PreUpdateError, PreUpdateEvent, PreUpdateOp, PreUpdateValue,
    SqliteSessionExt,
};
use parking_lot::Mutex;

/// A dynamically-typed column value snapshot copied out of a live
/// `PreUpdateValue` so that assertions can look at it after the callback
/// returns.
#[derive(Debug, Clone, PartialEq)]
enum CapturedValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
}

impl CapturedValue {
    fn capture(v: &PreUpdateValue<'_>) -> Self {
        match v.column_type() {
            PreUpdateColumnType::Integer => Self::Integer(v.as_i64()),
            PreUpdateColumnType::Float => Self::Float(v.as_f64()),
            PreUpdateColumnType::Text => Self::Text(v.as_text().unwrap_or_default().to_owned()),
            PreUpdateColumnType::Blob => Self::Blob(v.as_bytes().unwrap_or_default().to_vec()),
            PreUpdateColumnType::Null => Self::Null,
        }
    }
}

/// A whole pre-update callback captured by value.
///
/// Reading all columns eagerly here means the test assertions can be plain
/// `assert_eq!` against owned data. Values only fetched when the operation
/// allows them (INSERT has no `old`, DELETE has no `new`) end up as empty
/// `Vec`s in the other direction.
#[derive(Debug, Clone)]
struct CapturedEvent {
    op: PreUpdateOp,
    database: String,
    table: String,
    old_rowid: i64,
    new_rowid: i64,
    depth: u32,
    column_count: usize,
    blob_write_column: Option<u32>,
    old_values: Vec<CapturedValue>,
    new_values: Vec<CapturedValue>,
}

impl CapturedEvent {
    fn capture(event: &PreUpdateEvent<'_>) -> Self {
        let column_count = event.column_count();
        let op = event.op();

        let mut old_values = Vec::new();
        if !matches!(op, PreUpdateOp::Insert) {
            for i in 0..column_count {
                let idx = u32::try_from(i).expect("column index fits in u32");
                if let Ok(v) = event.old_value(idx) {
                    old_values.push(CapturedValue::capture(&v));
                }
            }
        }

        let mut new_values = Vec::new();
        if !matches!(op, PreUpdateOp::Delete) {
            for i in 0..column_count {
                let idx = u32::try_from(i).expect("column index fits in u32");
                if let Ok(v) = event.new_value(idx) {
                    new_values.push(CapturedValue::capture(&v));
                }
            }
        }

        Self {
            op,
            database: event.database().to_owned(),
            table: event.table().to_owned(),
            old_rowid: event.old_rowid(),
            new_rowid: event.new_rowid(),
            depth: event.depth(),
            column_count,
            blob_write_column: event.blob_write_column(),
            old_values,
            new_values,
        }
    }
}

/// Register a hook that captures every event via `CapturedEvent::capture`,
/// and hand back the shared log plus the RAII guard.
fn record_all(
    conn: &mut SqliteConnection,
) -> (
    Arc<Mutex<Vec<CapturedEvent>>>,
    diesel_sqlite_session::PreUpdateHook,
) {
    let log: Arc<Mutex<Vec<CapturedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let hook = conn.on_preupdate(move |event| {
        sink.lock().push(CapturedEvent::capture(&event));
    });
    (log, hook)
}

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

#[test]
fn insert_fires_hook_with_new_rowid_and_new_values_only() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);

    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Widget', 42)")
        .execute(&mut conn)
        .unwrap();

    // Also pin the negative shape of `old_value` on INSERT with a second hook
    // pass. We install a targeted hook that records the error variant.
    drop(hook);
    let old_error: Arc<Mutex<Option<PreUpdateError>>> = Arc::new(Mutex::new(None));
    let sink = old_error.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            let err = event.old_value(0).err();
            *sink.lock() = err;
        }
    });
    sql_query("INSERT INTO items (id, name, quantity) VALUES (2, 'Sprocket', 7)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert_eq!(events.len(), 1, "one event per row insert");
    let event = &events[0];
    assert_eq!(event.op, PreUpdateOp::Insert);
    assert_eq!(event.database, "main");
    assert_eq!(event.table, "items");
    assert_eq!(event.column_count, 3);
    assert_eq!(event.new_rowid, 1);
    assert_eq!(event.blob_write_column, None);
    assert_eq!(event.depth, 0);
    assert_eq!(
        event.new_values,
        vec![
            CapturedValue::Integer(1),
            CapturedValue::Text("Widget".to_string()),
            CapturedValue::Integer(42),
        ],
    );
    assert!(event.old_values.is_empty());

    let captured_err = old_error.lock().take();
    assert!(
        matches!(captured_err, Some(PreUpdateError::OldNotAvailableOnInsert)),
        "old_value on INSERT must report OldNotAvailableOnInsert, got {captured_err:?}",
    );
}

#[test]
fn update_fires_hook_with_old_and_new_values() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'pre_name', 10)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("UPDATE items SET name = 'post_name', quantity = 55 WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events: Vec<CapturedEvent> = log
        .lock()
        .iter()
        .filter(|e| matches!(e.op, PreUpdateOp::Update))
        .cloned()
        .collect();
    assert_eq!(events.len(), 1, "one Update event per row updated");
    let event = &events[0];
    assert_eq!(event.old_rowid, 1);
    assert_eq!(event.new_rowid, 1);
    assert_eq!(event.column_count, 3);
    assert_eq!(
        event.old_values,
        vec![
            CapturedValue::Integer(1),
            CapturedValue::Text("pre_name".to_string()),
            CapturedValue::Integer(10),
        ],
    );
    assert_eq!(
        event.new_values,
        vec![
            CapturedValue::Integer(1),
            CapturedValue::Text("post_name".to_string()),
            CapturedValue::Integer(55),
        ],
    );
}

#[test]
fn delete_fires_hook_with_old_values_only() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO items (id, name, quantity) VALUES (1, 'Doomed', 99)")
        .execute(&mut conn)
        .unwrap();

    let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let new_error: Arc<Mutex<Option<PreUpdateError>>> = Arc::new(Mutex::new(None));
    let events_sink = events.clone();
    let error_sink = new_error.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Delete) {
            // Capture the negative shape of `new_value` on DELETE inside the
            // callback frame, then record the event by value.
            let err = event.new_value(0).err();
            *error_sink.lock() = err;
        }
        events_sink.lock().push(CapturedEvent::capture(&event));
    });

    sql_query("DELETE FROM items WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let deletes: Vec<CapturedEvent> = events
        .lock()
        .iter()
        .filter(|e| matches!(e.op, PreUpdateOp::Delete))
        .cloned()
        .collect();
    assert_eq!(deletes.len(), 1, "one Delete event per row deleted");
    let event = &deletes[0];
    assert_eq!(event.op, PreUpdateOp::Delete);
    assert_eq!(event.database, "main");
    assert_eq!(event.table, "items");
    assert_eq!(event.old_rowid, 1);
    assert_eq!(event.column_count, 3);
    assert_eq!(
        event.old_values,
        vec![
            CapturedValue::Integer(1),
            CapturedValue::Text("Doomed".to_string()),
            CapturedValue::Integer(99),
        ],
    );
    assert!(event.new_values.is_empty());

    let captured = new_error.lock().take();
    assert!(
        matches!(captured, Some(PreUpdateError::NewNotAvailableOnDelete)),
        "new_value on DELETE must report NewNotAvailableOnDelete, got {captured:?}",
    );
}

#[test]
fn column_type_reports_actual_dynamic_type() {
    let mut conn = fresh_connection();
    // Columns declared with no type get BLOB affinity, meaning SQLite stores
    // the literal type verbatim rather than coercing to the declared type.
    sql_query("CREATE TABLE mixed (a, b, c, d, e)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO mixed (a, b, c, d, e) VALUES (7, 'hello', 3.5, x'DEADBEEF', NULL)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.column_count, 5);
    assert_eq!(event.new_values.len(), 5);

    assert_eq!(event.new_values[0], CapturedValue::Integer(7));
    assert_eq!(
        event.new_values[1],
        CapturedValue::Text("hello".to_string())
    );
    assert_eq!(event.new_values[2], CapturedValue::Float(3.5));
    assert_eq!(
        event.new_values[3],
        CapturedValue::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
    );
    assert_eq!(event.new_values[4], CapturedValue::Null);
}

#[test]
fn column_out_of_range_returns_column_out_of_range() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE tiny (a, b)")
        .execute(&mut conn)
        .unwrap();

    let captured: Arc<Mutex<Option<PreUpdateError>>> = Arc::new(Mutex::new(None));
    let sink = captured.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            *sink.lock() = event.new_value(9).err();
        }
    });
    sql_query("INSERT INTO tiny (a, b) VALUES (1, 2)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let err = captured.lock().take();
    match err {
        Some(PreUpdateError::ColumnOutOfRange { index, count }) => {
            assert_eq!(index, 9);
            assert_eq!(count, 2);
        }
        other => panic!("expected ColumnOutOfRange {{ index: 9, count: 2 }}, got {other:?}"),
    }
}

#[test]
fn dropping_the_guard_stops_the_callback() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE counted (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    let hook = conn.on_preupdate(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    sql_query("INSERT INTO counted (v) VALUES (1)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);

    drop(hook);

    for i in 2..=5 {
        sql_query(format!("INSERT INTO counted (v) VALUES ({i})"))
            .execute(&mut conn)
            .unwrap();
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "no more events must fire after the guard is dropped",
    );
}

#[test]
fn re_registering_replaces_the_previous_hook() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE slots (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let a_count = Arc::new(AtomicU32::new(0));
    let a_sink = a_count.clone();
    let _hook_a = conn.on_preupdate(move |_| {
        a_sink.fetch_add(1, Ordering::SeqCst);
    });
    sql_query("INSERT INTO slots (v) VALUES (1)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(a_count.load(Ordering::SeqCst), 1);

    let b_count = Arc::new(AtomicU32::new(0));
    let b_sink = b_count.clone();
    let hook_b = conn.on_preupdate(move |_| {
        b_sink.fetch_add(1, Ordering::SeqCst);
    });
    // Registering hook_b silently retires hook_a's closure. The stale
    // guard hook_a is now a no-op wrapper. `hook_a` stays in scope so it
    // drops AFTER hook_b (LIFO drop order) and its Drop finds nothing to do.

    sql_query("INSERT INTO slots (v) VALUES (2)")
        .execute(&mut conn)
        .unwrap();

    assert_eq!(
        a_count.load(Ordering::SeqCst),
        1,
        "A frozen at 1 after re-registration"
    );
    assert_eq!(
        b_count.load(Ordering::SeqCst),
        1,
        "B observed the second INSERT"
    );

    drop(hook_b);
}

#[test]
fn hook_panic_is_caught_and_dml_still_succeeds() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE brave (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let hook = conn.on_preupdate(|_| {
        panic!("boom inside pre-update callback");
    });

    let result = sql_query("INSERT INTO brave (v) VALUES (1)").execute(&mut conn);
    assert!(
        result.is_ok(),
        "INSERT must succeed even though the hook panicked, got {result:?}",
    );

    drop(hook);

    let count: i64 = diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM brave")
        .get_result(&mut conn)
        .unwrap();
    assert_eq!(
        count, 1,
        "the row landed in the table after the panic was swallowed"
    );
}

#[test]
fn trigger_nested_dml_reports_positive_depth() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE parent (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE child (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query(
        "CREATE TRIGGER cascade AFTER INSERT ON parent FOR EACH ROW \
         BEGIN INSERT INTO child (v) VALUES (NEW.v * 2); END",
    )
    .execute(&mut conn)
    .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO parent (v) VALUES (5)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    let parent_event = events
        .iter()
        .find(|e| e.table == "parent")
        .expect("saw an event on parent");
    let child_event = events
        .iter()
        .find(|e| e.table == "child")
        .expect("saw an event on child from the trigger");

    assert_eq!(parent_event.depth, 0, "top-level INSERT reports depth 0");
    assert!(
        child_event.depth >= 1,
        "trigger-driven INSERT reports depth >= 1, got {}",
        child_event.depth,
    );
}

#[test]
fn blob_write_column_reports_none_for_regular_dml() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE observed (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO observed (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO observed (id, v) VALUES (2, 20)")
        .execute(&mut conn)
        .unwrap();
    sql_query("UPDATE observed SET v = 21 WHERE id = 2")
        .execute(&mut conn)
        .unwrap();
    sql_query("DELETE FROM observed WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert!(events.len() >= 3, "at least one event per DML statement");
    for event in &events {
        assert_eq!(
            event.blob_write_column, None,
            "regular DML must report blob_write_column == None, got {:?} for op {:?}",
            event.blob_write_column, event.op,
        );
    }
}

#[test]
fn multi_row_insert_fires_hook_per_row() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE bulk (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO bulk (v) VALUES (1), (2), (3)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert_eq!(events.len(), 3, "one event per row in a multi-row INSERT");
    let rowids: Vec<i64> = events.iter().map(|e| e.new_rowid).collect();
    assert_eq!(rowids, vec![1, 2, 3]);
    for event in &events {
        assert_eq!(event.op, PreUpdateOp::Insert);
    }
}

#[test]
fn empty_text_and_blob_values_survive_the_round_trip() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE emp (t, b)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO emp (t, b) VALUES ('', x'')")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].new_values[0], CapturedValue::Text(String::new()));
    assert_eq!(events[0].new_values[1], CapturedValue::Blob(Vec::new()));
}

#[test]
fn null_value_reports_is_null_and_null_type() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE nulls (a, b)")
        .execute(&mut conn)
        .unwrap();

    // Capture is_null and column_type inside the callback frame, since those
    // methods borrow from the transient PreUpdateValue.
    let recorded: Arc<Mutex<Vec<(bool, PreUpdateColumnType)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = recorded.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            let mut buf = sink.lock();
            for i in 0..u32::try_from(event.column_count()).unwrap() {
                let v = event.new_value(i).unwrap();
                buf.push((v.is_null(), v.column_type()));
            }
        }
    });
    sql_query("INSERT INTO nulls (a, b) VALUES (NULL, 7)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let observed = recorded.lock().clone();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0], (true, PreUpdateColumnType::Null));
    assert_eq!(observed[1], (false, PreUpdateColumnType::Integer));
}

#[test]
fn preupdate_op_from_raw_roundtrips_known_codes() {
    for op in [
        PreUpdateOp::Insert,
        PreUpdateOp::Update,
        PreUpdateOp::Delete,
    ] {
        assert_eq!(PreUpdateOp::from_raw(op.to_raw()), Some(op));
    }
    assert_eq!(PreUpdateOp::from_raw(0), None);
    assert_eq!(PreUpdateOp::from_raw(-1), None);
    assert_eq!(PreUpdateOp::from_raw(999), None);
}

#[test]
fn session_after_dropping_preupdate_hook_still_records_changes() {
    // Pin the documented "one at a time" cutover: install a preupdate hook,
    // drop the guard, then create a Session on the same connection. Session
    // must be able to install its own pre-update callback and record changes.
    use diesel_sqlite_session::{ConflictAction, SqliteSessionExt as _};

    let mut conn = fresh_connection();
    sql_query("CREATE TABLE cutover (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    // Phase 1: use PreUpdateHook, then drop the guard.
    let count = Arc::new(AtomicU32::new(0));
    let sink = count.clone();
    let hook = conn.on_preupdate(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    sql_query("INSERT INTO cutover (v) VALUES (1)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(hook);

    // Phase 2: session takes over the slot cleanly.
    let mut session = conn.create_session().unwrap();
    session.attach_by_name("cutover").unwrap();
    sql_query("INSERT INTO cutover (v) VALUES (2)")
        .execute(&mut conn)
        .unwrap();
    let changeset = session.changeset().unwrap();
    drop(session);
    assert!(!changeset.is_empty(), "session recorded the phase-2 insert");

    // Round-trip the changeset onto a replica to be sure.
    let mut replica = fresh_connection();
    sql_query("CREATE TABLE cutover (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut replica)
        .unwrap();
    replica
        .apply_changeset(&changeset, |_| ConflictAction::Abort)
        .unwrap();
    let v: i64 =
        diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT v FROM cutover WHERE id = 2")
            .get_result(&mut replica)
            .unwrap();
    assert_eq!(v, 2);
}

#[test]
fn nested_triggers_report_increasing_depth() {
    // Trigger chain: an INSERT into `top` fires an INSERT into `mid`, which
    // in turn fires an INSERT into `leaf`. SQLite's `sqlite3_preupdate_depth`
    // must report 0 for the top-level statement, 1 for the row cascaded by
    // the first trigger, and 2 for the row cascaded by the second.
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE top (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE mid (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE leaf (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();
    sql_query(
        "CREATE TRIGGER top_to_mid AFTER INSERT ON top FOR EACH ROW \
         BEGIN INSERT INTO mid (v) VALUES (NEW.v + 1); END",
    )
    .execute(&mut conn)
    .unwrap();
    sql_query(
        "CREATE TRIGGER mid_to_leaf AFTER INSERT ON mid FOR EACH ROW \
         BEGIN INSERT INTO leaf (v) VALUES (NEW.v + 1); END",
    )
    .execute(&mut conn)
    .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO top (v) VALUES (10)")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    let by_table = |name: &str| -> u32 {
        events
            .iter()
            .find(|e| e.table == name)
            .unwrap_or_else(|| panic!("saw an event on {name}: got {events:?}"))
            .depth
    };
    assert_eq!(by_table("top"), 0, "top-level DML reports depth 0");
    assert_eq!(by_table("mid"), 1, "first trigger nesting reports depth 1");
    assert_eq!(
        by_table("leaf"),
        2,
        "second trigger nesting reports depth 2"
    );
}

#[test]
fn attached_database_reports_its_alias_name() {
    // Attaching an in-memory database as `aux` and driving DML through the
    // qualified table name pins that `database()` returns the ATTACH alias
    // rather than a fixed `"main"`.
    let mut conn = fresh_connection();
    sql_query("ATTACH DATABASE ':memory:' AS aux")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE aux.notes (id INTEGER PRIMARY KEY, txt TEXT)")
        .execute(&mut conn)
        .unwrap();

    let (log, hook) = record_all(&mut conn);
    sql_query("INSERT INTO aux.notes (id, txt) VALUES (1, 'attached')")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let events = log.lock().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].database, "aux");
    assert_eq!(events[0].table, "notes");
}

#[test]
fn panicking_hook_lets_insert_update_and_delete_all_succeed() {
    // The FFI trampoline's `catch_unwind` is shared across ops. Assert that
    // each of the three op paths individually survives a panic in the hook.
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE brave3 (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(&mut conn)
        .unwrap();

    let hook = conn.on_preupdate(|_| {
        panic!("boom");
    });

    assert!(sql_query("INSERT INTO brave3 (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .is_ok());
    assert!(sql_query("UPDATE brave3 SET v = 20 WHERE id = 1")
        .execute(&mut conn)
        .is_ok());
    assert!(sql_query("DELETE FROM brave3 WHERE id = 1")
        .execute(&mut conn)
        .is_ok());

    drop(hook);

    let n: i64 = diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM brave3")
        .get_result(&mut conn)
        .unwrap();
    assert_eq!(n, 0, "delete landed after the panic was swallowed");
}

#[test]
fn preupdate_value_coerces_across_adjacent_types() {
    // SQLite's `sqlite3_value_int64` and `sqlite3_value_double` coerce across
    // storage classes on read. Pin the behavior we expose so a change in the
    // wrapper (e.g. gating readers on `column_type`) would fail the test.
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE coerce (a, b, c, d)")
        .execute(&mut conn)
        .unwrap();

    let captured: Arc<Mutex<Vec<(i64, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            let mut buf = sink.lock();
            for i in 0..u32::try_from(event.column_count()).unwrap() {
                let v = event.new_value(i).unwrap();
                buf.push((v.as_i64(), v.as_f64()));
            }
        }
    });
    sql_query("INSERT INTO coerce (a, b, c, d) VALUES (42, 3.75, '17', '2.5')")
        .execute(&mut conn)
        .unwrap();
    drop(hook);

    let observed = captured.lock().clone();
    assert_eq!(observed.len(), 4);
    // Integer 42 read as f64 is 42.0.
    assert_eq!(observed[0].0, 42);
    assert!((observed[0].1 - 42.0).abs() < f64::EPSILON);
    // Float 3.75 read as i64 truncates to 3.
    assert_eq!(observed[1].0, 3);
    assert!((observed[1].1 - 3.75).abs() < f64::EPSILON);
    // Text '17' coerces to integer 17 and float 17.0.
    assert_eq!(observed[2].0, 17);
    assert!((observed[2].1 - 17.0).abs() < f64::EPSILON);
    // Text '2.5' coerces to integer 2 (SQLite truncates) and float 2.5.
    assert_eq!(observed[3].0, 2);
    assert!((observed[3].1 - 2.5).abs() < f64::EPSILON);
}
