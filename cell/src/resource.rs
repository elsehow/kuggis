//! Provider-independent resource identity and lifecycle contracts. Values in a
//! request are not authority. Trusted adapters observe state, validate plans,
//! authenticate actors and execute approved operations.
use crate::{canon, Hash, Key, RequestId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
pub struct ResourceId(pub Hash);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
pub struct ProviderRef(pub RequestId);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
pub struct CellIdentity(#[ts(type = "string")] pub Key);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
pub struct CredentialRef(pub RequestId);

/// Opaque provider-assigned identity, never a command, URL to follow, or secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct ExternalResourceId(String);
impl ExternalResourceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ExternalResourceId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 256
            || value.bytes().any(|b| !(0x21..=0x7e).contains(&b))
        {
            return Err(
                "external resource ID must be 1–256 printable ASCII bytes without whitespace"
                    .into(),
            );
        }
        Ok(Self(value))
    }
}
impl From<ExternalResourceId> for String {
    fn from(v: ExternalResourceId) -> Self {
        v.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Site,
    Compute,
    Storage,
    Job,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ResourceManagement {
    Managed,
    External,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceOwner {
    Cell(CellIdentity),
    Operator,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum OwnerDeletionPolicy {
    RetainForOperator,
    RequireDecision,
    Destroy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceState {
    Published { version: RequestId },
    Unpublished,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AccessScope {
    Use,
    ManageSite,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResourceAttachment {
    pub cell: CellIdentity,
    pub scope: AccessScope,
    pub credential: Option<CredentialRef>,
}
/// Operator-verified provider/account labels, not credentials or URLs to follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ExternalAccount {
    pub provider: ExternalResourceId,
    pub account: ExternalResourceId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ExternalRegistration {
    pub account: ExternalAccount,
    pub kind: ResourceKind,
    pub external_id: ExternalResourceId,
}
impl ExternalRegistration {
    pub fn record(self, provider: ProviderRef) -> ResourceRecord {
        ResourceRecord {
            id: ResourceId::external(provider, self.kind, &self.account, &self.external_id),
            provider,
            external_id: self.external_id,
            external_account: Some(self.account),
            kind: self.kind,
            owner: ResourceOwner::Operator,
            management: ResourceManagement::External,
            state: ResourceState::Unknown,
            deletion_policy: OwnerDeletionPolicy::RetainForOperator,
            attachments: vec![],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResourceRecord {
    pub id: ResourceId,
    pub provider: ProviderRef,
    pub external_id: ExternalResourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_account: Option<ExternalAccount>,
    pub kind: ResourceKind,
    pub owner: ResourceOwner,
    pub management: ResourceManagement,
    pub state: ResourceState,
    pub deletion_policy: OwnerDeletionPolicy,
    pub attachments: Vec<ResourceAttachment>,
}
impl ResourceId {
    pub fn external(
        provider: ProviderRef,
        kind: ResourceKind,
        account: &ExternalAccount,
        external: &ExternalResourceId,
    ) -> Self {
        Self(Hash::of(&canon::bytes(&(
            "external-resource-v1",
            provider,
            kind,
            account,
            external,
        ))))
    }
    /// Provider/account scope and kind are part of identity. Same-named remote
    /// resources in different accounts cannot alias. Ownership is NOT identity.
    pub fn derive(
        provider: ProviderRef,
        kind: ResourceKind,
        external: &ExternalResourceId,
    ) -> Self {
        Self(Hash::of(&canon::bytes(&(provider, kind, external))))
    }
}
impl ResourceRecord {
    pub fn revision(&self) -> Hash {
        Hash::of(&canon::bytes(self))
    }
    pub fn validate(&self) -> Result<(), String> {
        let expected = match &self.external_account {
            Some(account) if self.management == ResourceManagement::External => {
                ResourceId::external(self.provider, self.kind, account, &self.external_id)
            }
            Some(_) => {
                return Err("managed resources cannot claim external account metadata".into())
            }
            None => ResourceId::derive(self.provider, self.kind, &self.external_id),
        };
        if self.id != expected {
            return Err("resource identity does not match provider/kind/external ID".into());
        }
        if self.management == ResourceManagement::External
            && (self.state != ResourceState::Unknown
                || self.deletion_policy == OwnerDeletionPolicy::Destroy)
        {
            return Err("external resources have unknown provider state and cannot promise automatic destruction".into());
        }
        if self.management == ResourceManagement::Managed
            && self.deletion_policy == OwnerDeletionPolicy::Destroy
        {
            return Err("no destruction adapter is installed".into());
        }
        if self.management == ResourceManagement::Managed && self.kind != ResourceKind::Site {
            return Err("no managed adapter is installed for this resource kind".into());
        }
        if self.kind == ResourceKind::Site
            && self.management == ResourceManagement::Managed
            && self.state == ResourceState::Unknown
        {
            return Err("managed site visibility must be explicit".into());
        }
        for (index, attachment) in self.attachments.iter().enumerate() {
            if self.attachments[..index]
                .iter()
                .any(|a| a.cell == attachment.cell)
            {
                return Err("duplicate cell attachment".into());
            }
            if self.management == ResourceManagement::External
                && attachment.scope != AccessScope::Use
            {
                return Err("external resources support use attachments only".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TransferVisibility {
    KeepCurrent,
    Unpublish,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceIntent {
    RegisterExternal,
    Attach(ResourceAttachment),
    Unpublish,
    Transfer {
        to: ResourceOwner,
        visibility: TransferVisibility,
    },
    Detach {
        cell: CellIdentity,
    },
    Destroy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEffect {
    MetadataRecorded,
    LocalAccessRecorded,
    ProviderAccessRequiresManualAction,
    PublicAccessStopped,
    DataRetained,
    OwnershipTransferred,
    LocalAccessRemoved,
    RemoteRevocationRequiresManualAction,
    CostsMayContinue,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResourcePlan {
    pub resource: ResourceRecord,
    pub expected_revision: Hash,
    pub intent: ResourceIntent,
    pub effects: Vec<ResourceEffect>,
}
impl ResourcePlan {
    pub fn new(resource: ResourceRecord, intent: ResourceIntent) -> Result<Self, String> {
        resource.validate()?;
        let external = resource.management == ResourceManagement::External;
        let effects = match &intent {
            ResourceIntent::RegisterExternal
                if external
                    && resource.external_account.is_some()
                    && resource.owner == ResourceOwner::Operator
                    && resource.attachments.is_empty() =>
            {
                vec![
                    ResourceEffect::MetadataRecorded,
                    ResourceEffect::CostsMayContinue,
                ]
            }
            ResourceIntent::Attach(attachment)
                if external
                    && attachment.scope == AccessScope::Use
                    && resource.attachments.len() < 64
                    && !resource
                        .attachments
                        .iter()
                        .any(|a| a.cell == attachment.cell) =>
            {
                vec![
                    ResourceEffect::LocalAccessRecorded,
                    ResourceEffect::ProviderAccessRequiresManualAction,
                    ResourceEffect::CostsMayContinue,
                ]
            }
            ResourceIntent::Unpublish if !external && resource.kind == ResourceKind::Site => vec![
                ResourceEffect::PublicAccessStopped,
                ResourceEffect::DataRetained,
            ],
            ResourceIntent::Transfer { to, visibility } if *to != resource.owner => {
                if external && *visibility == TransferVisibility::Unpublish {
                    return Err("external effects cannot be stopped by this adapter".into());
                }
                let mut effects = vec![
                    ResourceEffect::OwnershipTransferred,
                    ResourceEffect::LocalAccessRemoved,
                    ResourceEffect::DataRetained,
                ];
                if *visibility == TransferVisibility::Unpublish {
                    effects.push(ResourceEffect::PublicAccessStopped);
                }
                if external {
                    effects.extend([
                        ResourceEffect::RemoteRevocationRequiresManualAction,
                        ResourceEffect::CostsMayContinue,
                    ]);
                }
                effects
            }
            ResourceIntent::Detach { cell }
                if resource.attachments.iter().any(|a| &a.cell == cell) =>
            {
                let mut effects = vec![ResourceEffect::LocalAccessRemoved];
                if external {
                    effects.extend([
                        ResourceEffect::RemoteRevocationRequiresManualAction,
                        ResourceEffect::CostsMayContinue,
                    ]);
                }
                effects
            }
            _ => return Err("unsupported resource lifecycle operation".into()),
        };
        Ok(Self {
            expected_revision: if intent == ResourceIntent::RegisterExternal {
                Hash::ZERO
            } else {
                resource.revision()
            },
            resource,
            intent,
            effects,
        })
    }
    pub fn approval(&self) -> Hash {
        Hash::of(&canon::bytes(self))
    }
    /// Reconstruct effects rather than trusting caller-declared capability/effect
    /// lists. Revision checks belong in the adapter's commit transaction too.
    pub fn validate(&self) -> Result<(), String> {
        if Self::new(self.resource.clone(), self.intent.clone())? != *self {
            return Err("resource plan does not match observed state/effects".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationPhase {
    Pending,
    Running,
    Succeeded,
    Failed { message: String },
    OutcomeUnknown { message: String },
    ManualActionRequired { obligations: Vec<String> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResourceOperation {
    pub id: RequestId,
    pub resource: ResourceId,
    pub approval: Hash,
    pub phase: OperationPhase,
}
#[cfg(test)]
mod tests {
    use super::*;
    pub fn sample() -> ResourceRecord {
        let provider = ProviderRef(RequestId::generate());
        let external = ExternalResourceId::try_from("namespace/demo".to_string()).unwrap();
        ResourceRecord {
            id: ResourceId::derive(provider, ResourceKind::Site, &external),
            provider,
            external_id: external,
            external_account: None,
            kind: ResourceKind::Site,
            owner: ResourceOwner::Cell(CellIdentity(crate::Secret::generate().public())),
            management: ResourceManagement::Managed,
            state: ResourceState::Unpublished,
            deletion_policy: OwnerDeletionPolicy::RetainForOperator,
            attachments: vec![],
        }
    }
    #[test]
    fn identity_is_scoped_and_ownership_is_not_identity() {
        let mut r = sample();
        let id = r.id;
        r.owner = ResourceOwner::Operator;
        assert_eq!(id, r.id);
        r.validate().unwrap();
        assert_ne!(
            id,
            ResourceId::derive(ProviderRef(RequestId::generate()), r.kind, &r.external_id)
        );
        assert_ne!(
            id,
            ResourceId::derive(r.provider, ResourceKind::Storage, &r.external_id)
        );
        r.id = ResourceId(Hash::of(b"forged"));
        assert!(r.validate().is_err());
    }
    #[test]
    fn identifiers_and_wire_shape_are_strict() {
        for value in ["", "a b", "a\nb", "é"] {
            assert!(
                serde_json::from_value::<ExternalResourceId>(serde_json::json!(value)).is_err()
            );
        }
        assert!(ExternalResourceId::try_from("x".repeat(257)).is_err());
        let r = sample();
        let mut json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            serde_json::from_value::<ResourceRecord>(json.clone()).unwrap(),
            r
        );
        json["capabilities"] = serde_json::json!(["destroy"]);
        assert!(serde_json::from_value::<ResourceRecord>(json).is_err());
    }
    #[test]
    fn plans_bind_state_target_and_effects() {
        let r = sample();
        let plan = ResourcePlan::new(r.clone(), ResourceIntent::Unpublish).unwrap();
        plan.validate().unwrap();
        let transfer = ResourcePlan::new(
            r,
            ResourceIntent::Transfer {
                to: ResourceOwner::Operator,
                visibility: TransferVisibility::Unpublish,
            },
        )
        .unwrap();
        assert_ne!(plan.approval(), transfer.approval());
        let mut forged = plan;
        forged.effects.clear();
        assert!(forged.validate().is_err());
        let mut changed = transfer.clone();
        changed.resource.state = ResourceState::Published {
            version: RequestId::generate(),
        };
        assert_ne!(changed.approval(), transfer.approval());
        assert!(changed.validate().is_err());
    }
    #[test]
    fn external_management_never_promises_destruction_or_revocation() {
        let mut r = sample();
        r.management = ResourceManagement::External;
        r.state = ResourceState::Unknown;
        r.deletion_policy = OwnerDeletionPolicy::RequireDecision;
        let cell = CellIdentity(crate::Secret::generate().public());
        r.attachments.push(ResourceAttachment {
            cell,
            scope: AccessScope::Use,
            credential: None,
        });
        r.validate().unwrap();
        assert!(ResourcePlan::new(r.clone(), ResourceIntent::Destroy).is_err());
        assert!(ResourcePlan::new(r.clone(), ResourceIntent::Unpublish).is_err());
        let plan = ResourcePlan::new(r, ResourceIntent::Detach { cell }).unwrap();
        assert!(plan
            .effects
            .contains(&ResourceEffect::RemoteRevocationRequiresManualAction));
        assert!(plan.effects.contains(&ResourceEffect::CostsMayContinue));
    }
}
