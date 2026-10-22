//! Independent observations: a saved credential, resolved authentication,
//! verified account information, and an inference result are different facts.
use crate::{Hash, Provider};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    #[default]
    Oauth,
    ApiKey,
}
impl LoginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Oauth => "oauth",
            Self::ApiKey => "api_key",
        }
    }
}
impl Provider {
    pub fn login_methods(self) -> Vec<LoginMethod> {
        if self.oauth_id().is_some() {
            vec![LoginMethod::Oauth, LoginMethod::ApiKey]
        } else {
            vec![LoginMethod::ApiKey]
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Oauth,
    ApiKey,
    ManagedEdge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CredentialState {
    Missing,
    Stored { method: AuthMethod },
    Expired { expires_at: u64 },
    Invalid,
}
impl CredentialState {
    pub fn from_pi(value: &Value, at: u64) -> Self {
        if value.is_null() {
            return Self::Missing;
        }
        let present = |field| value[field].as_str().is_some_and(|s| !s.is_empty());
        match value["type"].as_str() {
            Some("oauth") if present("access") && present("refresh") => {
                match value["expires"].as_u64() {
                    Some(expires_at) if expires_at <= at => Self::Expired { expires_at },
                    Some(_) => Self::Stored {
                        method: AuthMethod::Oauth,
                    },
                    None => Self::Invalid,
                }
            }
            Some("api_key") if present("key") => Self::Stored {
                method: AuthMethod::ApiKey,
            },
            _ => Self::Invalid,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Authentication {
    #[default]
    Unchecked,
    /// Pi resolved a credential. This does not assert remote inference access.
    Resolved {
        checked_at: u64,
        method: AuthMethod,
    },
    Unavailable {
        checked_at: u64,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct AccountIdentity {
    pub account: Hash,
    pub organization: Hash,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub subscription_status: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AccountObservation {
    #[default]
    Unchecked,
    Verified {
        checked_at: u64,
        identity: AccountIdentity,
    },
    Unavailable {
        checked_at: u64,
        message: String,
    },
}
impl AccountIdentity {
    pub fn from_anthropic(value: &Value) -> Option<Self> {
        let account = value["account"]["uuid"]
            .as_str()
            .filter(|s| !s.is_empty())?;
        let organization = value["organization"]["uuid"]
            .as_str()
            .filter(|s| !s.is_empty())?;
        Some(Self {
            account: Hash::of(account.as_bytes()),
            organization: Hash::of(organization.as_bytes()),
            email: value["account"]["email"].as_str().map(str::to_owned),
            plan: value["organization"]["organization_type"]
                .as_str()
                .map(str::to_owned),
            subscription_status: value["organization"]["subscription_status"]
                .as_str()
                .map(str::to_owned),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureKind {
    Authentication,
    ExtraUsage,
    UsageLimit,
    RateLimited,
    Unavailable,
    Other,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ProviderFailure {
    pub kind: ProviderFailureKind,
    pub message: String,
}
impl ProviderFailure {
    /// Pi exposes provider errors as text. Preserve it, classify only explicit
    /// reports, and never infer an account's subscription entitlement from it.
    pub fn from_message(message: String) -> Self {
        let lower = message.to_ascii_lowercase();
        let kind = if lower.contains("extra usage") {
            ProviderFailureKind::ExtraUsage
        } else if lower.contains("authentication_error")
            || lower.contains("invalid_api_key")
            || lower.contains("unauthorized")
        {
            ProviderFailureKind::Authentication
        } else if lower.contains("rate_limit") || lower.contains("rate limit") {
            ProviderFailureKind::RateLimited
        } else if lower.contains("usage limit") || lower.contains("usage exhausted") {
            ProviderFailureKind::UsageLimit
        } else if lower.contains("overloaded_error") || lower.contains("temporarily unavailable") {
            ProviderFailureKind::Unavailable
        } else {
            ProviderFailureKind::Other
        };
        Self { kind, message }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RequestOutcome {
    Succeeded,
    Failed { failure: ProviderFailure },
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct RequestObservation {
    pub observed_at: u64,
    pub model: String,
    pub outcome: RequestOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderObservations {
    /// Observations of one credential must never be relabelled after sign-in.
    pub credential: Hash,
    pub authentication: Authentication,
    pub account: AccountObservation,
    pub last_request: Option<RequestObservation>,
}
impl ProviderObservations {
    pub fn new(credential: Hash) -> Self {
        Self {
            credential,
            authentication: Authentication::Unchecked,
            account: AccountObservation::Unchecked,
            last_request: None,
        }
    }
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ProviderConnection {
    pub id: Provider,
    pub login_methods: Vec<LoginMethod>,
    pub granted: bool,
    pub credential: CredentialState,
    pub authentication: Authentication,
    pub account: AccountObservation,
    pub last_request: Option<RequestObservation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn saved_credentials_are_not_verified_authentication() {
        assert_eq!(
            CredentialState::from_pi(&json!({"type":"oauth"}), 0),
            CredentialState::Invalid
        );
        let auth = json!({"type":"oauth","access":"fixture","refresh":"fixture","expires":10});
        assert_eq!(
            CredentialState::from_pi(&auth, 10),
            CredentialState::Expired { expires_at: 10 }
        );
        assert_eq!(
            CredentialState::from_pi(&auth, 9),
            CredentialState::Stored {
                method: AuthMethod::Oauth
            }
        );
        assert!(matches!(
            ProviderObservations::new(Hash::ZERO).authentication,
            Authentication::Unchecked
        ));
    }
    #[test]
    fn extra_usage_does_not_mean_subscription_exhaustion() {
        assert_eq!(
            ProviderFailure::from_message("You're out of extra usage".into()).kind,
            ProviderFailureKind::ExtraUsage
        );
        assert_eq!(
            ProviderFailure::from_message("unfamiliar provider reply".into()).kind,
            ProviderFailureKind::Other
        );
        assert!(AccountIdentity::from_anthropic(&json!({"account":{"uuid":"a"}})).is_none());
    }
}
