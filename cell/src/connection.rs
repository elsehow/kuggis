//! Cell-local MCP connections. Permission to reach a host is a separate grant.
//! Configuration references credentials; secret updates are write-only.
use crate::{Host, Url};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionName {
    BigBrain,
    Portal,
}
impl ConnectionName {
    pub const ALL: [Self; 2] = [Self::BigBrain, Self::Portal];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BigBrain => "bigbrain",
            Self::Portal => "portal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(type = "string")]
#[serde(try_from = "String", into = "String")]
pub struct McpEndpoint(Url);
impl TryFrom<String> for McpEndpoint {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        let url: Url = value
            .parse()
            .map_err(|e| format!("Invalid connection URL: {e}"))?;
        let uri: axum::http::Uri = url.as_str().parse().map_err(|_| "Invalid connection URL")?;
        if value.contains('#') || ![443, 8443].contains(&uri.port_u16().unwrap_or(443)) {
            return Err("Use HTTPS on port 443 or 8443 without a fragment".into());
        }
        Ok(Self(url))
    }
}
impl From<McpEndpoint> for String {
    fn from(value: McpEndpoint) -> Self {
        value.0.to_string()
    }
}
impl McpEndpoint {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    pub fn host(&self) -> Host {
        self.0.host()
    }
}

#[derive(Clone, Deserialize, ts_rs::TS)]
#[serde(try_from = "String")]
#[ts(type = "string")]
pub struct BearerToken(String);
impl TryFrom<String> for BearerToken {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 8192
            || value.chars().any(char::is_control)
            || value.contains("${")
            || value.starts_with('!')
        {
            return Err("Enter a nonempty literal bearer token".into());
        }
        Ok(Self(value))
    }
}
impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken([redacted])")
    }
}
impl BearerToken {
    pub(crate) fn authorization(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretUpdate {
    Keep {},
    Clear {},
    Replace { value: BearerToken },
}

impl Default for SecretUpdate {
    fn default() -> Self {
        Self::Keep {}
    }
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ConnectionUpdate {
    pub name: ConnectionName,
    pub url: McpEndpoint,
    #[serde(default)]
    pub credential: SecretUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "Vec<ConnectionUpdate>")]
pub struct ConnectionChanges(Vec<ConnectionUpdate>);
impl ConnectionChanges {
    pub fn into_entries(self) -> Vec<ConnectionUpdate> {
        self.0
    }
}
impl TryFrom<Vec<ConnectionUpdate>> for ConnectionChanges {
    type Error = String;
    fn try_from(entries: Vec<ConnectionUpdate>) -> Result<Self, String> {
        let mut names = BTreeSet::new();
        if entries.iter().any(|c| !names.insert(c.name)) {
            return Err("Connection names must be unique".into());
        }
        Ok(Self(entries))
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ConnectionAuth {
    None,
    /// A reference to this cell's saved credential, never its value.
    Bearer {
        credential: ConnectionName,
    },
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ConnectionConfig {
    pub name: ConnectionName,
    pub url: McpEndpoint,
    pub auth: ConnectionAuth,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_and_updates_are_validated_at_the_boundary() {
        for url in [
            "http://tools.example/mcp",
            "https://user:pass@tools.example/mcp",
            "https://127.0.0.1/mcp",
            "https://tools.example:8080/mcp",
            "https://tools.example/mcp#ignored",
        ] {
            assert!(McpEndpoint::try_from(url.to_owned()).is_err(), "{url}");
        }
        assert!(McpEndpoint::try_from("https://tools.example:8443/mcp".to_owned()).is_ok());
        assert!(
            serde_json::from_str::<SecretUpdate>(r#"{"operation":"replace","value":""}"#).is_err()
        );
        assert!(
            serde_json::from_str::<SecretUpdate>(r#"{"operation":"clear","value":"secret"}"#)
                .is_err()
        );
        let duplicate = r#"[{"name":"portal","url":"https://tools.example/mcp"},{"name":"portal","url":"https://tools.example/other"}]"#;
        assert!(serde_json::from_str::<ConnectionChanges>(duplicate).is_err());
        let token = BearerToken::try_from("secret-fixture".to_owned()).unwrap();
        assert!(!format!("{token:?}").contains("secret-fixture"));
    }
}
