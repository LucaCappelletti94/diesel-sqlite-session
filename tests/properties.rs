//! Property-based tests for replication invariants.

use diesel::prelude::*;
use diesel::sql_query;
use diesel_sqlite_session::{ConflictAction, SqliteSessionExt};

diesel::table! {
    prop_items (id) {
        id -> Integer,
        name -> Nullable<Text>,
        quantity -> Nullable<Integer>,
    }
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Put {
        id: i32,
        name: Option<&'static str>,
        quantity: Option<i32>,
    },
    Delete {
        id: i32,
    },
}

fn setup_connection() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    sql_query("CREATE TABLE prop_items (id INTEGER PRIMARY KEY, name TEXT, quantity INTEGER)")
        .execute(&mut conn)
        .unwrap();
    conn
}

fn apply_operation(conn: &mut SqliteConnection, operation: &Operation) {
    match operation {
        Operation::Put { id, name, quantity } => {
            diesel::replace_into(prop_items::table)
                .values((
                    prop_items::id.eq(*id),
                    prop_items::name.eq(*name),
                    prop_items::quantity.eq(*quantity),
                ))
                .execute(conn)
                .unwrap();
        }
        Operation::Delete { id } => {
            diesel::delete(prop_items::table.filter(prop_items::id.eq(*id)))
                .execute(conn)
                .unwrap();
        }
    }
}

fn fetch_rows(conn: &mut SqliteConnection) -> Vec<(i32, Option<String>, Option<i32>)> {
    prop_items::table
        .select((prop_items::id, prop_items::name, prop_items::quantity))
        .order_by(prop_items::id.asc())
        .load(conn)
        .unwrap()
}

fn verify_replication_invariants(operations: &[Operation]) {
    let mut source_for_patchset = setup_connection();
    let mut patch_session = source_for_patchset.create_session().unwrap();
    patch_session.attach::<prop_items::table>().unwrap();
    for operation in operations {
        apply_operation(&mut source_for_patchset, operation);
    }
    let expected_rows = fetch_rows(&mut source_for_patchset);
    let patchset = patch_session.patchset().unwrap();

    let mut source_for_changeset = setup_connection();
    let mut changeset_session = source_for_changeset.create_session().unwrap();
    changeset_session.attach::<prop_items::table>().unwrap();
    for operation in operations {
        apply_operation(&mut source_for_changeset, operation);
    }
    assert_eq!(fetch_rows(&mut source_for_changeset), expected_rows);
    let changeset = changeset_session.changeset().unwrap();

    let mut patchset_replica = setup_connection();
    patchset_replica
        .apply_patchset(&patchset, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(fetch_rows(&mut patchset_replica), expected_rows);

    let mut changeset_replica = setup_connection();
    changeset_replica
        .apply_changeset(&changeset, |_| ConflictAction::Abort)
        .unwrap();
    assert_eq!(fetch_rows(&mut changeset_replica), expected_rows);
}

fn enumerate_sequences<F>(
    candidates: &[Operation],
    max_len: usize,
    current: &mut Vec<Operation>,
    on_sequence: &mut F,
) where
    F: FnMut(&[Operation]),
{
    on_sequence(current);
    if current.len() == max_len {
        return;
    }

    for operation in candidates {
        current.push(*operation);
        enumerate_sequences(candidates, max_len, current, on_sequence);
        current.pop();
    }
}

#[test]
fn changeset_and_patchset_replicate_the_same_final_state() {
    let candidates = [
        Operation::Put {
            id: 1,
            name: Some("alpha"),
            quantity: Some(10),
        },
        Operation::Put {
            id: 1,
            name: None,
            quantity: None,
        },
        Operation::Put {
            id: 2,
            name: Some("beta"),
            quantity: Some(20),
        },
        Operation::Put {
            id: 3,
            name: Some("gamma"),
            quantity: Some(-3),
        },
        Operation::Delete { id: 1 },
        Operation::Delete { id: 2 },
    ];

    let mut sequence = Vec::new();
    enumerate_sequences(&candidates, 4, &mut sequence, &mut |ops| {
        verify_replication_invariants(ops);
    });
}
