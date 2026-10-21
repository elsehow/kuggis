//! The Pi wire shapes Kuggis consumes. Provider-specific message bodies remain
//! opaque, while model selection and command fields are validated by Serde.
use crate::session::{Image, StreamingBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantDelta {
    #[serde(rename = "type")]
    pub kind: String,
    pub content_index: Option<u64>,
    pub delta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SelectionFields", rename_all = "camelCase")]
pub(crate) struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionFields {
    provider: String,
    #[serde(alias = "id")]
    model_id: String,
}
impl TryFrom<SelectionFields> for ModelSelection {
    type Error = &'static str;
    fn try_from(fields: SelectionFields) -> Result<Self, Self::Error> {
        if fields.provider.trim().is_empty() || fields.model_id.trim().is_empty() {
            return Err("Model selection requires a provider and model ID");
        }
        Ok(Self {
            provider: fields.provider,
            model_id: fields.model_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeState {
    pub model: Option<ModelSelection>,
    pub thinking_level: Option<ThinkingLevel>,
    pub is_streaming: bool,
    pub session_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CatalogModel {
    #[serde(flatten)]
    pub selection: ModelSelection,
    pub name: Option<String>,
    pub reasoning: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ModelCatalog {
    pub models: Vec<CatalogModel>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Command<'a> {
    GetState,
    GetEntries,
    GetAvailableModels,
    ClearQueue,
    Abort,
    Prompt {
        message: &'a str,
        #[serde(rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
        #[serde(skip_serializing_if = "<[Image]>::is_empty")]
        images: &'a [Image],
    },
    SetModel {
        #[serde(flatten)]
        selection: &'a ModelSelection,
    },
    SetThinkingLevel {
        level: ThinkingLevel,
    },
}

#[derive(Serialize)]
pub(super) struct CommandEnvelope<'a> {
    pub id: String,
    #[serde(flatten)]
    pub command: Command<'a>,
}

#[derive(Deserialize)]
pub(super) struct Response {
    pub id: String,
    pub success: bool,
    #[serde(default)]
    pub data: Value,
    pub error: Option<String>,
}

/// Literal true: a dismissal cannot accidentally be submitted as false.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "bool", into = "bool")]
pub(crate) struct True;
impl TryFrom<bool> for True {
    type Error = &'static str;
    fn try_from(value: bool) -> Result<Self, Self::Error> {
        if value {
            Ok(Self)
        } else {
            Err("expected true")
        }
    }
}
impl From<True> for bool {
    fn from(_: True) -> bool {
        true
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum HostReply {
    Cancelled { cancelled: True },
    Confirm { confirmed: bool },
    Value { value: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Notification<'a> {
    ExtensionUiResponse {
        id: &'a str,
        #[serde(flatten)]
        reply: HostReply,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_selection_is_complete_and_extensible() {
        for model in [
            json!({}),
            json!({"provider":"custom"}),
            json!({"id":"x"}),
            json!({"provider":"", "id":"x"}),
            json!({"provider":"custom", "id":null}),
        ] {
            assert!(serde_json::from_value::<RuntimeState>(
                json!({"model":model,"isStreaming":false})
            )
            .is_err());
        }
        let state: RuntimeState = serde_json::from_value(json!({"model":{"provider":"custom-provider","id":"new-model","extra":"ignored"},"isStreaming":false})).unwrap();
        assert_eq!(state.model.unwrap().model_id, "new-model");
        assert!(
            serde_json::from_value::<RuntimeState>(json!({"model":null,"isStreaming":false}))
                .unwrap()
                .model
                .is_none()
        );
        assert!(serde_json::from_value::<RuntimeState>(json!({"model":null})).is_err());
        assert!(
            serde_json::from_value::<ModelCatalog>(json!({"models":[{"provider":"custom"}]}))
                .is_err()
        );
    }
}
