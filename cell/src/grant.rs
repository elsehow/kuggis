//! Grants: every reach a cell has beyond its own root. Each grant implies
//! hosts the proxy may tunnel to, and some imply an exe.dev attachment that
//! holds the credential at the edge.

use crate::canon;
use crate::prim::{Host, Name, Repo};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A model provider a cell may reach. Executables are not capabilities.
/// Credentials live in the cell’s Pi auth store.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAi,
    PaperInstruments,
}
impl Provider {
    pub const ALL: [Self; 3] = [Self::Anthropic, Self::OpenAi, Self::PaperInstruments];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::PaperInstruments => "paperinstruments",
        }
    }
    pub fn oauth_id(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("anthropic"),
            Self::OpenAi => Some("openai-codex"),
            Self::PaperInstruments => None,
        }
    }
    pub fn from_pi(name: &str) -> Option<Self> {
        match name {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "openai-codex" => Some(Self::OpenAi),
            "paperinstruments" => Some(Self::PaperInstruments),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Grant {
    /// Provider access, independent of the runtime. Credentials are held
    /// separately on the box or at the edge, never in a grant.
    Provider(Provider),
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
    /// Any other exe.dev integration, by name, attached by tag.
    Edge(Name),
    /// A host the proxy may tunnel to, with nothing injected.
    Host(Host),
}

impl fmt::Display for Provider {
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
/// Pi’s API-key and subscription endpoints, including device-code sign-in.
pub const OPENAI_HOSTS: [&str; 3] = ["api.openai.com", "auth.openai.com", "chatgpt.com"];
pub const PAPER_INSTRUMENTS_HOST: &str = "api.paperinstruments.com";
/// exe.dev's GitHub edge; one hostname for every repo integration.
pub const GITHUB_EDGE: &str = "github.int.exe.xyz";
/// Suffix of every personal exe.dev integration hostname.
pub const EDGE_SUFFIX: &str = ".int.exe.xyz";

impl Grant {
    /// The hosts this grant lets the proxy tunnel to.
    pub fn hosts(&self) -> Vec<Host> {
        let fixed = |s: &str| -> Host { s.parse().expect("constant host") };
        match self {
            Grant::Provider(Provider::Anthropic) => CLAUDE_HOSTS.iter().map(|h| fixed(h)).collect(),
            Grant::Provider(Provider::OpenAi) => OPENAI_HOSTS.iter().map(|h| fixed(h)).collect(),
            Grant::Provider(Provider::PaperInstruments) => vec![fixed(PAPER_INSTRUMENTS_HOST)],
            Grant::Code { .. } => vec![fixed(GITHUB_EDGE)],
            Grant::Edge(name) => vec![fixed(&format!("{name}{EDGE_SUFFIX}"))],
            Grant::Host(h) => vec![h.clone()],
        }
    }

    /// Enforced by an exe.dev attachment, so observable and attachable
    /// there. The rest are enforced on the box and reconciled by setup.
    pub fn at_edge(&self) -> bool {
        matches!(self, Grant::Code { .. } | Grant::Edge(_))
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

    #[test]
    fn paper_instruments_is_a_scoped_pi_provider() {
        let grant: Grant = serde_json::from_str(r#"{"provider":"paperinstruments"}"#).unwrap();
        assert_eq!(grant, Grant::Provider(Provider::PaperInstruments));
        assert_eq!(
            grant.hosts(),
            vec![PAPER_INSTRUMENTS_HOST.parse::<Host>().unwrap()]
        );
        assert!(!grant.at_edge());
        assert!(!Grant::Provider(Provider::OpenAi).covers(&grant));
        assert_eq!(
            Provider::from_pi("paperinstruments"),
            Some(Provider::PaperInstruments)
        );
        assert_eq!(Provider::PaperInstruments.oauth_id(), None);
        assert_eq!(
            Provider::PaperInstruments.login_methods(),
            vec![crate::provider::LoginMethod::ApiKey]
        );
    }

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
        assert!(Grant::Provider(Provider::Anthropic).covers(&Grant::Provider(Provider::Anthropic)));
        assert!(!Grant::Provider(Provider::Anthropic).covers(&Grant::Provider(Provider::OpenAi)));
    }

    #[test]
    fn hosts_are_fixed_per_grant() {
        let claude: Vec<String> = Grant::Provider(Provider::Anthropic)
            .hosts()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(claude, CLAUDE_HOSTS);
        assert_eq!(
            Grant::Edge("brave".parse().unwrap()).hosts()[0].as_str(),
            "brave.int.exe.xyz"
        );
        assert!(serde_json::from_str::<Grant>(r#"{"provider":"pi"}"#).is_err());
    }

    #[test]
    fn json_shape() {
        let g = vec![
            Grant::Provider(Provider::Anthropic),
            code("example/widget", true),
            Grant::Edge("brave".parse().unwrap()),
            Grant::Host("pypi.org".parse().unwrap()),
        ];
        let s = serde_json::to_string(&g).unwrap();
        assert_eq!(
            s,
            r#"[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":true}},{"edge":"brave"},{"host":"pypi.org"}]"#
        );
        let back: Vec<Grant> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, g);
        assert!(serde_json::from_str::<Grant>(
            r#"{"code":{"repo":"a/b","write":true,"merge":true}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Grant>(r#"{"host":"10.0.0.1"}"#).is_err());
        assert!(serde_json::from_str::<Grant>(r#"{"provider":"gemini"}"#).is_err());
        assert!(serde_json::from_str::<Grant>(r#"{"agent":"claude"}"#).is_err());
        assert_eq!(
            Grant::Provider(Provider::OpenAi).hosts()[0].as_str(),
            OPENAI_HOSTS[0],
        );
    }

    #[test]
    fn order_is_total_and_stable() {
        let mut g = [
            Grant::Host("a.com".parse().unwrap()),
            Grant::Edge("brave".parse().unwrap()),
            Grant::Provider(Provider::Anthropic),
            code("o/r", false),
        ];
        g.sort();
        assert_eq!(g[0], Grant::Provider(Provider::Anthropic));
        assert_eq!(g[1], code("o/r", false));
        assert_eq!(g[2], Grant::Edge("brave".parse().unwrap()));
    }
}
