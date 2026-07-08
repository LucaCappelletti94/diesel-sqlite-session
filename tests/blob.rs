//! Integration tests for the incremental BLOB I/O wrapper.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{BlobError, BlobMode, SqliteBlob, SqliteSessionExt};

fn fresh_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").expect("open in-memory database")
}

/// Prepare a table `photos (id INTEGER PRIMARY KEY, data BLOB)` with one row
/// that reserves `size` zero bytes at rowid 1.
fn setup_zero_blob(conn: &mut SqliteConnection, size: usize) {
    sql_query("CREATE TABLE photos (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(conn)
        .unwrap();
    sql_query(format!(
        "INSERT INTO photos (id, data) VALUES (1, zeroblob({size}))"
    ))
    .execute(conn)
    .unwrap();
}

fn blob_bytes(conn: &mut SqliteConnection, id: i64) -> Vec<u8> {
    use diesel::sql_types::{BigInt, Binary};

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Binary)]
        data: Vec<u8>,
    }

    sql_query("SELECT data FROM photos WHERE id = ?")
        .bind::<BigInt, _>(id)
        .get_result::<Row>(conn)
        .unwrap()
        .data
}

#[test]
fn open_read_only_reports_the_correct_length() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 32);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open read-only handle");
    assert_eq!(blob.mode(), BlobMode::ReadOnly);
    assert_eq!(blob.len(), 32);
    assert!(!blob.is_empty());
}

#[test]
fn write_at_persists_bytes_and_read_at_reads_them_back() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 16);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open read-write handle");
    let payload = b"HelloBlob";
    blob.write_at(4, payload).expect("write_at succeeds");

    let mut echo = [0u8; 9];
    blob.read_at(4, &mut echo).expect("read_at succeeds");
    assert_eq!(&echo, payload);

    // Close so the write is committed before the outer read.
    blob.close().expect("close succeeds");

    let full = blob_bytes(&mut conn, 1);
    assert_eq!(full.len(), 16);
    // Bytes 0..4 stay zero, then payload, then zero again.
    assert_eq!(&full[..4], &[0, 0, 0, 0]);
    assert_eq!(&full[4..13], payload);
    assert_eq!(&full[13..], &[0, 0, 0]);
}

#[test]
fn write_at_on_read_only_handle_returns_read_only_error() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 8);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open handle");
    let err = blob.write_at(0, b"x").unwrap_err();
    assert!(
        matches!(err, BlobError::ReadOnly),
        "unexpected error: {err:?}"
    );
}

#[test]
fn read_beyond_end_reports_offset_out_of_range() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open handle");
    let mut buf = [0u8; 8];
    match blob.read_at(0, &mut buf) {
        Err(BlobError::OffsetOutOfRange {
            offset,
            buf_len,
            blob_len,
        }) => {
            assert_eq!(offset, 0);
            assert_eq!(buf_len, 8);
            assert_eq!(blob_len, 4);
        }
        other => panic!("expected OffsetOutOfRange, got {other:?}"),
    }
}

#[test]
fn write_extending_past_end_reports_offset_out_of_range() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    let err = blob.write_at(3, b"ab").unwrap_err();
    assert!(
        matches!(
            err,
            BlobError::OffsetOutOfRange {
                offset: 3,
                buf_len: 2,
                blob_len: 4,
            }
        ),
        "unexpected error: {err:?}",
    );
}

#[test]
fn open_with_null_byte_in_names_returns_invalid_name() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let err = conn
        .open_blob("main", "phot\0os", "data", 1, BlobMode::ReadOnly)
        .unwrap_err();
    assert!(matches!(err, BlobError::InvalidName), "unexpected: {err:?}");
}

#[test]
fn open_at_nonexistent_row_returns_open_failed() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let err = conn
        .open_blob("main", "photos", "data", 999, BlobMode::ReadOnly)
        .unwrap_err();
    assert!(
        matches!(err, BlobError::OpenFailed(_)),
        "unexpected: {err:?}"
    );
}

#[test]
fn reopen_points_the_handle_at_a_new_row() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);
    sql_query("INSERT INTO photos (id, data) VALUES (2, x'01020304')")
        .execute(&mut conn)
        .unwrap();

    let mut blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open handle");
    let mut buf = [0u8; 4];
    blob.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, [0, 0, 0, 0]);

    blob.reopen(2).expect("reopen at rowid 2");
    blob.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, [1, 2, 3, 4]);
}

#[test]
fn dropping_the_handle_closes_it() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob: SqliteBlob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    drop(blob);

    // Re-opening at the same row must succeed after the previous handle was
    // closed on drop.
    let blob2 = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open second handle");
    assert_eq!(blob2.len(), 4);
}

#[test]
fn close_returns_ok_on_success() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    blob.close().expect("close reports success");
}

#[test]
fn empty_read_and_write_are_no_ops() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    let mut buf: [u8; 0] = [];
    blob.read_at(0, &mut buf).expect("empty read succeeds");
    blob.write_at(4, &[])
        .expect("empty write at the end succeeds");
}

#[test]
fn open_on_attached_database_uses_the_alias_name() {
    // Attach a fresh in-memory DB as `aux`, create the same schema there,
    // then open a blob handle with the alias. This pins that `open_blob`
    // routes through the `database` argument rather than hardcoding "main".
    let mut conn = fresh_connection();
    sql_query("ATTACH DATABASE ':memory:' AS aux")
        .execute(&mut conn)
        .unwrap();
    sql_query("CREATE TABLE aux.attached_blobs (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&mut conn)
        .unwrap();
    sql_query("INSERT INTO aux.attached_blobs (id, data) VALUES (1, zeroblob(6))")
        .execute(&mut conn)
        .unwrap();

    let blob = conn
        .open_blob("aux", "attached_blobs", "data", 1, BlobMode::ReadWrite)
        .expect("open aux-side handle");
    blob.write_at(0, b"attach").expect("write succeeds");

    let mut echo = [0u8; 6];
    blob.read_at(0, &mut echo).expect("read succeeds");
    assert_eq!(&echo, b"attach");

    // main.attached_blobs must not exist (guards against alias-swallowing).
    let err = conn
        .open_blob("main", "attached_blobs", "data", 1, BlobMode::ReadOnly)
        .unwrap_err();
    assert!(
        matches!(err, BlobError::OpenFailed(_)),
        "unexpected: {err:?}"
    );
}

#[test]
fn deleting_the_target_row_stales_the_handle() {
    // Deleting the row a handle points at leaves the handle expired. Reads
    // and writes through the wrapper fail (either via the pre-flight range
    // check when SQLite reports 0 bytes, or via the underlying accessor
    // returning `SQLITE_ABORT`). Reopening onto the vanished rowid also
    // fails. Both outcomes are user-observable; the exact error variant is
    // SQLite-version dependent, so we only assert failure.
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);

    let blob = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadWrite)
        .expect("open handle");
    assert_eq!(blob.len(), 4);

    sql_query("DELETE FROM photos WHERE id = 1")
        .execute(&mut conn)
        .unwrap();

    let mut buf = [0u8; 4];
    assert!(
        blob.read_at(0, &mut buf).is_err(),
        "read on an expired handle must fail",
    );
    assert!(
        blob.write_at(0, b"nope").is_err(),
        "write on an expired handle must fail",
    );

    // Reopen onto the vanished row must fail because the row no longer exists.
    let mut blob = blob;
    let err = blob.reopen(1).unwrap_err();
    assert!(
        matches!(err, BlobError::ReopenFailed(_)),
        "unexpected: {err:?}"
    );
}

#[test]
fn multiple_read_only_handles_can_coexist_on_the_same_row() {
    let mut conn = fresh_connection();
    setup_zero_blob(&mut conn, 4);
    sql_query("UPDATE photos SET data = x'ABCDEF01' WHERE id = 1")
        .execute(&mut conn)
        .unwrap();

    let a = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open handle A");
    let b = conn
        .open_blob("main", "photos", "data", 1, BlobMode::ReadOnly)
        .expect("open handle B while A is alive");

    let mut buf_a = [0u8; 4];
    let mut buf_b = [0u8; 4];
    a.read_at(0, &mut buf_a).unwrap();
    b.read_at(0, &mut buf_b).unwrap();
    assert_eq!(buf_a, [0xAB, 0xCD, 0xEF, 0x01]);
    assert_eq!(buf_b, buf_a);

    // Drop order does not matter, but this at least pins that both handles
    // finalize cleanly.
    drop(a);
    drop(b);
}
