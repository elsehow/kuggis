//! Read-only compatibility for signed logs. Preserve the original field order
//! and values for signature/hash verification; only the opened view is upgraded.
use crate::{Hash, Name, Provider, RequestId, Said, SessionId, SessionRef, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Harness {
    Claude,
    Codex,
    Pi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum LegacySaid {
    Session {
        name: Name,
        harness: Harness,
        state: State,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Brief {
        to: Name,
        session: Name,
        text: String,
    },
    Checked {
        cell: Hash,
        ok: bool,
        report: String,
        #[serde(default = "Hash::zero", skip_serializing_if = "Hash::is_zero")]
        binary: Hash,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        signed: BTreeSet<Harness>,
    },
}

pub(crate) fn session_id(cell: &Name, name: &Name) -> SessionId {
    let hash = Hash::of(format!("kuggis/legacy-session/{cell}/{name}").as_bytes());
    SessionId::from(<[u8; 16]>::try_from(&hash.as_bytes()[..16]).unwrap())
}

impl LegacySaid {
    pub(crate) fn upgrade(&self, owner: &Name, seq: u64) -> Said {
        match self {
            Self::Session {
                name, state, text, ..
            } => Said::Session {
                session: SessionRef {
                    cell: owner.clone(),
                    id: session_id(owner, name),
                },
                state: *state,
                text: text.clone(),
            },
            Self::Brief { to, session, text } => {
                let hash = Hash::of(&crate::canon::bytes(&(owner, seq, self)));
                Said::Brief {
                    request: RequestId::from(<[u8; 16]>::try_from(&hash.as_bytes()[..16]).unwrap()),
                    session: SessionRef {
                        cell: to.clone(),
                        id: session_id(to, session),
                    },
                    text: text.clone(),
                }
            }
            Self::Checked {
                cell,
                ok,
                report,
                binary,
                signed,
            } => Said::Checked {
                cell: *cell,
                ok: *ok,
                report: report.clone(),
                binary: *binary,
                signed: signed
                    .iter()
                    .filter_map(|h| match h {
                        Harness::Claude => Some(Provider::Anthropic),
                        Harness::Codex => Some(Provider::OpenAi),
                        Harness::Pi => None,
                    })
                    .collect(),
            },
        }
    }
}
