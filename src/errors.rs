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

#[cfg(test)]
mod tests {
    use super::*;

    mod sqlite_error_code {
        use super::*;

        #[test]
        fn from_raw_returns_none_for_ok() {
            assert_eq!(SqliteErrorCode::from_raw(0), None);
        }

        #[test]
        fn from_raw_maps_known_codes() {
            assert_eq!(SqliteErrorCode::from_raw(1), Some(SqliteErrorCode::Error));
            assert_eq!(
                SqliteErrorCode::from_raw(2),
                Some(SqliteErrorCode::Internal)
            );
            assert_eq!(
                SqliteErrorCode::from_raw(3),
                Some(SqliteErrorCode::Permission)
            );
            assert_eq!(SqliteErrorCode::from_raw(5), Some(SqliteErrorCode::Busy));
            assert_eq!(SqliteErrorCode::from_raw(6), Some(SqliteErrorCode::Locked));
            assert_eq!(
                SqliteErrorCode::from_raw(7),
                Some(SqliteErrorCode::NoMemory)
            );
            assert_eq!(
                SqliteErrorCode::from_raw(8),
                Some(SqliteErrorCode::ReadOnly)
            );
            assert_eq!(SqliteErrorCode::from_raw(17), Some(SqliteErrorCode::Schema));
            assert_eq!(SqliteErrorCode::from_raw(21), Some(SqliteErrorCode::Misuse));
        }

        #[test]
        fn from_raw_maps_unknown_codes() {
            assert_eq!(
                SqliteErrorCode::from_raw(99),
                Some(SqliteErrorCode::Unknown(99))
            );
            assert_eq!(
                SqliteErrorCode::from_raw(-1),
                Some(SqliteErrorCode::Unknown(-1))
            );
        }

        #[test]
        fn from_error_maps_known_codes() {
            assert_eq!(SqliteErrorCode::from_error(1), SqliteErrorCode::Error);
            assert_eq!(SqliteErrorCode::from_error(2), SqliteErrorCode::Internal);
            assert_eq!(SqliteErrorCode::from_error(3), SqliteErrorCode::Permission);
            assert_eq!(SqliteErrorCode::from_error(5), SqliteErrorCode::Busy);
            assert_eq!(SqliteErrorCode::from_error(6), SqliteErrorCode::Locked);
            assert_eq!(SqliteErrorCode::from_error(7), SqliteErrorCode::NoMemory);
            assert_eq!(SqliteErrorCode::from_error(8), SqliteErrorCode::ReadOnly);
            assert_eq!(SqliteErrorCode::from_error(17), SqliteErrorCode::Schema);
            assert_eq!(SqliteErrorCode::from_error(21), SqliteErrorCode::Misuse);
        }

        #[test]
        fn from_error_maps_unknown_codes() {
            assert_eq!(SqliteErrorCode::from_error(0), SqliteErrorCode::Unknown(0));
            assert_eq!(
                SqliteErrorCode::from_error(99),
                SqliteErrorCode::Unknown(99)
            );
        }

        #[test]
        fn to_raw_roundtrips() {
            assert_eq!(SqliteErrorCode::Error.to_raw(), 1);
            assert_eq!(SqliteErrorCode::Internal.to_raw(), 2);
            assert_eq!(SqliteErrorCode::Permission.to_raw(), 3);
            assert_eq!(SqliteErrorCode::Busy.to_raw(), 5);
            assert_eq!(SqliteErrorCode::Locked.to_raw(), 6);
            assert_eq!(SqliteErrorCode::NoMemory.to_raw(), 7);
            assert_eq!(SqliteErrorCode::ReadOnly.to_raw(), 8);
            assert_eq!(SqliteErrorCode::Schema.to_raw(), 17);
            assert_eq!(SqliteErrorCode::Misuse.to_raw(), 21);
            assert_eq!(SqliteErrorCode::Unknown(42).to_raw(), 42);
        }

        #[test]
        fn display_formats_correctly() {
            assert_eq!(SqliteErrorCode::Error.to_string(), "SQLITE_ERROR (1)");
            assert_eq!(SqliteErrorCode::Internal.to_string(), "SQLITE_INTERNAL (2)");
            assert_eq!(SqliteErrorCode::Permission.to_string(), "SQLITE_PERM (3)");
            assert_eq!(SqliteErrorCode::Busy.to_string(), "SQLITE_BUSY (5)");
            assert_eq!(SqliteErrorCode::Locked.to_string(), "SQLITE_LOCKED (6)");
            assert_eq!(SqliteErrorCode::NoMemory.to_string(), "SQLITE_NOMEM (7)");
            assert_eq!(SqliteErrorCode::ReadOnly.to_string(), "SQLITE_READONLY (8)");
            assert_eq!(SqliteErrorCode::Schema.to_string(), "SQLITE_SCHEMA (17)");
            assert_eq!(SqliteErrorCode::Misuse.to_string(), "SQLITE_MISUSE (21)");
            assert_eq!(
                SqliteErrorCode::Unknown(99).to_string(),
                "SQLITE_UNKNOWN (99)"
            );
        }
    }

    mod session_error {
        use super::*;

        #[test]
        fn display_create_failed() {
            let err = SessionError::CreateFailed(SqliteErrorCode::NoMemory);
            assert_eq!(
                err.to_string(),
                "Failed to create session: SQLITE_NOMEM (7)"
            );
        }

        #[test]
        fn display_attach_failed() {
            let err = SessionError::AttachFailed(SqliteErrorCode::Error);
            assert_eq!(err.to_string(), "Failed to attach table: SQLITE_ERROR (1)");
        }

        #[test]
        fn display_changeset_failed() {
            let err = SessionError::ChangesetFailed(SqliteErrorCode::Busy);
            assert_eq!(
                err.to_string(),
                "Failed to generate changeset: SQLITE_BUSY (5)"
            );
        }

        #[test]
        fn display_patchset_failed() {
            let err = SessionError::PatchsetFailed(SqliteErrorCode::Locked);
            assert_eq!(
                err.to_string(),
                "Failed to generate patchset: SQLITE_LOCKED (6)"
            );
        }

        #[test]
        fn display_invalid_table_name() {
            let err = SessionError::InvalidTableName;
            assert_eq!(err.to_string(), "Table name contains null byte");
        }

        #[test]
        fn is_std_error() {
            fn assert_error<E: std::error::Error>() {}
            assert_error::<SessionError>();
        }
    }

    mod apply_error {
        use super::*;

        #[test]
        fn display_apply_failed() {
            let err = ApplyError::ApplyFailed(SqliteErrorCode::Schema);
            assert_eq!(
                err.to_string(),
                "Failed to apply changeset: SQLITE_SCHEMA (17)"
            );
        }

        #[test]
        fn display_conflict_aborted() {
            let err = ApplyError::ConflictAborted;
            assert_eq!(err.to_string(), "Conflict handler requested abort");
        }

        #[test]
        fn is_std_error() {
            fn assert_error<E: std::error::Error>() {}
            assert_error::<ApplyError>();
        }
    }

    mod conflict_type {
        use super::*;

        #[test]
        fn from_raw_maps_known_codes() {
            assert_eq!(ConflictType::from_raw(1), Some(ConflictType::Data));
            assert_eq!(ConflictType::from_raw(2), Some(ConflictType::NotFound));
            assert_eq!(ConflictType::from_raw(3), Some(ConflictType::Conflict));
            assert_eq!(ConflictType::from_raw(4), Some(ConflictType::Constraint));
            assert_eq!(ConflictType::from_raw(5), Some(ConflictType::ForeignKey));
        }

        #[test]
        fn from_raw_returns_none_for_unknown() {
            assert_eq!(ConflictType::from_raw(0), None);
            assert_eq!(ConflictType::from_raw(6), None);
            assert_eq!(ConflictType::from_raw(-1), None);
        }

        #[test]
        fn to_raw_returns_correct_values() {
            assert_eq!(ConflictType::Data.to_raw(), 1);
            assert_eq!(ConflictType::NotFound.to_raw(), 2);
            assert_eq!(ConflictType::Conflict.to_raw(), 3);
            assert_eq!(ConflictType::Constraint.to_raw(), 4);
            assert_eq!(ConflictType::ForeignKey.to_raw(), 5);
        }
    }

    mod conflict_action {
        use super::*;

        #[test]
        fn to_raw_returns_correct_values() {
            assert_eq!(ConflictAction::Omit.to_raw(), 0);
            assert_eq!(ConflictAction::Replace.to_raw(), 1);
            assert_eq!(ConflictAction::Abort.to_raw(), 2);
        }
    }
}
