//! Grants: every reach a cell has beyond its own root. Each grant implies
//! hosts the proxy may tunnel to, and some imply an exe.dev attachment that
//! holds the credential at the edge.

use crate::prim::{Host, Name, Repo};
use serde::{Deserialize, Serialize};

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
    /// A repository at exe.dev's GitHub edge, read or write.
    Code { repo: Repo, write: bool },
    /// BigBrain's door: read and drop. Only on the laptop.
    Vault,
    /// Any other exe.dev integration, by name, attached by tag.
    Edge(Name),
    /// A host the proxy may tunnel to, with nothing injected.
    Host(Host),
}

/// Where Claude Code talks.
pub const ANTHROPIC: &str = "api.anthropic.com";
/// exe.dev's edge for the Codex integration named `codex`.
pub const CODEX_EDGE: &str = "codex.int.exe.xyz";
/// exe.dev's GitHub edge; one hostname for every repo integration.
pub const GITHUB_EDGE: &str = "github.int.exe.xyz";
/// Suffix of every personal exe.dev integration hostname.
pub const EDGE_SUFFIX: &str = ".int.exe.xyz";

impl Grant {
    /// The hosts this grant lets the proxy tunnel to.
    pub fn hosts(&self) -> Vec<Host> {
        let fixed = |s: &str| vec![s.parse().expect("constant host")];
        match self {
            Grant::Agent(Harness::Claude) => fixed(ANTHROPIC),
            Grant::Agent(Harness::Codex) => fixed(CODEX_EDGE),
            Grant::Agent(Harness::Pi) => vec![],
            Grant::Code { .. } => fixed(GITHUB_EDGE),
            Grant::Vault => vec![],
            Grant::Edge(name) => fixed(&format!("{name}{EDGE_SUFFIX}")),
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

    /// Does `self` reach at least as far as `other`? Equal grants do; a
    /// writable repo covers the same repo read-only; nothing else covers
    /// anything.
    pub fn covers(&self, other: &Grant) -> bool {
        match (self, other) {
            (
                Grant::Code {
                    repo: a,
                    write: true,
                },
                Grant::Code { repo: b, .. },
            ) => a == b,
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
        }
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
        assert_eq!(Grant::Agent(Harness::Claude).hosts()[0].as_str(), ANTHROPIC);
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
