//! Managed-site adapter for generic resource lifecycle operations. Metadata,
//! policy fencing, publication visibility and receipts commit in one registry
//! transaction. Future remote adapters must reconcile non-atomic effects.
use super::*;
use crate::resource::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResourceInventory {
    pub provider: ProviderRef,
    pub revision: Hash,
    pub resources: Vec<ResourceRecord>,
    pub frozen: Vec<CellIdentity>,
    pub namespaces: Vec<NamespaceId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CleanupPlan {
    pub provider: ProviderRef,
    pub cell: CellIdentity,
    pub inventory_revision: Hash,
    pub changes: Vec<ResourcePlan>,
}
impl CleanupPlan {
    pub fn approval(&self) -> Hash {
        Hash::of(&crate::canon::bytes(self))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CleanupReceipt {
    pub operation: RequestId,
    pub approval: Hash,
    pub cell: CellIdentity,
    pub resources: Vec<ResourceId>,
    pub manual_obligations: Vec<ResourceEffect>,
}
fn invalid(message: impl ToString) -> RegistryError {
    RegistryError::InvalidPolicy(message.to_string())
}

impl State {
    pub(super) fn resource_id(&self, site: &SiteId) -> ResourceId {
        let external = ExternalResourceId::try_from(format!("{}/{}", site.namespace.0, site.name))
            .expect("validated site ID");
        ResourceId::derive(ProviderRef(self.audience), ResourceKind::Site, &external)
    }
    fn site_for_resource(&self, id: ResourceId) -> Result<SiteId> {
        for (namespace, record) in &self.namespaces {
            for name in &record.sites {
                let site = SiteId {
                    namespace: namespace.clone(),
                    name: name.clone(),
                };
                if self.resource_id(&site) == id {
                    return Ok(site);
                }
            }
        }
        Err(RegistryError::UnknownSite)
    }
    pub(super) fn add_site_resource(&mut self, site: &SiteId) {
        let owner = self.namespaces[&site.namespace].owner;
        let record = ResourceRecord {
            id: self.resource_id(site),
            provider: ProviderRef(self.audience),
            external_id: ExternalResourceId::try_from(format!(
                "{}/{}",
                site.namespace.0, site.name
            ))
            .expect("site ID"),
            kind: ResourceKind::Site,
            owner: ResourceOwner::Cell(CellIdentity(owner)),
            management: ResourceManagement::Managed,
            state: ResourceState::Unpublished,
            deletion_policy: OwnerDeletionPolicy::RetainForOperator,
            attachments: vec![],
        };
        let id = record.id;
        self.resources.insert(id, record);
        self.refresh_attachments_for(vec![id]);
    }
    pub(super) fn migrate_resources(&mut self) {
        let sites: Vec<_> = self
            .namespaces
            .iter()
            .flat_map(|(ns, n)| {
                n.sites.iter().map(move |name| SiteId {
                    namespace: ns.clone(),
                    name: name.clone(),
                })
            })
            .collect();
        for site in sites {
            self.add_site_resource(&site);
        }
        self.sync_resource_states();
    }
    pub(super) fn sync_resource_states(&mut self) {
        for history in self.publications.values() {
            let id = self.resource_id(&history.site);
            if let Some(record) = self.resources.get_mut(&id) {
                record.state = history
                    .active
                    .map(|version| ResourceState::Published { version })
                    .unwrap_or(ResourceState::Unpublished);
            }
        }
    }
    /// Policy apply explicitly reconstructs delegated access. Namespace scope
    /// covers only sites still owned by that namespace's allocation owner;
    /// it cannot recapture a site transferred to another owner/operator.
    pub(super) fn refresh_attachments(&mut self) {
        self.refresh_attachments_for(self.resources.keys().copied().collect());
    }
    fn refresh_attachments_for(&mut self, ids: Vec<ResourceId>) {
        for id in ids {
            let Ok(site) = self.site_for_resource(id) else {
                continue;
            };
            let record = self.resources.get_mut(&id).expect("known resource");
            let namespace_owner =
                ResourceOwner::Cell(CellIdentity(self.namespaces[&site.namespace].owner));
            record.attachments = self
                .authorities
                .iter()
                .filter(|a| {
                    record.owner != ResourceOwner::Cell(CellIdentity(a.key))
                        && !self.frozen.contains(&CellIdentity(a.key))
                        && a.scopes.iter().any(|s| match s {
                            PublishScope::Site(target) => target == &site,
                            PublishScope::Namespace(ns) => {
                                ns == &site.namespace && record.owner == namespace_owner
                            }
                        })
                })
                .map(|a| ResourceAttachment {
                    cell: CellIdentity(a.key),
                    scope: AccessScope::ManageSite,
                    credential: None,
                })
                .collect();
        }
    }
    pub(super) fn validate_resources(&self) -> Result<()> {
        for (id, record) in &self.resources {
            record.validate().map_err(invalid)?;
            if id != &record.id {
                return Err(invalid("resource index mismatch"));
            }
            if record.management == ResourceManagement::Managed {
                let site = self.site_for_resource(*id)?;
                let active = self
                    .publications
                    .get(&Hash::of(&crate::canon::bytes(&site)))
                    .and_then(|h| h.active);
                let expected = active
                    .map(|version| ResourceState::Published { version })
                    .unwrap_or(ResourceState::Unpublished);
                if record.state != expected {
                    return Err(invalid("site resource visibility mismatch"));
                }
            }
        }
        for (namespace, n) in &self.namespaces {
            for name in &n.sites {
                if !self.resources.contains_key(&self.resource_id(&SiteId {
                    namespace: namespace.clone(),
                    name: name.clone(),
                })) {
                    return Err(invalid("missing site resource record"));
                }
            }
        }
        Ok(())
    }
    fn inventory(&self) -> ResourceInventory {
        ResourceInventory {
            provider: ProviderRef(self.audience),
            revision: Hash::of(&crate::canon::bytes(&(
                &self.resources,
                &self.frozen,
                &self.namespaces,
                &self.authorities,
            ))),
            resources: self.resources.values().cloned().collect(),
            frozen: self.frozen.clone(),
            namespaces: self.namespaces.keys().cloned().collect(),
        }
    }
    pub(super) fn effective_site_access(&self, caller: Key, site: &SiteId) -> bool {
        self.resources
            .get(&self.resource_id(site))
            .is_some_and(|r| {
                r.owner == ResourceOwner::Cell(CellIdentity(caller))
                    || r.attachments.iter().any(|a| {
                        a.cell == CellIdentity(caller) && a.scope == AccessScope::ManageSite
                    })
            })
    }
    fn apply_resource_change(&mut self, plan: &ResourcePlan) -> Result<()> {
        plan.validate().map_err(invalid)?;
        if matches!(plan.intent, ResourceIntent::Transfer { to: ResourceOwner::Cell(cell), .. } if self.frozen.contains(&cell))
        {
            return Err(invalid("cannot transfer to a frozen identity"));
        }
        let current = self
            .resources
            .get(&plan.resource.id)
            .ok_or(RegistryError::UnknownSite)?;
        if current.revision() != plan.expected_revision {
            return Err(invalid("resource changed; review again"));
        }
        // All currently managed effects are local to this transaction. External
        // provider operations are intentionally unsupported here.
        let managed = current.management == ResourceManagement::Managed;
        let site = if managed {
            Some(self.site_for_resource(current.id)?)
        } else {
            None
        };
        if matches!(
            plan.intent,
            ResourceIntent::Unpublish
                | ResourceIntent::Transfer {
                    visibility: TransferVisibility::Unpublish,
                    ..
                }
        ) {
            let site = site.ok_or_else(|| invalid("no unpublish capability"))?;
            if let Some(history) = self
                .publications
                .get_mut(&Hash::of(&crate::canon::bytes(&site)))
            {
                history.active = None;
            }
        }
        let record = self
            .resources
            .get_mut(&plan.resource.id)
            .expect("checked resource");
        match &plan.intent {
            ResourceIntent::Transfer { to, .. } => {
                record.owner = *to;
                record.attachments.clear();
            }
            ResourceIntent::Detach { cell } => record.attachments.retain(|a| a.cell != *cell),
            ResourceIntent::Unpublish => (),
            ResourceIntent::Destroy => return Err(invalid("destruction is unsupported")),
        }
        self.sync_resource_states();
        Ok(())
    }
    fn cleanup_plan(
        &self,
        cell: CellIdentity,
        choices: &BTreeMap<ResourceId, ResourceOwner>,
    ) -> Result<CleanupPlan> {
        if self.frozen.contains(&cell) {
            return Err(invalid(
                "identity is already frozen; inspect/retry its cleanup receipt",
            ));
        }
        let mut changes = vec![];
        let mut remaining = choices.clone();
        for resource in self.resources.values() {
            let intent = if resource.owner == ResourceOwner::Cell(cell) {
                let target = remaining
                    .remove(&resource.id)
                    .unwrap_or(ResourceOwner::Operator);
                let visibility = if choices.contains_key(&resource.id) {
                    TransferVisibility::KeepCurrent
                } else if resource.management == ResourceManagement::Managed {
                    TransferVisibility::Unpublish
                } else {
                    TransferVisibility::KeepCurrent
                };
                Some(ResourceIntent::Transfer {
                    to: target,
                    visibility,
                })
            } else if resource.attachments.iter().any(|a| a.cell == cell) {
                Some(ResourceIntent::Detach { cell })
            } else {
                None
            };
            if let Some(intent) = intent {
                changes.push(ResourcePlan::new(resource.clone(), intent).map_err(invalid)?);
            }
        }
        if !remaining.is_empty() {
            return Err(invalid(
                "cleanup choices must name resources owned by this cell",
            ));
        }
        Ok(CleanupPlan {
            provider: ProviderRef(self.audience),
            cell,
            inventory_revision: self.inventory().revision,
            changes,
        })
    }
}
impl Registry {
    /// OPERATOR ONLY. Inventory failure is never an empty inventory.
    pub fn resources(&self) -> Result<ResourceInventory> {
        self.transaction(|s| Ok((s.inventory(), false)))
    }
    pub fn plan_resource(&self, id: ResourceId, intent: ResourceIntent) -> Result<ResourcePlan> {
        self.transaction(|s| {
            let r = s.resources.get(&id).ok_or(RegistryError::UnknownSite)?;
            Ok((
                ResourcePlan::new(r.clone(), intent).map_err(invalid)?,
                false,
            ))
        })
    }
    pub fn apply_resource(
        &self,
        id: RequestId,
        plan: &ResourcePlan,
        approval: Hash,
    ) -> Result<ResourceOperation> {
        self.transaction(|s| {
            if plan.approval() != approval {
                return Err(invalid("resource approval mismatch"));
            }
            if let Some(old) = s.resource_operations.get(&id) {
                if old.approval != approval || old.resource != plan.resource.id {
                    return Err(invalid("operation ID already used"));
                }
                return Ok((old.clone(), false));
            }
            s.apply_resource_change(plan)?;
            let result = ResourceOperation {
                id,
                resource: plan.resource.id,
                approval,
                phase: OperationPhase::Succeeded,
            };
            s.resource_operations.insert(id, result.clone());
            Ok((result, true))
        })
    }
    pub fn plan_cleanup(
        &self,
        cell: CellIdentity,
        choices: &BTreeMap<ResourceId, ResourceOwner>,
    ) -> Result<CleanupPlan> {
        self.transaction(|s| Ok((s.cleanup_plan(cell, choices)?, false)))
    }
    /// Freeze + current-policy fencing + site cleanup + receipt are one commit.
    /// Retrying after a lost acknowledgement returns the original receipt.
    pub fn apply_cleanup(
        &self,
        id: RequestId,
        plan: &CleanupPlan,
        approval: Hash,
        acknowledge_manual: bool,
    ) -> Result<CleanupReceipt> {
        self.transaction(|s|{
            if plan.approval()!=approval || plan.provider!=ProviderRef(s.audience) {return Err(invalid("cleanup approval/provider mismatch"));}
            if let Some(old)=s.cleanups.get(&id) {
                if old.approval!=approval {return Err(invalid("cleanup operation ID already used"));}
                return Ok((old.clone(),false));
            }
            if s.frozen.contains(&plan.cell) || s.inventory().revision!=plan.inventory_revision {return Err(invalid("resource inventory changed; review cleanup again"));}
            let mut ids=vec![];
            let mut manual=vec![];
            // Compare the complete owned/attached inventory, not just the caller's
            // list. This also binds namespace allocations and authority changes.
            let expected:Vec<_>=s.resources.values().filter(|r|r.owner==ResourceOwner::Cell(plan.cell)||r.attachments.iter().any(|a|a.cell==plan.cell)).map(|r|r.id).collect();
            if expected!=plan.changes.iter().map(|p|p.resource.id).collect::<Vec<_>>() {return Err(invalid("cleanup omits or adds resources"));}
            for change in &plan.changes {
                if change.resource.owner==ResourceOwner::Cell(plan.cell) {
                    if !matches!(change.intent,ResourceIntent::Transfer{to,..} if to!=ResourceOwner::Cell(plan.cell)) {return Err(invalid("owned resource needs explicit new custody"));}
                } else if change.intent!=(ResourceIntent::Detach{cell:plan.cell}) {return Err(invalid("delegates may only detach during deletion"));}
                if change.effects.contains(&ResourceEffect::RemoteRevocationRequiresManualAction) {manual=vec![ResourceEffect::RemoteRevocationRequiresManualAction,ResourceEffect::CostsMayContinue];}
                s.apply_resource_change(change)?; ids.push(change.resource.id);
            }
            if !manual.is_empty()&&!acknowledge_manual {return Err(invalid("manual provider cleanup must be acknowledged"));}
            s.frozen.push(plan.cell);
            s.authorities.retain(|a|a.key!=plan.cell.0);
            // A frozen namespace creator's delegates cannot create new resources
            // under the departing owner; transferred resources keep only their
            // explicit new owner's subsequently approved access.
            let receipt=CleanupReceipt{operation:id,approval,cell:plan.cell,resources:ids,manual_obligations:manual};
            s.cleanups.insert(id,receipt.clone());
            Ok((receipt,true))
        })
    }
    pub fn cleanup_receipt(&self, id: RequestId) -> Result<Option<CleanupReceipt>> {
        self.transaction(|s| Ok((s.cleanups.get(&id).cloned(), false)))
    }
}
