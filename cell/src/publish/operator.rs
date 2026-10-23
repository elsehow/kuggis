//! Local operator policy loading. Existing pins are mandatory: publishing must
//! never do trust-on-first-use, provision a key, or accept a key from an agent.
use crate::{Cell, CellId, Hash, Key, RequestId, Tree};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Debug, Serialize)]
pub struct PolicyPlan {
    pub(super) audience: RequestId,
    pub(super) previous: Hash,
    pub(super) cells: Vec<PolicyCell>,
}
#[derive(Debug, Serialize)]
pub(super) struct PolicyCell {
    cell: Cell,
    parent: Option<CellId>,
    key: Key,
}
impl PolicyPlan {
    pub(super) fn new(
        audience: RequestId,
        previous: Hash,
        tree: &Tree,
        pins: &BTreeMap<CellId, Key>,
    ) -> Result<Self, super::registry::RegistryError> {
        let mut cells = vec![];
        for cell in tree.iter() {
            let key = *pins.get(&cell.id).ok_or_else(|| {
                super::registry::RegistryError::InvalidPolicy(format!(
                    "missing pin for {}",
                    cell.id
                ))
            })?;
            cells.push(PolicyCell {
                cell: cell.clone(),
                parent: tree.parent(&cell.id).map(|p| p.id.clone()),
                key,
            });
        }
        Ok(Self {
            audience,
            previous,
            cells,
        })
    }
    pub fn hash(&self) -> Hash {
        Hash::of(&crate::canon::bytes(self))
    }
}

pub fn load(
    tree_dir: &Path,
    pins_dir: &Path,
) -> crate::apply::Fallible<(Tree, BTreeMap<CellId, Key>)> {
    let (tree, _) = crate::tree::load(tree_dir)?;
    let mut pins = BTreeMap::new();
    for cell in tree.iter() {
        let path = pins_dir.join(format!("{}.pub", cell.id));
        // Pins are trusted operator files, not followed from an agent-controlled
        // link. Missing files are errors, not opportunities to pin a new key.
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(format!("{}: expected a regular pinned-key file", path.display()).into());
        }
        pins.insert(cell.id.clone(), fs::read_to_string(path)?.parse()?);
    }
    Ok((tree, pins))
}
