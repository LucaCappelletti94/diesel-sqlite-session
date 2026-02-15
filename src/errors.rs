//! Error types for session operations.

use std::fmt;

use thiserror::Error;

/// `SQLite` result codes returned by the session extension.
///
/// These correspond to `SQLite`'s [result codes](https://www.sqlite.org/rescode.html).
/// Only codes relevant to session operations are enumerated; others are captured
/// in the `Unknown` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqliteErrorCode {
    /// Generic error (`SQLITE_ERROR` = 1).
    Error,
    /// Internal logic error (`SQLITE_INTERNAL` = 2).
    Internal,
    /// Access permission denied (`SQLITE_PERM` = 3).
    Permission,
    /// Database file is locked (`SQLITE_BUSY` = 5).
    Busy,
    /// A table in the database is locked (`SQLITE_LOCKED` = 6).
    Locked,
    /// Memory allocation failed (`SQLITE_NOMEM` = 7).
    NoMemory,
    /// Attempt to write a readonly database (`SQLITE_READONLY` = 8).
    ReadOnly,
    /// Database schema changed (`SQLITE_SCHEMA` = 17).
    Schema,
    /// Library used incorrectly (`SQLITE_MISUSE` = 21).
    Misuse,
    /// Unknown or unhandled `SQLite` error code.
    Unknown(i32),
}

impl SqliteErrorCode {
    /// Create from a raw `SQLite` result code.
    ///
    /// Returns `None` for `SQLITE_OK` (0) since that indicates success.
    #[must_use]
    pub const fn from_raw(code: i32) -> Option<Self> {
        match code {
            0 => None, // SQLITE_OK
            1 => Some(Self::Error),
            2 => Some(Self::Internal),
            3 => Some(Self::Permission),
            5 => Some(Self::Busy),
            6 => Some(Self::Locked),
            7 => Some(Self::NoMemory),
            8 => Some(Self::ReadOnly),
            17 => Some(Self::Schema),
            21 => Some(Self::Misuse),
            other => Some(Self::Unknown(other)),
        }
    }

    /// Create from a non-zero `SQLite` error code.
    ///
    /// Use this when you've already verified the code is not `SQLITE_OK`.
    /// Falls back to `Unknown(code)` if the code is 0 or unrecognized.
    #[must_use]
    pub const fn from_error(code: i32) -> Self {
        match code {
            1 => Self::Error,
            2 => Self::Internal,
            3 => Self::Permission,
            5 => Self::Busy,
            6 => Self::Locked,
            7 => Self::NoMemory,
            8 => Self::ReadOnly,
            17 => Self::Schema,
            21 => Self::Misuse,
            other => Self::Unknown(other),
        }
    }

    /// Get the raw `SQLite` result code.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        match self {
            Self::Error => 1,
            Self::Internal => 2,
            Self::Permission => 3,
            Self::Busy => 5,
            Self::Locked => 6,
            Self::NoMemory => 7,
            Self::ReadOnly => 8,
            Self::Schema => 17,
            Self::Misuse => 21,
            Self::Unknown(code) => code,
        }
    }
}

impl fmt::Display for SqliteErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => write!(f, "SQLITE_ERROR (1)"),
            Self::Internal => write!(f, "SQLITE_INTERNAL (2)"),
            Self::Permission => write!(f, "SQLITE_PERM (3)"),
            Self::Busy => write!(f, "SQLITE_BUSY (5)"),
            Self::Locked => write!(f, "SQLITE_LOCKED (6)"),
            Self::NoMemory => write!(f, "SQLITE_NOMEM (7)"),
            Self::ReadOnly => write!(f, "SQLITE_READONLY (8)"),
            Self::Schema => write!(f, "SQLITE_SCHEMA (17)"),
            Self::Misuse => write!(f, "SQLITE_MISUSE (21)"),
            Self::Unknown(code) => write!(f, "SQLITE_UNKNOWN ({code})"),
        }
    }
}

/// Errors that can occur when working with `SQLite` sessions.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Failed to create a new session.
    #[error("Failed to create session: {0}")]
    CreateFailed(SqliteErrorCode),

    /// Failed to attach a table to the session.
    #[error("Failed to attach table: {0}")]
    AttachFailed(SqliteErrorCode),

    /// Failed to generate a changeset.
    #[error("Failed to generate changeset: {0}")]
    ChangesetFailed(SqliteErrorCode),

    /// Failed to generate a patchset.
    #[error("Failed to generate patchset: {0}")]
    PatchsetFailed(SqliteErrorCode),

    /// Table name contains invalid characters.
    #[error("Table name contains null byte")]
    InvalidTableName,
}

/// Errors that can occur when applying changesets or patchsets.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// Failed to apply the changeset or patchset.
    #[error("Failed to apply changeset: {0}")]
    ApplyFailed(SqliteErrorCode),

    /// The conflict handler returned [`ConflictAction::Abort`].
    #[error("Conflict handler requested abort")]
    ConflictAborted,
}

/// Types of conflicts that can occur when applying changes.
///
/// These correspond to `SQLite`'s `SQLITE_CHANGESET_*` conflict codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ConflictType {
    /// A row with the same primary key already exists (INSERT conflict).
    Data = 1,
    /// The row to be updated/deleted was not found.
    NotFound = 2,
    /// The row to be updated/deleted has different values than expected.
    Conflict = 3,
    /// A constraint (other than foreign key) was violated.
    Constraint = 4,
    /// A foreign key constraint was violated.
    ForeignKey = 5,
}

impl ConflictType {
    /// Create a `ConflictType` from an `SQLite` conflict code.
    #[must_use]
    pub const fn from_raw(code: i32) -> Option<Self> {
        match code {
            1 => Some(Self::Data),
            2 => Some(Self::NotFound),
            3 => Some(Self::Conflict),
            4 => Some(Self::Constraint),
            5 => Some(Self::ForeignKey),
            _ => None,
        }
    }

    /// Convert to the raw `SQLite` conflict code.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        self as i32
    }
}

/// Action to take when a conflict occurs during changeset application.
///
/// These correspond to `SQLite`'s `SQLITE_CHANGESET_*` resolution codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ConflictAction {
    /// Skip this conflicting change and continue with the next one.
    Omit = 0,
    /// Force apply this change, replacing the existing row.
    Replace = 1,
    /// Stop processing and return an error.
    Abort = 2,
}

impl ConflictAction {
    /// Convert to the raw `SQLite` conflict resolution code.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        self as i32
    }
}
