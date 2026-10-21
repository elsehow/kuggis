//! Grants: every reach a cell has beyond its own root. Each grant implies
//! hosts the proxy may tunnel to, and some imply an exe.dev attachment that
//! holds the credential at the edge.

use crate::canon;
use crate::prim::{Host, Name, Repo};
use serde::{Deserialize, Serialize};
use std::fmt;

/// An agent harness a cell may run, signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Pi,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Grant {
    /// May run this harness. Claude's token lives on the box; Codex talks
    /// to exe.dev's edge; pi borrows whichever of those is present.
    Agent(Harness),
    /// A repository at exe.dev's GitHub edge, read or write. `via` names a
    /// fork the grant is carried by: the edge then holds the fork, and the
    /// authority is still on `repo`. The edge is one GitHub App for every
    /// cell, so what tells a parent from a child is which repository each
    /// carries: the child its fork, the parent the upstream. Merging is
    /// writing the upstream.
    Code {
        repo: Repo,
        write: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<Repo>,
    },
    /// BigBrain's door: read and drop. Only on the laptop.
    Vault,
    /// Any other exe.dev integration, by name, attached by tag.
    Edge(Name),
    /// A host the proxy may tunnel to, with nothing injected.
    Host(Host),
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        canon::display(self, f)
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        canon::display(self, f)
    }
}

/// Where Claude Code talks: the API, and the two hosts its sign-in needs.
/// Everything else it would reach, the updater, claude.ai connectors,
/// telemetry, stays behind the wall.
pub const CLAUDE_HOSTS: [&str; 3] = ["api.anthropic.com", "claude.ai", "platform.claude.com"];
/// exe.dev's edge for the Codex integration named `codex`.
pub const CODEX_EDGE: &str = "codex.int.exe.xyz";
/// exe.dev's GitHub edge; one hostname for every repo integration.
pub const GITHUB_EDGE: &str = "github.int.exe.xyz";
/// Suffix of every personal exe.dev integration hostname.
pub const EDGE_SUFFIX: &str = ".int.exe.xyz";

impl Grant {
    /// The hosts this grant lets the proxy tunnel to.
    pub fn hosts(&self) -> Vec<Host> {
        let fixed = |s: &str| -> Host { s.parse().expect("constant host") };
        match self {
            Grant::Agent(Harness::Claude) => CLAUDE_HOSTS.iter().map(|h| fixed(h)).collect(),
            Grant::Agent(Harness::Codex) => vec![fixed(CODEX_EDGE)],
            Grant::Agent(Harness::Pi) => vec![],
            Grant::Code { .. } => vec![fixed(GITHUB_EDGE)],
            Grant::Vault => vec![],
            Grant::Edge(name) => vec![fixed(&format!("{name}{EDGE_SUFFIX}"))],
            Grant::Host(h) => vec![h.clone()],
        }
    }

    /// Enforced by an exe.dev attachment, so observable and attachable
    /// there. The rest are enforced on the box and reconciled by setup.
    pub fn at_edge(&self) -> bool {
        matches!(
            self,
            Grant::Agent(Harness::Codex) | Grant::Code { .. } | Grant::Edge(_)
        )
    }

    /// The grant as the edge carries it: the fork in place of its upstream.
    pub fn carried(&self) -> Grant {
        match self {
            Grant::Code {
                write,
                via: Some(fork),
                ..
            } => Grant::Code {
                repo: fork.clone(),
                write: *write,
                via: None,
            },
            other => other.clone(),
        }
    }

    /// Does `self` reach at least as far as `other`? Equal grants do; a
    /// writable repo covers the same repo read-only; nothing else covers
    /// anything.
    pub fn covers(&self, other: &Grant) -> bool {
        match (self, other) {
            (
                Grant::Code {
                    repo: a,
                    write: wa,
                    via: va,
                },
                Grant::Code {
                    repo: b,
                    write: wb,
                    via: vb,
                },
            ) => a == b && (*wa || !*wb) && (va.is_none() || va == vb),
            _ => self == other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(repo: &str, write: bool) -> Grant {
        Grant::Code {
            repo: repo.parse().unwrap(),
            write,
            via: None,
        }
    }

    fn via(repo: &str, write: bool, fork: &str) -> Grant {
        Grant::Code {
            repo: repo.parse().unwrap(),
            write,
            via: Some(fork.parse().unwrap()),
        }
    }

    #[test]
    fn a_fork_carries_the_upstream_grant() {
        let g = via("o/r", true, "cells/r");
        assert_eq!(
            serde_json::to_string(&g).unwrap(),
            r#"{"code":{"repo":"o/r","write":true,"via":"cells/r"}}"#
        );
        assert_eq!(g.carried(), code("cells/r", true));
        assert_eq!(code("o/r", true).carried(), code("o/r", true));
        assert!(
            code("o/r", true).covers(&g),
            "upstream write covers any fork"
        );
        assert!(via("o/r", true, "cells/r").covers(&g));
        assert!(
            !via("o/r", true, "other/r").covers(&g),
            "another fork does not"
        );
        assert!(
            !g.covers(&code("o/r", true)),
            "a fork does not cover the upstream"
        );
        assert!(!code("o/r", false).covers(&g));
        assert_eq!(g.hosts()[0].as_str(), GITHUB_EDGE);
    }

    #[test]
    fn covering() {
        assert!(code("o/r", true).covers(&code("o/r", false)));
        assert!(code("o/r", true).covers(&code("o/r", true)));
        assert!(!code("o/r", false).covers(&code("o/r", true)));
        assert!(!code("o/r", true).covers(&code("o/x", false)));
        assert!(Grant::Vault.covers(&Grant::Vault));
        assert!(!Grant::Vault.covers(&Grant::Agent(Harness::Claude)));
    }

    #[test]
    fn hosts_are_fixed_per_grant() {
        let claude: Vec<String> = Grant::Agent(Harness::Claude)
            .hosts()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(claude, CLAUDE_HOSTS);
        assert_eq!(
            Grant::Edge("brave".parse().unwrap()).hosts()[0].as_str(),
            "brave.int.exe.xyz"
        );
        assert!(Grant::Vault.hosts().is_empty());
        assert!(Grant::Agent(Harness::Pi).hosts().is_empty());
    }

    #[test]
    fn json_shape() {
        let g = vec![
            Grant::Agent(Harness::Claude),
            code("elsehow/airo", true),
            Grant::Vault,
            Grant::Edge("brave".parse().unwrap()),
            Grant::Host("pypi.org".parse().unwrap()),
        ];
        let s = serde_json::to_string(&g).unwrap();
        assert_eq!(
            s,
            r#"[{"agent":"claude"},{"code":{"repo":"elsehow/airo","write":true}},"vault",{"edge":"brave"},{"host":"pypi.org"}]"#
        );
        let back: Vec<Grant> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, g);
        assert!(serde_json::from_str::<Grant>(
            r#"{"code":{"repo":"a/b","write":true,"merge":true}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Grant>(r#"{"host":"10.0.0.1"}"#).is_err());
        assert!(serde_json::from_str::<Grant>(r#"{"agent":"gemini"}"#).is_err());
    }

    #[test]
    fn order_is_total_and_stable() {
        let mut g = [
            Grant::Host("a.com".parse().unwrap()),
            Grant::Vault,
            Grant::Agent(Harness::Pi),
            code("o/r", false),
        ];
        g.sort();
        assert_eq!(g[0], Grant::Agent(Harness::Pi));
        assert_eq!(g[1], code("o/r", false));
        assert_eq!(g[2], Grant::Vault);
    }
}
