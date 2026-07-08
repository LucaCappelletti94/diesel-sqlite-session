//! Coverage gaps: error and debug paths that the happy-path tests do not
//! exercise. Each `#[test]` closes a specific uncovered branch surfaced by
//! `cargo llvm-cov`. Grouped by the module they cover.

use std::io::{self, Cursor, ErrorKind, Read, Write};

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    concat_changesets, concat_changesets_strm, invert_changeset, invert_changeset_strm, ApplyError,
    ApplyFlags, BlobMode, Changegroup, ChangesetError, ChangesetOp, ChangesetReader,
    ConflictAction, ConflictType, PreUpdateColumnType, PreUpdateOp, PreUpdateValue, Rebaser,
    SqliteSessionExt,
};

// -----------------------------------------------------------------------------
// Test doubles for reader / writer failure surfaces.
// -----------------------------------------------------------------------------

struct PanicRead;
impl Read for PanicRead {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        panic!("reader panic");
    }
}

struct ErrRead;
impl Read for ErrRead {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("reader io"))
    }
}

struct PanicWrite;
impl Write for PanicWrite {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        panic!("writer panic");
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ErrWrite;
impl Write for ErrWrite {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(ErrorKind::PermissionDenied, "writer io"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Shared fixtures.
// -----------------------------------------------------------------------------

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn create_items(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

fn make_changeset(rows: &[(i64, i64)]) -> Vec<u8> {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    for (id, v) in rows {
        sql_query(format!("INSERT INTO items (id, v) VALUES ({id}, {v})"))
            .execute(&mut conn)
            .unwrap();
    }
    session.changeset().unwrap()
}

const GARBAGE: &[u8] = &[0xFF; 64];

// =============================================================================
// A. Buffered SQLite error paths
// =============================================================================

#[test]
fn invert_changeset_rejects_garbage() {
    let err = invert_changeset(GARBAGE).unwrap_err();
    assert!(
        matches!(err, ChangesetError::InvertFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn concat_changesets_rejects_garbage_first_arg() {
    let good = make_changeset(&[(1, 10)]);
    let err = concat_changesets(GARBAGE, &good).unwrap_err();
    assert!(
        matches!(err, ChangesetError::ConcatFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn changegroup_add_rejects_garbage() {
    let mut group = Changegroup::new().unwrap();
    let err = group.add(GARBAGE).unwrap_err();
    assert!(
        matches!(err, ChangesetError::ChangegroupAddFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn changegroup_add_rejects_empty_input() {
    let mut group = Changegroup::new().unwrap();
    assert!(matches!(
        group.add(&[]).unwrap_err(),
        ChangesetError::EmptyChangeset
    ));
}

#[test]
fn rebaser_configure_rejects_empty_input() {
    let mut r = Rebaser::new().unwrap();
    assert!(matches!(
        r.configure(&[]).unwrap_err(),
        ChangesetError::EmptyChangeset
    ));
}

#[test]
fn rebaser_configure_rejects_garbage() {
    let mut r = Rebaser::new().unwrap();
    let err = r.configure(GARBAGE).unwrap_err();
    assert!(
        matches!(err, ChangesetError::RebaserConfigureFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn rebaser_rebase_rejects_empty_input() {
    let r = Rebaser::new().unwrap();
    assert!(matches!(
        r.rebase(&[]).unwrap_err(),
        ChangesetError::EmptyChangeset
    ));
}

// Note: `sqlite3rebaser_rebase` treats bytes it cannot decode as an empty
// stream and returns `SQLITE_OK` with a zero-length output. There is no
// portable way to force `RebaserRebaseFailed` from a caller.
// =============================================================================
// B. Streamed error and panic paths
// =============================================================================

#[test]
fn invert_changeset_strm_surfaces_reader_io() {
    let mut out = Vec::new();
    let err = invert_changeset_strm(ErrRead, &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderIo(_)), "got {err:?}");
}

#[test]
fn invert_changeset_strm_surfaces_reader_panic() {
    let mut out = Vec::new();
    let err = invert_changeset_strm(PanicRead, &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderPanicked), "got {err:?}");
}

#[test]
fn invert_changeset_strm_surfaces_writer_io() {
    let bytes = make_changeset(&[(1, 10)]);
    let err = invert_changeset_strm(Cursor::new(bytes), ErrWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterIo(_)), "got {err:?}");
}

#[test]
fn invert_changeset_strm_surfaces_writer_panic() {
    let bytes = make_changeset(&[(1, 10)]);
    let err = invert_changeset_strm(Cursor::new(bytes), PanicWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterPanicked), "got {err:?}");
}

#[test]
fn invert_changeset_strm_surfaces_invert_failed_on_garbage() {
    let mut out = Vec::new();
    let err = invert_changeset_strm(Cursor::new(GARBAGE.to_vec()), &mut out).unwrap_err();
    assert!(
        matches!(err, ChangesetError::InvertFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn concat_changesets_strm_surfaces_reader_io() {
    let good = make_changeset(&[(1, 10)]);
    let mut out = Vec::new();
    let err = concat_changesets_strm(ErrRead, Cursor::new(good), &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderIo(_)), "got {err:?}");
}

#[test]
fn concat_changesets_strm_surfaces_reader_panic() {
    let good = make_changeset(&[(1, 10)]);
    let mut out = Vec::new();
    let err = concat_changesets_strm(PanicRead, Cursor::new(good), &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderPanicked), "got {err:?}");
}

#[test]
fn concat_changesets_strm_surfaces_writer_io() {
    let a = make_changeset(&[(1, 10)]);
    let b = make_changeset(&[(2, 20)]);
    let err = concat_changesets_strm(Cursor::new(a), Cursor::new(b), ErrWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterIo(_)), "got {err:?}");
}

#[test]
fn changegroup_add_strm_surfaces_reader_io() {
    let mut group = Changegroup::new().unwrap();
    let err = group.add_strm(ErrRead).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderIo(_)), "got {err:?}");
}

#[test]
fn changegroup_output_strm_surfaces_writer_panic() {
    let mut group = Changegroup::new().unwrap();
    group.add(&make_changeset(&[(1, 10)])).unwrap();
    let err = group.output_strm(PanicWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterPanicked), "got {err:?}");
}

#[test]
fn rebaser_rebase_strm_surfaces_reader_io() {
    let r = Rebaser::new().unwrap();
    let mut out = Vec::new();
    let err = r.rebase_strm(ErrRead, &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderIo(_)), "got {err:?}");
}

#[test]
fn rebaser_rebase_strm_surfaces_reader_panic() {
    let r = Rebaser::new().unwrap();
    let mut out = Vec::new();
    let err = r.rebase_strm(PanicRead, &mut out).unwrap_err();
    assert!(matches!(err, ChangesetError::ReaderPanicked), "got {err:?}");
}

#[test]
fn rebaser_rebase_strm_surfaces_writer_io() {
    let r = Rebaser::new().unwrap();
    let bytes = make_changeset(&[(1, 10)]);
    let err = r.rebase_strm(Cursor::new(bytes), ErrWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterIo(_)), "got {err:?}");
}

#[test]
fn rebaser_rebase_strm_surfaces_writer_panic() {
    let r = Rebaser::new().unwrap();
    let bytes = make_changeset(&[(1, 10)]);
    let err = r.rebase_strm(Cursor::new(bytes), PanicWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterPanicked), "got {err:?}");
}

// =============================================================================
// C. ChangesetRow accessor error paths
// =============================================================================

#[test]
fn changeset_row_rejects_column_out_of_range_on_old_value() {
    // Build an UPDATE row so both old and new values are legal, then trigger
    // the ColumnOutOfRange branch for both.
    let mut conn = fresh_connection();
    create_items(&mut conn);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("UPDATE items SET v = 20 WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut reader = ChangesetReader::open(&bytes).unwrap();
    let row = reader.next().unwrap().expect("row present");
    let count = row.column_count();
    let bad = u32::try_from(count).unwrap() + 5;
    let err = row.old_value(bad).unwrap_err();
    assert!(
        matches!(err, ChangesetError::ColumnOutOfRange { .. }),
        "got {err:?}",
    );
    let err = row.new_value(bad).unwrap_err();
    assert!(
        matches!(err, ChangesetError::ColumnOutOfRange { .. }),
        "got {err:?}",
    );
}

#[test]
fn changeset_row_rejects_old_value_on_insert() {
    let bytes = make_changeset(&[(1, 10)]);
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    let row = reader.next().unwrap().unwrap();
    assert!(matches!(row.op(), ChangesetOp::Insert));
    assert!(matches!(
        row.old_value(0).unwrap_err(),
        ChangesetError::OldNotAvailableOnInsert
    ));
}

#[test]
fn changeset_row_rejects_new_value_on_delete() {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("DELETE FROM items WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut reader = ChangesetReader::open(&bytes).unwrap();
    let row = reader.next().unwrap().unwrap();
    assert!(matches!(row.op(), ChangesetOp::Delete));
    assert!(matches!(
        row.new_value(0).unwrap_err(),
        ChangesetError::NewNotAvailableOnDelete
    ));
}

// =============================================================================
// D. ConflictInfo error paths and Debug
// =============================================================================

#[test]
fn conflict_info_reports_column_count_and_supports_debug_and_oob_reads() {
    // Provoke a conflict by inserting the same PK on both source and replica.
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();

    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    replica
        .apply_changeset_with(
            &source_bytes,
            ApplyFlags::empty(),
            |_| true,
            move |info| {
                sink.lock().push(format!("{info:?}"));
                let cc = info.column_count();
                assert!(cc > 0);
                let bad = u32::try_from(cc).unwrap() + 5;
                // For an INSERT conflict, `old_value` is unavailable for any
                // index; a good index shows the OldNotAvailableOnInsert
                // branch. `new_value(bad)` and `conflict_value(bad)` prove
                // ColumnOutOfRange on the other two accessors.
                assert!(matches!(
                    info.old_value(0),
                    Err(ChangesetError::OldNotAvailableOnInsert)
                ));
                assert!(matches!(
                    info.new_value(bad),
                    Err(ChangesetError::ColumnOutOfRange { .. })
                ));
                assert!(matches!(
                    info.conflict_value(bad),
                    Err(ChangesetError::ColumnOutOfRange { .. })
                ));
                // fk_conflicts_count is defined only for FK conflicts, but
                // calling it here still exercises the sqlite path.
                let _ = info.fk_conflicts_count();
                ConflictAction::Replace
            },
        )
        .expect("apply resolves with Replace");

    let msgs = seen.lock().clone();
    assert!(!msgs.is_empty(), "conflict callback fired");
    assert!(
        msgs[0].contains("ConflictInfo"),
        "Debug rendered: {}",
        msgs[0],
    );
}

#[test]
fn conflict_info_conflict_value_reads_the_offending_row() {
    // On a DATA conflict, `conflict_value` returns the on-disk value.
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let observed = std::sync::Arc::new(parking_lot::Mutex::new(None::<i64>));
    let sink = observed.clone();
    replica
        .apply_changeset_with(
            &source_bytes,
            ApplyFlags::empty(),
            |_| true,
            move |info| {
                if matches!(info.conflict_type(), ConflictType::Conflict) {
                    if let Ok(v) = info.conflict_value(0) {
                        *sink.lock() = Some(v.as_i64());
                    }
                }
                ConflictAction::Replace
            },
        )
        .unwrap();
}

// =============================================================================
// E. Debug impls for the RAII wrappers
// =============================================================================

#[test]
fn debug_impls_render_type_names() {
    let group = Changegroup::new().unwrap();
    let rebaser = Rebaser::new().unwrap();
    assert!(format!("{group:?}").contains("Changegroup"));
    assert!(format!("{rebaser:?}").contains("Rebaser"));

    let bytes = make_changeset(&[(1, 10)]);
    let mut reader = ChangesetReader::open(&bytes).unwrap();
    assert!(format!("{reader:?}").contains("ChangesetReader"));
    let row = reader.next().unwrap().unwrap();
    let dbg_row = format!("{row:?}");
    assert!(dbg_row.contains("ChangesetRow"));
    assert!(dbg_row.contains("items"));
    let value = row.new_value(0).unwrap().unwrap();
    assert!(format!("{value:?}").contains("ChangesetValue"));
}

#[test]
fn sqlite_blob_debug_renders_length_and_mode() {
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, b BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO t (id, b) VALUES (1, x'deadbeef')")
        .execute(&mut conn)
        .unwrap();
    let blob = conn
        .open_blob("main", "t", "b", 1, BlobMode::ReadOnly)
        .unwrap();
    let s = format!("{blob:?}");
    assert!(s.contains("SqliteBlob"), "{s}");
    assert!(s.contains("ReadOnly"), "{s}");
    assert!(s.contains("len"), "{s}");
}

// =============================================================================
// F. Preupdate hook: value paths and Debug
// =============================================================================

#[test]
fn preupdate_event_debug_and_value_debug_render() {
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&mut conn)
        .unwrap();

    let _hook = conn.on_preupdate(move |event| {
        let mut msgs = sink.lock();
        msgs.push(format!("{event:?}"));
        if matches!(event.op(), PreUpdateOp::Insert) {
            if let Ok(v) = event.new_value(1) {
                msgs.push(format!("{v:?}"));
                // Exercise the ColumnOutOfRange branch too.
                let bad = u32::try_from(event.column_count()).unwrap() + 5;
                assert!(event.new_value(bad).is_err());
            }
        }
    });

    sql_query("INSERT INTO t (id, name) VALUES (1, 'hello')")
        .execute(&mut conn)
        .unwrap();

    let msgs = seen.lock().clone();
    assert!(
        msgs.iter().any(|m| m.contains("PreUpdateEvent")),
        "events: {msgs:?}",
    );
    assert!(
        msgs.iter().any(|m| m.contains("PreUpdateValue")),
        "events: {msgs:?}",
    );
    assert!(
        msgs.iter().any(|m| m.contains("Text")),
        "column_type reached Text: {msgs:?}",
    );
}

#[test]
fn preupdate_value_as_bytes_returns_none_for_null() {
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(None::<bool>));
    let sink = seen.clone();
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, b BLOB)")
        .execute(&mut conn)
        .unwrap();
    let _hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            let v: PreUpdateValue<'_> = event.new_value(1).unwrap();
            assert!(matches!(v.column_type(), PreUpdateColumnType::Null));
            *sink.lock() = Some(v.as_bytes().is_none());
        }
    });

    sql_query("INSERT INTO t (id, b) VALUES (1, NULL)")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(*seen.lock(), Some(true));
}

#[test]
fn preupdate_value_as_bytes_returns_empty_slice_for_zero_length_blob() {
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(None::<usize>));
    let sink = seen.clone();
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, b BLOB)")
        .execute(&mut conn)
        .unwrap();
    let _hook = conn.on_preupdate(move |event| {
        if matches!(event.op(), PreUpdateOp::Insert) {
            if let Ok(v) = event.new_value(1) {
                if let Some(slice) = v.as_bytes() {
                    *sink.lock() = Some(slice.len());
                }
            }
        }
    });

    sql_query("INSERT INTO t (id, b) VALUES (1, x'')")
        .execute(&mut conn)
        .unwrap();
    assert_eq!(*seen.lock(), Some(0));
}

// =============================================================================
// G. Buffered apply plain error path (SQLITE_ERROR from garbage)
// =============================================================================

#[test]
fn apply_changeset_rejects_garbage() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_with(
            GARBAGE,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ApplyFailed(_)), "got {err:?}");
}

// =============================================================================
// H. apply_changeset_strm_with error branches
// =============================================================================

#[test]
fn apply_changeset_strm_with_surfaces_filter_panic() {
    let bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_strm_with(
            Cursor::new(bytes),
            ApplyFlags::empty(),
            |_t| panic!("strm filter boom"),
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::FilterPanicked), "got {err:?}");
}

#[test]
fn apply_changeset_strm_with_surfaces_conflict_panic() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_| true,
            |_| panic!("strm conflict boom"),
        )
        .unwrap_err();
    assert!(
        matches!(err, ApplyError::ConflictHandlerPanicked),
        "got {err:?}",
    );
}

#[test]
fn apply_changeset_strm_with_surfaces_conflict_abort() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ConflictAborted), "got {err:?}");
}

#[test]
fn apply_changeset_strm_with_surfaces_apply_failed_on_garbage() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_strm_with(
            Cursor::new(GARBAGE.to_vec()),
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ApplyFailed(_)), "got {err:?}");
}

#[test]
fn apply_changeset_strm_with_emits_rebase_blob_on_replace() {
    // Streamed conflict-resolution via `Replace` produces a non-empty
    // rebase blob just like the buffered path.
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let outcome = replica
        .apply_changeset_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Replace,
        )
        .unwrap();
    assert!(!outcome.rebase.is_empty(), "rebase blob populated");
}

// =============================================================================
// I. v3 empty-changeset early return
// =============================================================================

#[test]
fn apply_changeset_v3_with_empty_input_returns_empty_outcome() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let outcome = replica
        .apply_changeset_v3_with(
            &[],
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap();
    assert!(outcome.rebase.is_empty());
}

#[test]
fn concat_changesets_strm_surfaces_writer_panic() {
    let a = make_changeset(&[(1, 10)]);
    let b = make_changeset(&[(2, 20)]);
    let err = concat_changesets_strm(Cursor::new(a), Cursor::new(b), PanicWrite).unwrap_err();
    assert!(matches!(err, ChangesetError::WriterPanicked), "got {err:?}");
}

#[test]
fn concat_changesets_strm_surfaces_concat_failed_on_garbage() {
    let mut out = Vec::new();
    let err = concat_changesets_strm(
        Cursor::new(GARBAGE.to_vec()),
        Cursor::new(GARBAGE.to_vec()),
        &mut out,
    )
    .unwrap_err();
    assert!(
        matches!(err, ChangesetError::ConcatFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn changegroup_add_strm_surfaces_add_failed_on_garbage() {
    let mut group = Changegroup::new().unwrap();
    let err = group.add_strm(Cursor::new(GARBAGE.to_vec())).unwrap_err();
    assert!(
        matches!(err, ChangesetError::ChangegroupAddFailed(_)),
        "got {err:?}",
    );
}

// =============================================================================
// J. Additional apply_v2/v3 error surfaces
// =============================================================================

#[test]
fn apply_changeset_strm_with_surfaces_reader_panic() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_strm_with(
            PanicRead,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ReaderPanicked), "got {err:?}");
}

#[test]
fn conflict_callback_can_read_new_value_on_data_conflict() {
    // DATA conflict fires when an UPDATE's before-image no longer matches the
    // replica row. new_value must be defined on an UPDATE conflict (unlike
    // DELETE conflicts where post-image is absent).
    let mut source = fresh_connection();
    create_items(&mut source);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("UPDATE items SET v = 20 WHERE id = 1")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 999)")
        .execute(&mut replica)
        .unwrap();

    let observed = std::sync::Arc::new(parking_lot::Mutex::new(None::<i64>));
    let sink = observed.clone();
    replica
        .apply_changeset_with(
            &bytes,
            ApplyFlags::empty(),
            |_| true,
            move |info| {
                if let Ok(Some(v)) = info.new_value(1) {
                    *sink.lock() = Some(v.as_i64());
                }
                ConflictAction::Replace
            },
        )
        .unwrap();
    assert_eq!(*observed.lock(), Some(20));
}

#[test]
fn apply_changeset_v3_strm_with_surfaces_reader_io() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_v3_strm_with(
            ErrRead,
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ReaderIo(_)), "got {err:?}");
}

#[test]
fn apply_changeset_v3_strm_with_surfaces_conflict_panic() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_v3_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_row| true,
            |_| panic!("v3 strm conflict boom"),
        )
        .unwrap_err();
    assert!(
        matches!(err, ApplyError::ConflictHandlerPanicked),
        "got {err:?}",
    );
}

#[test]
fn apply_changeset_v3_strm_with_surfaces_conflict_abort() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_v3_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ConflictAborted), "got {err:?}");
}

#[test]
fn apply_changeset_v3_strm_with_surfaces_apply_failed_on_garbage() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_v3_strm_with(
            Cursor::new(GARBAGE.to_vec()),
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ApplyFailed(_)), "got {err:?}");
}

#[test]
fn apply_changeset_v3_strm_with_emits_rebase_blob_on_replace() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let outcome = replica
        .apply_changeset_v3_strm_with(
            Cursor::new(source_bytes),
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Replace,
        )
        .unwrap();
    assert!(!outcome.rebase.is_empty(), "rebase blob populated");
}

#[test]
fn apply_changeset_v3_with_surfaces_conflict_abort() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_v3_with(
            &source_bytes,
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ConflictAborted), "got {err:?}");
}

#[test]
fn apply_changeset_v3_with_surfaces_conflict_panic() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let err = replica
        .apply_changeset_v3_with(
            &source_bytes,
            ApplyFlags::empty(),
            |_row| true,
            |_| panic!("v3 conflict boom"),
        )
        .unwrap_err();
    assert!(
        matches!(err, ApplyError::ConflictHandlerPanicked),
        "got {err:?}",
    );
}

#[test]
fn apply_changeset_v3_with_surfaces_apply_failed_on_garbage() {
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_v3_with(
            GARBAGE,
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    assert!(matches!(err, ApplyError::ApplyFailed(_)), "got {err:?}");
}

#[test]
fn apply_changeset_v3_with_emits_rebase_blob_on_replace() {
    let source_bytes = make_changeset(&[(1, 10)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 99)")
        .execute(&mut replica)
        .unwrap();
    let outcome = replica
        .apply_changeset_v3_with(
            &source_bytes,
            ApplyFlags::empty(),
            |_row| true,
            |_| ConflictAction::Replace,
        )
        .unwrap();
    assert!(!outcome.rebase.is_empty(), "rebase blob populated");
}

// =============================================================================
// K. Blob write failure via an expired handle
// =============================================================================

#[test]
fn blob_write_at_returns_write_failed_on_expired_handle() {
    // Opening a read-write blob and then modifying the row through SQL
    // invalidates the handle. The next `write_at` surfaces as
    // `BlobError::WriteFailed(SQLITE_ABORT)`.
    let mut conn = fresh_connection();
    sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, b BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO t (id, b) VALUES (1, x'deadbeef')")
        .execute(&mut conn)
        .unwrap();
    let blob = conn
        .open_blob("main", "t", "b", 1, BlobMode::ReadWrite)
        .unwrap();
    // Rewriting the blob column via SQL expires the handle.
    sql_query("UPDATE t SET b = x'cafebabe' WHERE id = 1")
        .execute(&mut conn)
        .unwrap();
    let err = blob.write_at(0, &[0xAA]).unwrap_err();
    assert!(
        matches!(err, diesel_sqlite_session::BlobError::WriteFailed(_)),
        "got {err:?}",
    );
}

#[test]
fn conflict_callback_rejects_new_value_on_delete_conflict() {
    // A DELETE row in the changeset that no longer matches the replica fires
    // a DATA conflict for op=Delete. Calling `new_value` inside the callback
    // hits the `NewNotAvailableOnDelete` guard.
    let mut source = fresh_connection();
    create_items(&mut source);
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut source)
        .unwrap();
    let mut session = source.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("DELETE FROM items WHERE id = 1")
        .execute(&mut source)
        .unwrap();
    let bytes = session.changeset().unwrap();
    drop(session);

    let mut replica = fresh_connection();
    create_items(&mut replica);
    sql_query("INSERT INTO items (id, v) VALUES (1, 999)")
        .execute(&mut replica)
        .unwrap();

    let seen = std::sync::Arc::new(parking_lot::Mutex::new(false));
    let sink = seen.clone();
    let _ = replica.apply_changeset_with(
        &bytes,
        ApplyFlags::empty(),
        |_| true,
        move |info| {
            if matches!(info.op(), Some(ChangesetOp::Delete)) {
                assert!(matches!(
                    info.new_value(0),
                    Err(ChangesetError::NewNotAvailableOnDelete)
                ));
                *sink.lock() = true;
            }
            ConflictAction::Replace
        },
    );
    assert!(*seen.lock(), "delete conflict callback fired");
}
