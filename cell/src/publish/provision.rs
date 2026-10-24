//! Trusted operator provisioning of the existing publisher and site resources.
//! The complete validated tree and verified pins are required; no agent RPC can
//! allocate authority. Enabling reconciliation declares this tree authoritative.
use super::bridge::Config;
use crate::{Cell, Key, Name};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCell {
    pub cell: Cell,
    pub parent: Option<Name>,
    pub key: Key,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub config: Option<Config>,
    pub sites: Vec<super::registry::resources::SiteView>,
    pub error: Option<String>,
}
impl Discovery {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            config: None,
            sites: vec![],
            error: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_installation_retries_checks_identity_and_preserves_running_chats() {
        let output = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/publishing_install.py"
            ))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
