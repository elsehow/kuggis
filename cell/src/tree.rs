//! The tree of cells. The tree owns the edges: a cell knows nothing of
//! where it sits. `Tree::new` is the only constructor, and it refuses every
//! shape the design forbids, so any `&Tree` downstream already holds the
//! invariants.

use crate::cell::Cell;
use crate::grant::Grant;
use crate::prim::{Name, Repo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub fn load(dir: &Path) -> crate::apply::Fallible<(Tree, BTreeMap<Name, PathBuf>)> {
    fn walk(
        dir: &Path,
        parent: Option<Name>,
        cells: &mut Vec<(Cell, Option<Name>)>,
        paths: &mut BTreeMap<Name, PathBuf>,
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
                let name: Name = stem.parse()?;
                let cell: Cell = serde_json::from_slice(&std::fs::read(&path)?)?;
                if cell.name != name {
                    return Err(format!("{}: holds cell {}", path.display(), cell.name).into());
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
        parent: Name,
        grant: Grant,
    },
    /// More than one writable repository.
    TwoWritable(Name),
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
            TreeError::Wider {
                parent,
                grant: Grant::Host(_),
                ..
            } => {
                write!(f, "You must add these hosts to parent cell {parent}. (Children cannot have more access than their parents).")
            }
            TreeError::Wider { child, grant, .. } => {
                write!(f, "{child}: {grant:?} is not covered by its parent")
            }
            TreeError::TwoWritable(n) => write!(f, "{n}: more than one writable repository"),
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
                        parent: parent_name.clone(),
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
            node("", r#"{"name":"coordinator","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"}]}"#),
            node("coordinator", r#"{"name":"widget","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"}]}"#),
            node("widget", r#"{"name":"widget-scratch","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":false}}]}"#),
            node("", r#"{"name":"research","grants":[{"provider":"anthropic"}]}"#),
        ])
        .unwrap();
        assert_eq!(t.len(), 4);
        let scratch: Name = "widget-scratch".parse().unwrap();
        assert_eq!(t.parent(&scratch).unwrap().name.as_str(), "widget");
        assert_eq!(t.children(&"coordinator".parse().unwrap()).count(), 1);
        assert!(t.parent(&"coordinator".parse().unwrap()).is_none());
        assert!(t.parent(&"research".parse().unwrap()).is_none());
    }

    #[test]
    fn the_edge_is_not_in_the_cell() {
        // the same cell, moved: same hash, different parent
        let json = r#"{"name":"widget","grants":[{"provider":"anthropic"}]}"#;
        let p = r#"{"name":"p","grants":[{"provider":"anthropic"}]}"#;
        let q = r#"{"name":"q","grants":[{"provider":"anthropic"}]}"#;
        let a = Tree::new([node("", p), node("", q), node("p", json)]).unwrap();
        let b = Tree::new([node("", p), node("", q), node("q", json)]).unwrap();
        let widget: Name = "widget".parse().unwrap();
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
                ("", r#"{"name":"p","grants":[{"provider":"anthropic"}]}"#),
                ("p", r#"{"name":"c","grants":[{"provider":"openai"}]}"#)
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
                ("g", r#"{"name":"p","grants":[{"provider":"anthropic"}]}"#),
                ("p", r#"{"name":"c","grants":[{"provider":"anthropic"}]}"#)
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

    #[test]
    fn public_network_can_be_delegated_or_narrowed_but_not_invented() {
        let public = r#"{"name":"p","grants":[{"network":"public"}]}"#;
        for child in [
            r#"{"name":"c","grants":[{"network":"public"}]}"#,
            r#"{"name":"c","grants":[{"host":"en.wikipedia.org"}]}"#,
            r#"{"name":"c"}"#,
        ] {
            Tree::new([node("", public), node("p", child)]).unwrap();
        }
        assert!(matches!(
            err(&[
                ("", r#"{"name":"p","grants":[{"host":"en.wikipedia.org"}]}"#),
                ("p", r#"{"name":"c","grants":[{"network":"public"}]}"#),
            ]),
            TreeError::Wider { .. }
        ));
        assert!(matches!(
            err(&[
                ("", public),
                ("p", r#"{"name":"c","grants":[{"edge":"brave"}]}"#),
            ]),
            TreeError::Wider { .. }
        ));
    }
}
