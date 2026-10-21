//! The tree of cells. The tree owns the edges: a cell knows nothing of
//! where it sits. `Tree::new` is the only constructor, and it refuses every
//! shape the design forbids, so any `&Tree` downstream already holds the
//! invariants.

use crate::cell::Cell;
use crate::grant::{Grant, Harness};
use crate::prim::{Name, Repo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    Duplicate(Name),
    SelfParent(Name),
    MissingParent {
        child: Name,
        parent: Name,
    },
    Cycle(Vec<Name>),
    /// A child holds a grant its parent does not cover.
    Wider {
        child: Name,
        grant: Grant,
    },
    /// More than one writable repository.
    TwoWritable(Name),
    /// The vault beside something that could carry it out.
    VaultBeside {
        cell: Name,
        grant: Grant,
    },
    /// pi needs Claude or Codex in the same cell to talk to a model.
    PiAlone(Name),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            TreeError::VaultBeside { cell, grant } => {
                write!(f, "{cell}: the vault beside {grant:?}")
            }
            TreeError::PiAlone(n) => write!(f, "{n}: pi needs claude or codex beside it"),
        }
    }
}

impl std::error::Error for TreeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    cells: BTreeMap<Name, Cell>,
    /// child -> parent. A cell absent here is held by the human directly.
    under: BTreeMap<Name, Name>,
}

impl Tree {
    /// Build from cells, each paired with the name of the cell it sits under,
    /// or `None` for one the human holds directly.
    pub fn new(cells: impl IntoIterator<Item = (Cell, Option<Name>)>) -> Result<Tree, TreeError> {
        let mut map = BTreeMap::new();
        let mut under = BTreeMap::new();
        for (cell, parent) in cells {
            check_cell(&cell)?;
            if parent.as_ref() == Some(&cell.name) {
                return Err(TreeError::SelfParent(cell.name));
            }
            let name = cell.name.clone();
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

    pub fn get(&self, name: &Name) -> Option<&Cell> {
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

    pub fn parent(&self, name: &Name) -> Option<&Cell> {
        self.under.get(name).and_then(|p| self.cells.get(p))
    }

    /// How many cells sit above this one; the human's own are at 0.
    pub fn depth(&self, name: &Name) -> usize {
        let mut depth = 0;
        let mut cur = name;
        while let Some(p) = self.under.get(cur) {
            depth += 1;
            cur = p;
        }
        depth
    }

    pub fn children<'a>(&'a self, name: &'a Name) -> impl Iterator<Item = &'a Cell> + 'a {
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
        return Err(TreeError::TwoWritable(cell.name.clone()));
    }
    if cell.grants.contains(&Grant::Vault) {
        for g in &cell.grants {
            let carries_out = matches!(
                g,
                Grant::Code { write: true, .. } | Grant::Edge(_) | Grant::Host(_)
            );
            if carries_out {
                return Err(TreeError::VaultBeside {
                    cell: cell.name.clone(),
                    grant: g.clone(),
                });
            }
        }
    }
    if cell.grants.contains(&Grant::Agent(Harness::Pi))
        && !cell.grants.contains(&Grant::Agent(Harness::Claude))
        && !cell.grants.contains(&Grant::Agent(Harness::Codex))
    {
        return Err(TreeError::PiAlone(cell.name.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell under `parent` ("" for the human's own).
    fn node(parent: &str, json: &str) -> (Cell, Option<Name>) {
        let under = (!parent.is_empty()).then(|| parent.parse().unwrap());
        (serde_json::from_str(json).unwrap(), under)
    }

    fn err(nodes: &[(&str, &str)]) -> TreeError {
        Tree::new(nodes.iter().map(|(p, j)| node(p, j))).expect_err("should be refused")
    }

    #[test]
    fn a_good_tree() {
        let t = Tree::new([
            node("", r#"{"name":"forecasting","grants":[{"agent":"claude"},{"code":{"repo":"elsehow/airo","write":true}},{"code":{"repo":"forecastingresearch/airo","write":false}},{"host":"pypi.org"}]}"#),
            node("forecasting", r#"{"name":"airo","grants":[{"agent":"claude"},{"code":{"repo":"elsehow/airo","write":true}},{"code":{"repo":"forecastingresearch/airo","write":false}},{"host":"pypi.org"}]}"#),
            node("airo", r#"{"name":"airo-scratch","grants":[{"agent":"claude"},{"code":{"repo":"elsehow/airo","write":false}}]}"#),
            node("", r#"{"name":"thinker","grants":["vault",{"agent":"claude"},{"agent":"pi"}]}"#),
        ])
        .unwrap();
        assert_eq!(t.len(), 4);
        let scratch: Name = "airo-scratch".parse().unwrap();
        assert_eq!(t.parent(&scratch).unwrap().name.as_str(), "airo");
        assert_eq!(t.children(&"forecasting".parse().unwrap()).count(), 1);
        assert!(t.parent(&"forecasting".parse().unwrap()).is_none());
        assert!(t.get(&"thinker".parse().unwrap()).unwrap().is_room());
    }

    #[test]
    fn the_edge_is_not_in_the_cell() {
        // the same cell, moved: same hash, different parent
        let json = r#"{"name":"airo","grants":[{"agent":"claude"}]}"#;
        let p = r#"{"name":"p","grants":[{"agent":"claude"}]}"#;
        let q = r#"{"name":"q","grants":[{"agent":"claude"}]}"#;
        let a = Tree::new([node("", p), node("", q), node("p", json)]).unwrap();
        let b = Tree::new([node("", p), node("", q), node("q", json)]).unwrap();
        let airo: Name = "airo".parse().unwrap();
        assert_eq!(a.get(&airo).unwrap().hash(), b.get(&airo).unwrap().hash());
        assert_ne!(a.parent(&airo), b.parent(&airo));
        assert_ne!(a, b);
    }

    #[test]
    fn refusals() {
        assert_eq!(
            err(&[("a", r#"{"name":"a"}"#)]),
            TreeError::SelfParent("a".parse().unwrap())
        );
        assert!(matches!(
            err(&[("b", r#"{"name":"a"}"#)]),
            TreeError::MissingParent { .. }
        ));
        assert!(matches!(
            err(&[
                ("b", r#"{"name":"a"}"#),
                ("c", r#"{"name":"b"}"#),
                ("a", r#"{"name":"c"}"#)
            ]),
            TreeError::Cycle(_)
        ));
        // child wider than parent, three ways
        assert!(matches!(
            err(&[
                ("", r#"{"name":"p","grants":[{"agent":"claude"}]}"#),
                ("p", r#"{"name":"c","grants":[{"agent":"codex"}]}"#)
            ]),
            TreeError::Wider { .. }
        ));
        assert!(matches!(
            err(&[
                (
                    "",
                    r#"{"name":"p","grants":[{"code":{"repo":"o/r","write":false}}]}"#
                ),
                (
                    "p",
                    r#"{"name":"c","grants":[{"code":{"repo":"o/r","write":true}}]}"#
                )
            ]),
            TreeError::Wider { .. }
        ));
        assert!(matches!(
            err(&[
                ("", r#"{"name":"p","grants":[{"host":"a.com"}]}"#),
                ("p", r#"{"name":"c","grants":[{"host":"b.com"}]}"#)
            ]),
            TreeError::Wider { .. }
        ));
        // grandchild wider than grandparent but not parent: still refused, at the parent edge
        assert!(matches!(
            err(&[
                ("", r#"{"name":"g","grants":[]}"#),
                ("g", r#"{"name":"p","grants":[{"agent":"claude"}]}"#),
                ("p", r#"{"name":"c","grants":[{"agent":"claude"}]}"#)
            ]),
            TreeError::Wider { child, .. } if child.as_str() == "p"
        ));
        assert_eq!(
            err(&[(
                "",
                r#"{"name":"a","grants":[{"code":{"repo":"o/r","write":true}},{"code":{"repo":"o/s","write":true}}]}"#
            )]),
            TreeError::TwoWritable("a".parse().unwrap())
        );
        assert!(matches!(
            err(&[("", r#"{"name":"a","grants":["vault",{"host":"pypi.org"}]}"#)]),
            TreeError::VaultBeside { .. }
        ));
        assert!(matches!(
            err(&[("", r#"{"name":"a","grants":["vault",{"edge":"brave"}]}"#)]),
            TreeError::VaultBeside { .. }
        ));
        assert!(matches!(
            err(&[(
                "",
                r#"{"name":"a","grants":["vault",{"code":{"repo":"o/r","write":true}}]}"#
            )]),
            TreeError::VaultBeside { .. }
        ));
        assert!(Tree::new([node("", r#"{"name":"a","grants":["vault",{"code":{"repo":"o/r","write":false}},{"agent":"claude"}]}"#)]).is_ok());
        assert_eq!(
            err(&[("", r#"{"name":"a","grants":[{"agent":"pi"}]}"#)]),
            TreeError::PiAlone("a".parse().unwrap())
        );
    }

    #[test]
    fn duplicates_are_refused() {
        assert_eq!(
            err(&[("", r#"{"name":"a"}"#), ("", r#"{"name":"a"}"#)]),
            TreeError::Duplicate("a".parse().unwrap())
        );
    }

    #[test]
    fn a_cell_with_no_grants_is_fine_and_reaches_nothing() {
        let t = Tree::new([node("", r#"{"name":"a"}"#)]).unwrap();
        assert!(t.get(&"a".parse().unwrap()).unwrap().allowlist().is_empty());
    }
}
