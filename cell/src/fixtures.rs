//! Explicit identities for tests reconstructing the same declaration.
use crate::{Cell, CellId, Grant};
pub fn cell(id: CellId, grants: impl IntoIterator<Item = Grant>) -> Cell {
    // The label is deliberately uninteresting in these reconciliation fixtures.
    Cell {
        name: id.as_str().parse().unwrap(),
        id,
        grants: grants.into_iter().collect(),
    }
}
