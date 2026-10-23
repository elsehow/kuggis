//! The tree of cells. The tree owns the edges: a cell knows nothing of
//! where it sits. `Tree::new` is the only constructor, and it refuses every
//! shape the design forbids, so any `&Tree` downstream already holds the
//! invariants.

use crate::cell::Cell;
use crate::grant::Grant;
use crate::prim::{CellId, Name, Repo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub fn load(dir: &Path) -> crate::apply::Fallible<(Tree, BTreeMap<CellId, PathBuf>)> {
    fn walk(
        dir: &Path,
        parent: Option<CellId>,
        cells: &mut Vec<(Cell, Option<CellId>)>,
        paths: &mut BTreeMap<CellId, PathBuf>,
    ) -> crate::apply::Fallible {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().to_string();
            if file.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_symlink() {
                return Err(
                    format!("{}: symlinks are not cell declarations", path.display()).into(),
                );
            }
            if path.is_dir() {
                walk(&path, Some(file.parse()?), cells, paths)?;
            } else if let Some(stem) = file.strip_suffix(".json") {
                let name: CellId = stem.parse()?;
                let cell: Cell = serde_json::from_slice(&std::fs::read(&path)?)?;
                if cell.id != name {
                    return Err(format!("{}: holds cell {}", path.display(), cell.id).into());
                }
                paths.insert(name, path);
                cells.push((cell, parent.clone()));
            }
        }
        Ok(())
    }
    let mut cells = vec![];
    let mut paths = BTreeMap::new();
    walk(dir, None, &mut cells, &mut paths)?;
    Ok((Tree::new(cells)?, paths))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    Duplicate(CellId),
    DuplicateName(Name),
    SelfParent(CellId),
    MissingParent {
        child: CellId,
        parent: CellId,
    },
    Cycle(Vec<CellId>),
    /// A child holds a grant its parent does not cover.
    Wider {
        child: CellId,
        grant: Grant,
    },
    /// More than one writable repository.
    TwoWritable(CellId),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeError::DuplicateName(n) => write!(f, "{n}: name already in use"),
            TreeError::Duplicate(n) => write!(f, "{n}: declared twice"),
            TreeError::SelfParent(n) => write!(f, "{n}: is its own parent"),
            TreeError::MissingParent { child, parent } => {
                write!(f, "{child}: parent {parent} does not exist")
            }
            TreeError::Cycle(path) => {
                let s: Vec<String> = path.iter().map(ToString::to_string).collect();
                write!(f, "cycle: {}", s.join(" -> "))
            }
            TreeError::Wider { child, grant } => {
                write!(f, "{child}: {grant:?} is not covered by its parent")
            }
            TreeError::TwoWritable(n) => write!(f, "{n}: more than one writable repository"),
        }
    }
}

impl std::error::Error for TreeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    cells: BTreeMap<CellId, Cell>,
    /// child -> parent. A cell absent here is held by the human directly.
    under: BTreeMap<CellId, CellId>,
}

impl Tree {
    /// Build from cells, each paired with the ID of the cell it sits under,
    /// or `None` for one the human holds directly.
    pub fn new(cells: impl IntoIterator<Item = (Cell, Option<CellId>)>) -> Result<Tree, TreeError> {
        let mut map = BTreeMap::new();
        let mut under = BTreeMap::new();
        let mut names = BTreeSet::new();
        for (cell, parent) in cells {
            check_cell(&cell)?;
            if map.contains_key(&cell.id) {
                return Err(TreeError::Duplicate(cell.id));
            }
            if !names.insert(cell.name.clone()) {
                return Err(TreeError::DuplicateName(cell.name));
            }
            if parent.as_ref() == Some(&cell.id) {
                return Err(TreeError::SelfParent(cell.id));
            }
            let name = cell.id.clone();
            if map.insert(name.clone(), cell).is_some() {
                return Err(TreeError::Duplicate(name));
            }
            if let Some(p) = parent {
                under.insert(name, p);
            }
        }
        let tree = Tree { cells: map, under };
        tree.check_edges()?;
        Ok(tree)
    }

    fn check_edges(&self) -> Result<(), TreeError> {
        for (name, cell) in &self.cells {
            let Some(parent_name) = self.under.get(name) else {
                continue;
            };
            let parent = self
                .cells
                .get(parent_name)
                .ok_or_else(|| TreeError::MissingParent {
                    child: name.clone(),
                    parent: parent_name.clone(),
                })?;
            for g in &cell.grants {
                if !parent.grants.iter().any(|p| p.covers(g)) {
                    return Err(TreeError::Wider {
                        child: name.clone(),
                        grant: g.clone(),
                    });
                }
            }
        }
        for start in self.under.keys() {
            let mut path = vec![start.clone()];
            let mut cur = start;
            while let Some(p) = self.under.get(cur) {
                if path.contains(p) {
                    path.push(p.clone());
                    return Err(TreeError::Cycle(path));
                }
                path.push(p.clone());
                cur = p;
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &CellId) -> Option<&Cell> {
        self.cells.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cell> {
        self.cells.values()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn parent(&self, name: &CellId) -> Option<&Cell> {
        self.under.get(name).and_then(|p| self.cells.get(p))
    }

    /// How many cells sit above this one; the human's own are at 0.
    pub fn depth(&self, name: &CellId) -> usize {
        let mut depth = 0;
        let mut cur = name;
        while let Some(p) = self.under.get(cur) {
            depth += 1;
            cur = p;
        }
        depth
    }

    pub fn children<'a>(&'a self, name: &'a CellId) -> impl Iterator<Item = &'a Cell> + 'a {
        self.under
            .iter()
            .filter(move |(_, p)| *p == name)
            .filter_map(|(c, _)| self.cells.get(c))
    }
}

fn check_cell(cell: &Cell) -> Result<(), TreeError> {
    let writable: BTreeSet<&Repo> = cell
        .grants
        .iter()
        .filter_map(|g| match g {
            Grant::Code {
                repo, write: true, ..
            } => Some(repo),
            _ => None,
        })
        .collect();
    if writable.len() > 1 {
        return Err(TreeError::TwoWritable(cell.id.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell under `parent` ("" for the human's own).
    fn node(parent: &str, json: &str) -> (Cell, Option<CellId>) {
        let under = (!parent.is_empty()).then(|| parent.parse().unwrap());
        (serde_json::from_str(json).unwrap(), under)
    }

    fn err(nodes: &[(&str, &str)]) -> TreeError {
        Tree::new(nodes.iter().map(|(p, j)| node(p, j))).expect_err("should be refused")
    }

    #[test]
    fn a_good_tree() {
        let t = Tree::new([
            node("", r#"{"id":"17e2081f-027e-5c4d-baeb-59faca4d38be","name":"17e2081f-027e-5c4d-baeb-59faca4d38be","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"}]}"#),
            node("17e2081f-027e-5c4d-baeb-59faca4d38be", r#"{"id":"839960b4-330d-5e73-80ff-6de714463468","name":"839960b4-330d-5e73-80ff-6de714463468","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"}]}"#),
            node("839960b4-330d-5e73-80ff-6de714463468", r#"{"id":"e6f81d9d-ae32-518a-a2e5-092a580ce15a","name":"e6f81d9d-ae32-518a-a2e5-092a580ce15a","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":false}}]}"#),
            node("", r#"{"id":"36f4a626-0bf6-5251-abbd-5a6f8e1c85ec","name":"36f4a626-0bf6-5251-abbd-5a6f8e1c85ec","grants":[{"provider":"anthropic"}]}"#),
        ])
        .unwrap();
        assert_eq!(t.len(), 4);
        let scratch: CellId = "e6f81d9d-ae32-518a-a2e5-092a580ce15a".parse().unwrap();
        assert_eq!(
            t.parent(&scratch).unwrap().name.as_str(),
            "839960b4-330d-5e73-80ff-6de714463468"
        );
        assert_eq!(
            t.children(&"17e2081f-027e-5c4d-baeb-59faca4d38be".parse().unwrap())
                .count(),
            1
        );
        assert!(t
            .parent(&"17e2081f-027e-5c4d-baeb-59faca4d38be".parse().unwrap())
            .is_none());
        assert!(t
            .parent(&"36f4a626-0bf6-5251-abbd-5a6f8e1c85ec".parse().unwrap())
            .is_none());
    }

    #[test]
    fn the_edge_is_not_in_the_cell() {
        // the same cell, moved: same hash, different parent
        let json = r#"{"id":"839960b4-330d-5e73-80ff-6de714463468","name":"839960b4-330d-5e73-80ff-6de714463468","grants":[{"provider":"anthropic"}]}"#;
        let p = r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","grants":[{"provider":"anthropic"}]}"#;
        let q = r#"{"id":"118fd667-8e32-5aa4-bcef-d107a7f3abca","name":"118fd667-8e32-5aa4-bcef-d107a7f3abca","grants":[{"provider":"anthropic"}]}"#;
        let a = Tree::new([
            node("", p),
            node("", q),
            node("30f2fb95-bfa2-5caa-b55c-5f41d348f293", json),
        ])
        .unwrap();
        let b = Tree::new([
            node("", p),
            node("", q),
            node("118fd667-8e32-5aa4-bcef-d107a7f3abca", json),
        ])
        .unwrap();
        let widget: CellId = "839960b4-330d-5e73-80ff-6de714463468".parse().unwrap();
        assert_eq!(
            a.get(&widget).unwrap().hash(),
            b.get(&widget).unwrap().hash()
        );
        assert_ne!(a.parent(&widget), b.parent(&widget));
        assert_ne!(a, b);
    }

    #[test]
    fn refusals() {
        assert_eq!(
            err(&[(
                "904254dc-1b8f-5e80-9755-e5afc5b195c5",
                r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#
            )]),
            TreeError::SelfParent("904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
        );
        assert!(matches!(
            err(&[(
                "46b16f13-11ac-5315-b431-845f196f907e",
                r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#
            )]),
            TreeError::MissingParent { .. }
        ));
        assert!(matches!(
            err(&[
                (
                    "46b16f13-11ac-5315-b431-845f196f907e",
                    r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#
                ),
                (
                    "acc27909-a4a4-5232-953d-15b8d534ae2a",
                    r#"{"id":"46b16f13-11ac-5315-b431-845f196f907e","name":"46b16f13-11ac-5315-b431-845f196f907e"}"#
                ),
                (
                    "904254dc-1b8f-5e80-9755-e5afc5b195c5",
                    r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a"}"#
                )
            ]),
            TreeError::Cycle(_)
        ));
        // child wider than parent, three ways
        assert!(matches!(
            err(&[
                (
                    "",
                    r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","grants":[{"provider":"anthropic"}]}"#
                ),
                (
                    "30f2fb95-bfa2-5caa-b55c-5f41d348f293",
                    r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a","grants":[{"provider":"openai"}]}"#
                )
            ]),
            TreeError::Wider { .. }
        ));
        assert!(matches!(
            err(&[
                (
                    "",
                    r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","grants":[{"code":{"repo":"o/r","write":false}}]}"#
                ),
                (
                    "30f2fb95-bfa2-5caa-b55c-5f41d348f293",
                    r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a","grants":[{"code":{"repo":"o/r","write":true}}]}"#
                )
            ]),
            TreeError::Wider { .. }
        ));
        assert!(matches!(
            err(&[
                (
                    "",
                    r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","grants":[{"host":"a.com"}]}"#
                ),
                (
                    "30f2fb95-bfa2-5caa-b55c-5f41d348f293",
                    r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a","grants":[{"host":"b.com"}]}"#
                )
            ]),
            TreeError::Wider { .. }
        ));
        // grandchild wider than grandparent but not parent: still refused, at the parent edge
        assert!(matches!(
            err(&[
                ("", r#"{"id":"62ca8bcc-6228-5e7f-a2ad-70ac49f441b6","name":"g","grants":[]}"#),
                ("62ca8bcc-6228-5e7f-a2ad-70ac49f441b6", r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","grants":[{"provider":"anthropic"}]}"#),
                ("30f2fb95-bfa2-5caa-b55c-5f41d348f293", r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a","grants":[{"provider":"anthropic"}]}"#)
            ]),
            TreeError::Wider { child, .. } if child.as_str() == "30f2fb95-bfa2-5caa-b55c-5f41d348f293"
        ));
        assert_eq!(
            err(&[(
                "",
                r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","grants":[{"code":{"repo":"o/r","write":true}},{"code":{"repo":"o/s","write":true}}]}"#
            )]),
            TreeError::TwoWritable("904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
        );
    }

    #[test]
    fn duplicates_are_refused() {
        assert_eq!(
            err(&[
                (
                    "",
                    r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#
                ),
                (
                    "",
                    r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#
                )
            ]),
            TreeError::Duplicate("904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
        );
    }

    #[test]
    fn a_cell_with_no_grants_is_fine_and_reaches_nothing() {
        let t = Tree::new([node("", r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5"}"#)]).unwrap();
        assert!(t
            .get(&"904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
            .unwrap()
            .allowlist()
            .is_empty());
    }
}
