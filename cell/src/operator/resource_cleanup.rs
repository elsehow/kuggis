//! Resource coverage bound into cell deletion approval. The provider commits
//! cleanup before the VM can be removed; persisted receipts survive retries.
use super::resources::{self, Backend};
use super::*;
use crate::publish::{
    registry::resources::{CleanupPlan, CleanupReceipt},
    NamespaceId, PublishScope,
};
use crate::{
    resource::*,
    resource_rpc::{Command as Rpc, Reply},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub backend: Backend,
    pub operation: RequestId,
    pub plan: CleanupPlan,
    pub namespaces: Vec<NamespaceId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Coverage {
    pub connections: Hash,
    pub identity: Option<CellIdentity>,
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub(crate) struct CleanupView {
    pub backend: Name,
    pub operation: RequestId,
    pub plan: CleanupPlan,
    pub receipt: Option<CleanupReceipt>,
}
impl Coverage {
    pub fn view(&self, done: &BTreeMap<RequestId, CleanupReceipt>) -> Vec<CleanupView> {
        self.bindings
            .iter()
            .map(|b| CleanupView {
                backend: b.backend.name.clone(),
                operation: b.operation,
                plan: b.plan.clone(),
                receipt: done.get(&b.operation).cloned(),
            })
            .collect()
    }
    pub fn manual(&self) -> bool {
        self.bindings.iter().any(|b| {
            b.plan.changes.iter().any(|p| {
                p.effects
                    .contains(&ResourceEffect::RemoteRevocationRequiresManualAction)
            })
        })
    }
}
fn receipt(binding: &Binding, value: &CleanupReceipt) -> Result<()> {
    if value.operation != binding.operation
        || value.approval != binding.plan.approval()
        || value.cell != binding.plan.cell
        || value.resources
            != binding
                .plan
                .changes
                .iter()
                .map(|p| p.resource.id)
                .collect::<Vec<_>>()
    {
        return Err(fail(
            "Resource cleanup receipt does not match the approved operation",
        ));
    }
    Ok(())
}
pub(super) fn prepare(
    op: &Operator,
    cell: &Cell,
    choices: &BTreeMap<ResourceId, ResourceOwner>,
    prior: Option<&Coverage>,
    done: &mut BTreeMap<RequestId, CleanupReceipt>,
) -> Result<Coverage> {
    let backends = resources::registered(op)?;
    let connections = Hash::of(&crate::canon::bytes(&backends));
    let identity = match fs::read_to_string(
        op.config
            .tree
            .join(".pins")
            .join(format!("{}.pub", cell.id)),
    ) {
        Ok(key) => Some(CellIdentity(key.parse().map_err(bad)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(fail(e)),
    };
    let publish = cell
        .grants
        .iter()
        .filter_map(|g| match g {
            crate::Grant::Publish(PublishScope::Namespace(ns)) => Some(ns.clone()),
            crate::Grant::Publish(PublishScope::Site(site)) => Some(site.namespace.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if identity.is_none() && (!backends.is_empty() || !publish.is_empty()) {
        return Err(bad(
            "A verified cell pin is required to check resource ownership before deletion",
        ));
    }
    let mut bindings = vec![];
    let mut remaining = choices.clone();
    for backend in backends {
        let identity = identity.expect("checked identity for resource backends");
        if let Some(old) = prior.and_then(|c| {
            c.bindings
                .iter()
                .find(|b| b.backend == backend && b.plan.cell == identity)
        }) {
            if !done.contains_key(&old.operation) {
                backend.inventory()?;
                match backend.rpc(Rpc::CleanupReceipt {
                    operation: old.operation,
                })? {
                    Reply::CleanupReceipt(Some(value)) => {
                        receipt(old, &value)?;
                        done.insert(old.operation, value);
                    }
                    Reply::CleanupReceipt(None) => (),
                    _ => return Err(fail("Unexpected cleanup receipt response")),
                }
            }
            if let Some(done) = done.get(&old.operation) {
                receipt(old, done)?;
                for change in &old.plan.changes {
                    if let Some(choice) = remaining.remove(&change.resource.id) {
                        if !matches!(change.intent,ResourceIntent::Transfer{to,visibility:TransferVisibility::KeepCurrent} if to==choice)
                        {
                            return Err(bad("Completed cleanup cannot be changed by a deletion review; use Resources"));
                        }
                    }
                }
                bindings.push(old.clone());
                continue;
            }
        }
        let inventory = backend.inventory()?;
        resources::remember(op, &backend)?;
        let mut local = BTreeMap::new();
        for resource in &inventory.resources {
            if let Some(target) = remaining.remove(&resource.id) {
                resources::validate_target(
                    op,
                    &ResourceIntent::Transfer {
                        to: target,
                        visibility: TransferVisibility::KeepCurrent,
                    },
                )?;
                local.insert(resource.id, target);
            }
        }
        let plan = match backend.rpc(Rpc::PlanCleanup {
            cell: identity,
            choices: local,
        })? {
            Reply::CleanupPlan(plan)
                if plan.provider == backend.provider && plan.cell == identity =>
            {
                plan
            }
            _ => return Err(fail("Unexpected cleanup plan response")),
        };
        bindings.push(Binding {
            backend,
            operation: RequestId::generate(),
            plan,
            namespaces: inventory.namespaces,
        });
    }
    if !remaining.is_empty() {
        return Err(bad("Cleanup choices contain unknown resources"));
    }
    for ns in publish {
        if !bindings.iter().any(|b| b.namespaces.contains(&ns)) {
            return Err(bad("A publishing namespace has no reachable configured resource backend; configure it before deleting this cell"));
        }
    }
    Ok(Coverage {
        connections,
        identity,
        bindings,
    })
}
pub(super) fn check(
    op: &Operator,
    coverage: Option<&Coverage>,
    done: &BTreeMap<RequestId, CleanupReceipt>,
) -> Result<()> {
    let coverage = coverage.ok_or_else(|| {
        bad("This older deletion approval has no resource coverage; review deletion again")
    })?;
    let backends = resources::registered(op)?;
    if Hash::of(&crate::canon::bytes(&backends)) != coverage.connections {
        return Err(bad("Resource connections changed; review deletion again"));
    }
    for binding in &coverage.bindings {
        if let Some(value) = done.get(&binding.operation) {
            receipt(binding, value)?;
            continue;
        }
        binding.backend.inventory()?;
        match binding.backend.rpc(Rpc::CleanupReceipt {
            operation: binding.operation,
        })? {
            Reply::CleanupReceipt(Some(value)) => receipt(binding, &value)?,
            Reply::CleanupReceipt(None) => {
                for change in &binding.plan.changes {
                    resources::validate_target(op, &change.intent)?;
                }
                let inventory = binding.backend.inventory()?;
                if inventory.revision != binding.plan.inventory_revision {
                    return Err(bad("Resource inventory changed; review deletion again"));
                }
            }
            _ => return Err(fail("Unexpected cleanup receipt response")),
        }
    }
    Ok(())
}
pub(super) fn perform(
    op: &Operator,
    binding: &Binding,
    acknowledge_manual: bool,
) -> Result<CleanupReceipt> {
    let value = match binding.backend.rpc(Rpc::CleanupReceipt {
        operation: binding.operation,
    })? {
        Reply::CleanupReceipt(Some(value)) => value,
        Reply::CleanupReceipt(None) => {
            for change in &binding.plan.changes {
                resources::validate_target(op, &change.intent)?;
            }
            binding.backend.inventory()?;
            match binding.backend.rpc(Rpc::ApplyCleanup {
                operation: binding.operation,
                plan: binding.plan.clone(),
                approval: binding.plan.approval(),
                acknowledge_manual,
            })? {
                Reply::Cleaned(value) => value,
                _ => return Err(fail("Unexpected resource cleanup response")),
            }
        }
        _ => return Err(fail("Unexpected cleanup receipt response")),
    };
    receipt(binding, &value)?;
    Ok(value)
}
