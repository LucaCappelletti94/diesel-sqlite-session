//! The dynamic type of a value read from a changeset row or a pre-update hook.

use crate::ffi::{SQLITE_BLOB, SQLITE_FLOAT, SQLITE_INTEGER, SQLITE_NULL, SQLITE_TEXT};

/// The dynamic type of a column value, as `sqlite3_value_type` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ColumnType {
    /// 64-bit signed integer.
    Integer = SQLITE_INTEGER,
    /// IEEE 754 double.
    Float = SQLITE_FLOAT,
    /// UTF-8 text.
    Text = SQLITE_TEXT,
    /// Raw byte string.
    Blob = SQLITE_BLOB,
    /// `NULL`.
    Null = SQLITE_NULL,
}

impl ColumnType {
    /// The type `code` names, with any unrecognised code read as `NULL`.
    pub(crate) fn from_raw(code: i32) -> Self {
        match code {
            SQLITE_INTEGER => Self::Integer,
            SQLITE_FLOAT => Self::Float,
            SQLITE_TEXT => Self::Text,
            SQLITE_BLOB => Self::Blob,
            _ => Self::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ColumnType;
    use crate::ffi::{SQLITE_BLOB, SQLITE_FLOAT, SQLITE_INTEGER, SQLITE_NULL, SQLITE_TEXT};

    #[test]
    fn column_type_from_raw_falls_back_to_null() {
        assert_eq!(ColumnType::from_raw(SQLITE_INTEGER), ColumnType::Integer);
        assert_eq!(ColumnType::from_raw(SQLITE_FLOAT), ColumnType::Float);
        assert_eq!(ColumnType::from_raw(SQLITE_TEXT), ColumnType::Text);
        assert_eq!(ColumnType::from_raw(SQLITE_BLOB), ColumnType::Blob);
        assert_eq!(ColumnType::from_raw(SQLITE_NULL), ColumnType::Null);
        assert_eq!(ColumnType::from_raw(-42), ColumnType::Null);
    }
}
