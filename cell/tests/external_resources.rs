use cell::publish::registry::Registry;
use cell::resource::*;
use cell::{Hash, RequestId, Secret};
struct Fixture {
    dir: std::path::PathBuf,
    registry: Registry,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("external-resource-{}", RequestId::generate()));
        let registry = Registry::create(&dir).unwrap();
        Self { dir, registry }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn registration(account: &str) -> ExternalRegistration {
    ExternalRegistration {
        account: ExternalAccount {
            provider: "runpod".to_string().try_into().unwrap(),
            account: account.to_string().try_into().unwrap(),
        },
        kind: ResourceKind::Compute,
        external_id: "pod-existing".to_string().try_into().unwrap(),
    }
}
fn apply(registry: &Registry, plan: &ResourcePlan) {
    registry
        .apply_resource_acknowledged(RequestId::generate(), plan, plan.approval(), true)
        .unwrap();
}
#[test]
fn registration_is_scoped_non_provisioning_idempotent_and_cannot_replace_records() {
    let f = Fixture::new();
    let plan = f.registry.plan_external(registration("account-a")).unwrap();
    assert_eq!(plan.expected_revision, Hash::ZERO);
    assert_eq!(plan.resource.state, ResourceState::Unknown);
    assert_eq!(plan.resource.owner, ResourceOwner::Operator);
    assert_ne!(
        plan.resource.id,
        f.registry
            .plan_external(registration("account-b"))
            .unwrap()
            .resource
            .id
    );
    assert!(f
        .registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .is_err());
    assert!(f.registry.resources().unwrap().resources.is_empty());
    let id = RequestId::generate();
    let receipt = f
        .registry
        .apply_resource_acknowledged(id, &plan, plan.approval(), true)
        .unwrap();
    let reopened = Registry::open(&f.dir).unwrap();
    assert_eq!(
        reopened.apply_resource(id, &plan, plan.approval()).unwrap(),
        receipt
    );
    assert!(reopened.plan_external(registration("account-a")).is_err());
    assert!(reopened
        .apply_resource_acknowledged(RequestId::generate(), &plan, plan.approval(), true)
        .is_err());
    let record = &reopened.resources().unwrap().resources[0];
    assert_eq!(record.management, ResourceManagement::External);
    assert!(reopened
        .plan_resource(record.id, ResourceIntent::Destroy)
        .is_err());
    assert!(reopened
        .plan_resource(record.id, ResourceIntent::Unpublish)
        .is_err());
    let mut forged = plan.clone();
    forged.resource.management = ResourceManagement::Managed;
    assert!(reopened
        .apply_resource_acknowledged(RequestId::generate(), &forged, forged.approval(), true)
        .is_err());
}
#[test]
fn attachments_have_no_ownership_or_credential_delivery_and_cleanup_is_local() {
    let f = Fixture::new();
    let plan = f.registry.plan_external(registration("account-a")).unwrap();
    let id = plan.resource.id;
    apply(&f.registry, &plan);
    let a = CellIdentity(Secret::generate().public());
    let b = CellIdentity(Secret::generate().public());
    for cell in [a, b] {
        let plan = f
            .registry
            .plan_resource(
                id,
                ResourceIntent::Attach(ResourceAttachment {
                    cell,
                    scope: AccessScope::Use,
                    credential: Some(CredentialRef(RequestId::generate())),
                }),
            )
            .unwrap();
        assert!(plan
            .effects
            .contains(&ResourceEffect::ProviderAccessRequiresManualAction));
        apply(&f.registry, &plan);
    }
    let before = f.registry.resources().unwrap().resources[0].clone();
    assert_eq!(before.owner, ResourceOwner::Operator);
    assert_eq!(before.attachments.len(), 2);
    assert!(f
        .registry
        .plan_resource(
            id,
            ResourceIntent::Attach(ResourceAttachment {
                cell: a,
                scope: AccessScope::Use,
                credential: None
            })
        )
        .is_err());
    let cleanup = f.registry.plan_cleanup(a, &Default::default()).unwrap();
    assert_eq!(
        cleanup.changes[0].intent,
        ResourceIntent::Detach { cell: a }
    );
    let op = RequestId::generate();
    assert!(f
        .registry
        .apply_cleanup(op, &cleanup, cleanup.approval(), false)
        .is_err());
    assert_eq!(f.registry.resources().unwrap().resources[0], before);
    let receipt = f
        .registry
        .apply_cleanup(op, &cleanup, cleanup.approval(), true)
        .unwrap();
    assert!(receipt
        .manual_obligations
        .contains(&ResourceEffect::RemoteRevocationRequiresManualAction));
    let record = f.registry.resources().unwrap().resources[0].clone();
    assert_eq!(record.owner, ResourceOwner::Operator);
    assert_eq!(record.state, ResourceState::Unknown);
    assert_eq!(record.attachments.len(), 1);
    assert_eq!(record.attachments[0].cell, b);
    assert!(f
        .registry
        .plan_resource(
            id,
            ResourceIntent::Attach(ResourceAttachment {
                cell: a,
                scope: AccessScope::Use,
                credential: None
            })
        )
        .is_err());
    let stale = f
        .registry
        .plan_resource(id, ResourceIntent::Detach { cell: b })
        .unwrap();
    let c = CellIdentity(Secret::generate().public());
    apply(
        &f.registry,
        &f.registry
            .plan_resource(
                id,
                ResourceIntent::Attach(ResourceAttachment {
                    cell: c,
                    scope: AccessScope::Use,
                    credential: None,
                }),
            )
            .unwrap(),
    );
    assert!(f
        .registry
        .apply_resource_acknowledged(RequestId::generate(), &stale, stale.approval(), true)
        .is_err());
}

#[test]
fn simultaneous_registration_plans_commit_only_once() {
    let f = Fixture::new();
    let plan = f.registry.plan_external(registration("account-a")).unwrap();
    let successes = std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| {
                let path = &f.dir;
                let plan = &plan;
                scope.spawn(move || {
                    Registry::open(path)
                        .unwrap()
                        .apply_resource_acknowledged(
                            RequestId::generate(),
                            plan,
                            plan.approval(),
                            true,
                        )
                        .is_ok()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(successes, 1);
    assert_eq!(f.registry.resources().unwrap().resources.len(), 1);
}
