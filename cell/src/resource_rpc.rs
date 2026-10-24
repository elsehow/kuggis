//! Trusted operator-only resource RPC. Not registered on public HTTP surfaces.
//! SSH runs this command on a configured publisher; stdin is bounded JSON.
use crate::publish::registry::{
    resources::{CleanupPlan, CleanupReceipt, ResourceInventory},
    Registry,
};
use crate::resource::*;
use crate::{Hash, RequestId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const MAX_RPC_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    ReconcilePublishing {
        provider: ProviderRef,
        endpoint: crate::Url,
        cells: Vec<crate::publish::provision::PolicyCell>,
    },
    Inventory,
    PlanExternal {
        registration: ExternalRegistration,
    },
    OperationReceipt {
        operation: RequestId,
    },
    Plan {
        resource: ResourceId,
        intent: ResourceIntent,
    },
    Apply {
        operation: RequestId,
        plan: ResourcePlan,
        approval: Hash,
        #[serde(default)]
        acknowledge_manual: bool,
    },
    PlanCleanup {
        cell: CellIdentity,
        choices: BTreeMap<ResourceId, ResourceOwner>,
    },
    ApplyCleanup {
        operation: RequestId,
        plan: CleanupPlan,
        approval: Hash,
        acknowledge_manual: bool,
    },
    CleanupReceipt {
        operation: RequestId,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Publishing(BTreeMap<crate::Name, crate::publish::provision::Discovery>),
    Refused { conflict: bool, message: String },
    Inventory(ResourceInventory),
    OperationReceipt(Option<ResourceOperation>),
    Plan(ResourcePlan),
    Applied(ResourceOperation),
    CleanupPlan(CleanupPlan),
    Cleaned(CleanupReceipt),
    CleanupReceipt(Option<CleanupReceipt>),
}
pub fn execute(
    registry: &Registry,
    command: Command,
) -> Result<Reply, crate::publish::registry::RegistryError> {
    Ok(match command {
        Command::ReconcilePublishing {
            provider,
            endpoint,
            cells,
        } => Reply::Publishing(registry.reconcile_publishing(provider, endpoint, cells)?),
        Command::Inventory => Reply::Inventory(registry.resources()?),
        Command::PlanExternal { registration } => {
            Reply::Plan(registry.plan_external(registration)?)
        }
        Command::OperationReceipt { operation } => {
            Reply::OperationReceipt(registry.resource_operation(operation)?)
        }
        Command::Plan { resource, intent } => {
            Reply::Plan(registry.plan_resource(resource, intent)?)
        }
        Command::Apply {
            operation,
            plan,
            approval,
            acknowledge_manual,
        } => Reply::Applied(registry.apply_resource_acknowledged(
            operation,
            &plan,
            approval,
            acknowledge_manual,
        )?),
        Command::PlanCleanup { cell, choices } => {
            Reply::CleanupPlan(registry.plan_cleanup(cell, &choices)?)
        }
        Command::ApplyCleanup {
            operation,
            plan,
            approval,
            acknowledge_manual,
        } => Reply::Cleaned(registry.apply_cleanup(
            operation,
            &plan,
            approval,
            acknowledge_manual,
        )?),
        Command::CleanupReceipt { operation } => {
            Reply::CleanupReceipt(registry.cleanup_receipt(operation)?)
        }
    })
}

/// Preserve conflict-vs-unavailable recovery across SSH without trusting stderr.
pub fn execute_wire(registry: &Registry, command: Command) -> Reply {
    execute(registry, command).unwrap_or_else(|error| {
        use crate::publish::registry::RegistryError;
        let conflict = matches!(
            error,
            RegistryError::InvalidPolicy(_)
                | RegistryError::Denied
                | RegistryError::UnknownSite
                | RegistryError::UnknownNamespace
        );
        Reply::Refused {
            conflict,
            message: error.to_string(),
        }
    })
}
