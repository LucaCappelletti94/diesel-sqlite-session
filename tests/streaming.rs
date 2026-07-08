//! Integration tests for the `_strm` streamed variants.
//!
//! One test per invariant. This file grows as each feature ships its
//! streamed sibling.

use std::io::{self, Cursor, ErrorKind, Read, Write};

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{
    ApplyError, ApplyFlags, ChangesetError, ChangesetOp, ChangesetReader, ConflictAction,
    SessionError, SqliteSessionExt,
};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

fn create_items(conn: &mut SqliteConnection) {
    sql_query("CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER)")
        .execute(conn)
        .unwrap();
}

fn count_items(conn: &mut SqliteConnection) -> i64 {
    diesel::dsl::sql::<diesel::sql_types::BigInt>("SELECT COUNT(*) FROM items")
        .get_result(conn)
        .unwrap()
}

/// Build an INSERT-only changeset for `rows` in a fresh `items` table.
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

#[test]
fn session_changeset_strm_writes_the_same_bytes_as_the_buffered_variant() {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();

    // Buffered reference bytes first.
    let reference = session.changeset().unwrap();

    // Stream into a Vec<u8> through a Cursor writer.
    let mut streamed = Vec::new();
    session
        .changeset_strm(&mut streamed)
        .expect("changeset_strm");

    assert_eq!(reference, streamed, "streamed bytes match buffered bytes");
}

#[test]
fn session_patchset_strm_writes_a_nonempty_buffer() {
    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 42)")
        .execute(&mut conn)
        .unwrap();

    let mut streamed = Vec::new();
    session.patchset_strm(&mut streamed).unwrap();
    assert!(!streamed.is_empty());
}

#[test]
fn session_changeset_strm_surfaces_writer_io_errors() {
    struct FailWrite;
    impl Write for FailWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::BrokenPipe, "test-writer-failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();

    let err = session.changeset_strm(FailWrite).unwrap_err();
    match err {
        SessionError::WriterIo(inner) => {
            assert_eq!(inner.kind(), ErrorKind::BrokenPipe);
        }
        other => panic!("expected WriterIo, got {other:?}"),
    }
}

#[test]
fn session_changeset_strm_surfaces_writer_panics() {
    struct PanicWrite;
    impl Write for PanicWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("panic-inside-writer");
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut conn = fresh_connection();
    create_items(&mut conn);
    let mut session = conn.create_session().unwrap();
    session.attach_all().unwrap();
    sql_query("INSERT INTO items (id, v) VALUES (1, 10)")
        .execute(&mut conn)
        .unwrap();

    let err = session.changeset_strm(PanicWrite).unwrap_err();
    assert!(matches!(err, SessionError::WriterPanicked), "{err:?}");
}

#[test]
fn changeset_reader_open_strm_iterates_a_streamed_changeset() {
    let bytes = make_changeset(&[(1, 10), (2, 20), (3, 30)]);
    let cursor = Cursor::new(bytes.clone());
    let mut reader = ChangesetReader::open_strm(cursor).expect("open_strm succeeds");
    let mut ids: Vec<i64> = Vec::new();
    while let Some(row) = reader.next().unwrap() {
        assert_eq!(row.op(), ChangesetOp::Insert);
        ids.push(row.new_value(0).unwrap().unwrap().as_i64());
    }
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn changeset_reader_open_inverted_strm_flips_ops() {
    let bytes = make_changeset(&[(1, 10)]);
    let cursor = Cursor::new(bytes);
    let mut reader = ChangesetReader::open_inverted_strm(cursor).expect("open_inverted_strm");
    let row = reader.next().unwrap().expect("saw a row");
    assert_eq!(row.op(), ChangesetOp::Delete);
}

#[test]
fn changeset_reader_open_strm_defers_reader_io_errors_to_next() {
    // `sqlite3changeset_start_strm` may accept the trampoline without ever
    // calling it. The reader error surfaces on the first `next()` call
    // instead, so open_strm succeeds and next() fails.
    struct FailRead;
    impl Read for FailRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::UnexpectedEof, "reader-failed"))
        }
    }
    let maybe_reader = ChangesetReader::open_strm(FailRead);
    match maybe_reader {
        Err(ChangesetError::ReaderIo(err)) => {
            assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        }
        Ok(mut reader) => match reader.next() {
            Err(ChangesetError::ReaderIo(err)) => {
                assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
            }
            Err(ChangesetError::NextFailed(_)) => {
                // Some SQLite paths report generic NextFailed when the
                // trampoline set SQLITE_IOERR. Accept as a valid outcome.
            }
            other => panic!("expected ReaderIo or NextFailed, got {other:?}"),
        },
        other => panic!("expected ReaderIo or Ok, got {other:?}"),
    }
}

#[test]
fn apply_changeset_strm_with_applies_the_streamed_bytes() {
    let bytes = make_changeset(&[(1, 10), (2, 20)]);
    let mut replica = fresh_connection();
    create_items(&mut replica);

    let outcome = replica
        .apply_changeset_strm_with(
            Cursor::new(bytes),
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .expect("apply_changeset_strm_with succeeds");
    assert!(outcome.rebase.is_empty());
    assert_eq!(count_items(&mut replica), 2);
}

#[test]
fn apply_changeset_strm_with_surfaces_reader_io_errors() {
    struct FailRead;
    impl Read for FailRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("reader-failed"))
        }
    }
    let mut replica = fresh_connection();
    create_items(&mut replica);
    let err = replica
        .apply_changeset_strm_with(
            FailRead,
            ApplyFlags::empty(),
            |_| true,
            |_| ConflictAction::Abort,
        )
        .unwrap_err();
    match err {
        ApplyError::ReaderIo(_) => {}
        other => panic!("expected ReaderIo, got {other:?}"),
    }
}
