//! Scenarios the native integration tests and the browser tests in `wasm-tests`
//! both run, so each platform checks the same assertions.

use diesel::SqliteConnection;
use diesel_sqlite_session::{SessionError, SqliteSessionExt};

/// A session on a database name nothing attached fails at construction.
pub fn assert_unattached_database_is_refused(conn: &mut SqliteConnection) {
    let err = conn.create_session_on("side").unwrap_err();
    assert!(
        matches!(&err, SessionError::UnknownDatabase(name) if name == "side"),
        "{err:?}",
    );
}
