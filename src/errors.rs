//! Error types for session operations.

use thiserror::Error;

/// Errors that can occur when working with `SQLite` sessions.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Failed to create a new session.
    #[error("Failed to create session: SQLite error code {0}")]
    CreateFailed(i32),

    /// Failed to attach a table to the session.
    #[error("Failed to attach table: SQLite error code {0}")]
    AttachFailed(i32),

    /// Failed to generate a changeset.
    #[error("Failed to generate changeset: SQLite error code {0}")]
    ChangesetFailed(i32),

    /// Failed to generate a patchset.
    #[error("Failed to generate patchset: SQLite error code {0}")]
    PatchsetFailed(i32),

    /// Table name contains invalid characters.
    #[error("Table name contains null byte")]
    InvalidTableName,
}

/// Errors that can occur when applying changesets or patchsets.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// Failed to apply the changeset or patchset.
    #[error("Failed to apply changeset: SQLite error code {0}")]
    ApplyFailed(i32),

    /// A conflict occurred and the conflict handler requested abort.
    #[error("Conflict aborted: {0}")]
    ConflictAborted(String),
}

/// Types of conflicts that can occur when applying changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictType {
    /// A row with the same primary key already exists (INSERT conflict).
    Data,
    /// The row to be updated/deleted was not found.
    NotFound,
    /// The row to be updated/deleted has different values than expected.
    Conflict,
    /// A foreign key constraint was violated.
    ForeignKey,
    /// A constraint (other than foreign key) was violated.
    Constraint,
}

impl ConflictType {
    /// Create a `ConflictType` from an `SQLite` conflict code.
    #[must_use]
    pub fn from_sqlite(code: i32) -> Option<Self> {
        // SQLite conflict codes from sqlite3session.h
        const SQLITE_CHANGESET_DATA: i32 = 1;
        const SQLITE_CHANGESET_NOTFOUND: i32 = 2;
        const SQLITE_CHANGESET_CONFLICT: i32 = 3;
        const SQLITE_CHANGESET_CONSTRAINT: i32 = 4;
        const SQLITE_CHANGESET_FOREIGN_KEY: i32 = 5;

        match code {
            SQLITE_CHANGESET_DATA => Some(Self::Data),
            SQLITE_CHANGESET_NOTFOUND => Some(Self::NotFound),
            SQLITE_CHANGESET_CONFLICT => Some(Self::Conflict),
            SQLITE_CHANGESET_CONSTRAINT => Some(Self::Constraint),
            SQLITE_CHANGESET_FOREIGN_KEY => Some(Self::ForeignKey),
            _ => None,
        }
    }
}

/// Action to take when a conflict occurs during changeset application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// Skip this conflicting change and continue with the next one.
    Omit,
    /// Force apply this change, replacing the existing row.
    Replace,
    /// Stop processing and return an error.
    Abort,
}

impl ConflictAction {
    /// Convert to `SQLite` conflict resolution code.
    #[must_use]
    pub fn to_sqlite(self) -> i32 {
        // SQLite conflict resolution codes from sqlite3session.h
        const SQLITE_CHANGESET_OMIT: i32 = 0;
        const SQLITE_CHANGESET_REPLACE: i32 = 1;
        const SQLITE_CHANGESET_ABORT: i32 = 2;

        match self {
            Self::Omit => SQLITE_CHANGESET_OMIT,
            Self::Replace => SQLITE_CHANGESET_REPLACE,
            Self::Abort => SQLITE_CHANGESET_ABORT,
        }
    }
}
