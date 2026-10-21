//! Durable conversations. Identity and ownership do not depend on a process,
//! a title, a selected model, or a particular user interface.

use crate::{Name, RequestId, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Session {
    id: SessionId,
    cell: Name,
    pub title: String,
    pub archived: bool,
}

impl Session {
    pub fn new(cell: Name, title: String) -> Self {
        Self::restore(SessionId::generate(), cell, title, false)
    }

    /// Restore an identity, including when importing the original local chat metadata.
    pub fn restore(id: SessionId, cell: Name, title: String, archived: bool) -> Self {
        Self {
            id,
            cell,
            title,
            archived,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }
    pub fn cell(&self) -> &Name {
        &self.cell
    }
    pub fn reference(&self) -> SessionRef {
        SessionRef {
            cell: self.cell.clone(),
            id: self.id,
        }
    }
}

/// An address across a cell boundary. A bare session ID is not authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub cell: Name,
    pub id: SessionId,
}

impl SessionRef {
    pub fn check_owner(&self, owner: &Name) -> Result<(), String> {
        if &self.cell == owner {
            Ok(())
        } else {
            Err(format!(
                "session {} belongs to {}, not {owner}",
                self.id, self.cell
            ))
        }
    }
}

/// A model-independent instruction. Pi's messages and deltas remain wire data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Prompt {
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Image>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<StreamingBehavior>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Image {
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommand {
    pub request: RequestId,
    pub session: SessionRef,
    pub action: SessionAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum SessionAction {
    Prompt(Prompt),
    Cancel,
}

impl SessionCommand {
    pub fn validate(&self, session: &Session) -> Result<(), String> {
        self.session.check_owner(session.cell())?;
        if self.session.id != session.id() {
            return Err("command targets a different session".into());
        }
        if let SessionAction::Prompt(input) = &self.action {
            if input.content.trim().is_empty() {
                return Err("content must be a nonempty string".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_renaming_archiving_and_serialization() {
        let mut session = Session::new("dev".parse().unwrap(), "First".into());
        let address = session.reference();
        session.title = "Renamed".into();
        session.archived = true;
        let restored: Session =
            serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
        assert_eq!(restored.reference(), address);
        assert_eq!(restored, session);
        assert!(serde_json::from_str::<Session>(
            r#"{"id":"00000000000000000000000000000000","title":"Missing owner","archived":false}"#
        )
        .is_err());
    }

    #[test]
    fn commands_cannot_cross_session_or_cell_boundaries() {
        let session = Session::new("dev".parse().unwrap(), "".into());
        let mut command = SessionCommand {
            request: RequestId::generate(),
            session: session.reference(),
            action: SessionAction::Cancel,
        };
        assert!(command.validate(&session).is_ok());
        command.session.cell = "parent".parse().unwrap();
        assert!(command.validate(&session).is_err());
        command.session = session.reference();
        command.session.id = SessionId::generate();
        assert!(command.validate(&session).is_err());
    }

    #[test]
    fn opaque_ids_parse_strictly_and_are_not_display_names() {
        for bad in [
            "dev",
            "../dev",
            "",
            "ABCDEF0123456789ABCDEF0123456789",
            "00",
        ] {
            assert!(bad.parse::<SessionId>().is_err());
            assert!(bad.parse::<RequestId>().is_err());
        }
        let id = SessionId::generate();
        assert_eq!(id.to_string().parse::<SessionId>().unwrap(), id);
        assert_ne!(id, SessionId::generate());
    }
}
