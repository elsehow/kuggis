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
    Inventory,
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
        Command::Inventory => Reply::Inventory(registry.resources()?),
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
        } => Reply::Applied(registry.apply_resource(operation, &plan, approval)?),
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
