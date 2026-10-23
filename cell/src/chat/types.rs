//! assistant-ui DTOs. Only the embedded Session is durable domain data.

use crate::pi::protocol::{AssistantDelta, ModelSelection, ThinkingLevel};
use crate::session::StreamingBehavior;
use crate::{Prompt, Session, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Typed envelope for Pi events that Kuggis interprets. The original JSON
/// remains available to pass through unknown extension fields and event kinds.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PiEvent {
    AgentStart,
    AgentEnd,
    AgentSettled,
    TurnStart,
    TurnEnd,
    MessageStart {
        message: PiMessage,
    },
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: AssistantDelta,
    },
    MessageEnd {
        message: PiMessage,
    },
    QueueUpdate {
        steering: Vec<String>,
        #[serde(rename = "followUp")]
        follow_up: Vec<String>,
    },
    SessionInfoChanged {
        name: String,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
    },
    CompactionStart,
    AutoCompactionStart,
    CompactionEnd,
    AutoCompactionEnd,
    AutoRetryStart {
        attempt: u64,
    },
    AutoRetryEnd,
    ExtensionUiRequest {
        id: String,
        method: String,
    },
    ExtensionUiResolved {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    Error {
        error: String,
    },
    #[serde(other)]
    Unknown,
}

impl PiEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::AgentSettled => "agent_settled",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::QueueUpdate { .. } => "queue_update",
            Self::SessionInfoChanged { .. } => "session_info_changed",
            Self::ThinkingLevelChanged { .. } => "thinking_level_changed",
            Self::CompactionStart => "compaction_start",
            Self::AutoCompactionStart => "auto_compaction_start",
            Self::CompactionEnd => "compaction_end",
            Self::AutoCompactionEnd => "auto_compaction_end",
            Self::AutoRetryStart { .. } => "auto_retry_start",
            Self::AutoRetryEnd => "auto_retry_end",
            Self::ExtensionUiRequest { .. } => "extension_ui_request",
            Self::ExtensionUiResolved { .. } => "extension_ui_resolved",
            Self::Error { .. } => "error",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct PiMessage {
    pub role: String,
    #[serde(rename = "stopReason")]
    pub stop_reason: Option<String>,
    #[serde(rename = "errorMessage")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RunStatus {
    Idle,
    Running,
    Failed,
}

/// The operator requires these fields before deciding whether a cell may restart
/// or whether its first chat already exists. Extra UI metadata is irrelevant.
#[derive(Debug, Deserialize)]
pub(crate) struct SessionSummary {
    pub id: SessionId,
    pub cell: crate::Name,
    pub status: RunStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedThread {
    pub metadata: SessionSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelConfig {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ModelSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(super) enum Readiness {
    Ready {
        selection: ModelSelection,
        source: SelectionSource,
    },
    MissingModel {
        message: &'static str,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SelectionSource {
    Session,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelInfo {
    #[serde(flatten)]
    pub selection: ModelSelection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadSnapshot {
    pub metadata: ThreadMetadata,
    pub messages: Vec<serde_json::Value>,
    pub host_ui_requests: Vec<HostUiRequest>,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostUiRequest {
    pub id: String,
    #[serde(flatten)]
    pub kind: HostUiKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum HostUiKind {
    Confirm {
        title: String,
        message: String,
    },
    Select {
        title: String,
        options: Vec<String>,
    },
    Input {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Editor {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields, rename_all_fields = "camelCase")]
pub(super) enum HostUiResponse {
    Confirm {
        request_id: String,
        confirmed: bool,
    },
    Value {
        request_id: String,
        value: String,
    },
    Dismiss {
        request_id: String,
        dismissed: crate::pi::protocol::True,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostUiSubmission {
    pub response: HostUiResponse,
}

impl HostUiResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Confirm { request_id, .. }
            | Self::Value { request_id, .. }
            | Self::Dismiss { request_id, .. } => request_id,
        }
    }
    pub fn resolve(
        self,
        request: &HostUiRequest,
    ) -> Result<crate::pi::protocol::HostReply, &'static str> {
        use crate::pi::protocol::HostReply;
        match (self, &request.kind) {
            (Self::Dismiss { dismissed, .. }, _) => Ok(HostReply::Cancelled {
                cancelled: dismissed,
            }),
            (Self::Confirm { confirmed, .. }, HostUiKind::Confirm { .. }) => {
                Ok(HostReply::Confirm { confirmed })
            }
            (Self::Value { value, .. }, HostUiKind::Select { options, .. })
                if options.contains(&value) =>
            {
                Ok(HostReply::Value { value })
            }
            (Self::Value { value, .. }, HostUiKind::Input { .. } | HostUiKind::Editor { .. }) => {
                Ok(HostReply::Value { value })
            }
            _ => Err("Reply does not match the pending request or its offered options"),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListThreads {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenameThread {
    pub title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetThinking {
    pub level: ThinkingLevel,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ThreadAction {
    Archive,
    Unarchive,
    Cancel,
    Model,
    Thinking,
    HostUi,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct QueuedMessage {
    pub id: String,
    pub mode: StreamingBehavior,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadMetadata {
    #[serde(flatten)]
    pub session: Session,
    pub workspace_path: PathBuf,
    pub status: RunStatus,
    pub compaction_active: bool,
    pub retry_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_attempt: Option<u64>,
    pub queued_messages: Vec<QueuedMessage>,
    pub config: ModelConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    pub message_count: usize,
    /// When a run last started or ended. The browser orders chats by it and
    /// marks a finished run as unread until the chat is opened.
    pub updated_at: String,
}

impl ThreadMetadata {
    pub fn new(session: Session, workspace_path: PathBuf) -> Self {
        Self {
            session,
            workspace_path,
            status: RunStatus::Idle,
            compaction_active: false,
            retry_active: false,
            retry_attempt: None,
            queued_messages: vec![],
            config: ModelConfig::default(),
            session_file: None,
            message_count: 0,
            updated_at: timestamp(std::time::SystemTime::now()),
        }
    }
}

/// RFC 3339 UTC with milliseconds, the form JavaScript's `Date.parse` accepts.
pub(super) fn timestamp(time: std::time::SystemTime) -> String {
    let ms = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let (days, of_day) = (ms / 86_400_000, ms % 86_400_000);
    // Howard Hinnant's civil_from_days, restricted to dates after 1970.
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct CreateThread {
    pub workspace_path: Option<PathBuf>,
    pub title: Option<String>,
    pub initial_message: Option<Prompt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SendMessage {
    pub input: Prompt,
}

/// Read the prototype's flat metadata once, preserving IDs, titles and archive
/// flags. Its runtime fields are intentionally not copied into the Session.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LegacyThread {
    pub id: SessionId,
    pub title: String,
    pub workspace_path: PathBuf,
    #[serde(default)]
    pub archived: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    #[test]
    fn host_ui_replies_match_one_shape_and_the_pending_request() {
        let request: HostUiRequest = from_value(
            json!({"id":"pick","kind":"select","title":"Choose","options":["one","two"]}),
        )
        .unwrap();
        for reply in [
            json!({"requestId":"pick","dismissed":false}),
            json!({"requestId":"pick","confirmed":true,"value":"one"}),
            json!({"requestId":"pick"}),
        ] {
            assert!(from_value::<HostUiResponse>(reply).is_err());
        }
        for reply in [
            json!({"requestId":"pick","value":"three"}),
            json!({"requestId":"pick","confirmed":true}),
        ] {
            assert!(from_value::<HostUiResponse>(reply)
                .unwrap()
                .resolve(&request)
                .is_err());
        }
        let reply = from_value::<HostUiResponse>(json!({"requestId":"pick","value":"one"}))
            .unwrap()
            .resolve(&request)
            .unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), json!({"value":"one"}));
        assert!(
            from_value::<HostUiRequest>(json!({"id":"pick","kind":"select","title":"Choose"}))
                .is_err()
        );
    }

    #[test]
    fn selected_model_preserves_the_assistant_ui_wire_format() {
        let config = ModelConfig {
            selection: Some(
                from_value(json!({"provider":"custom","modelId":"new-model"})).unwrap(),
            ),
            thinking_level: Some(ThinkingLevel::Low),
        };
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            json!({"provider":"custom","modelId":"new-model","thinkingLevel":"low"})
        );
        assert_eq!(
            serde_json::to_value(ModelConfig::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn timestamps_match_javascript_iso_strings() {
        // Expected values from `new Date(ms).toISOString()`, across leap days and a century.
        for (ms, expected) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_709_208_000_000, "2024-02-29T12:00:00.000Z"),
            (1_792_700_886_123, "2026-10-22T20:28:06.123Z"),
            (4_107_542_399_999, "2100-02-28T23:59:59.999Z"),
        ] {
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);
            assert_eq!(timestamp(time), expected);
        }
    }
}
