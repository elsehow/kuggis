use cell::publish::{
    artifact::{Artifact, Entry},
    registry::{Registry, SiteAction},
    transport::{self, Command, Reply, SignedRequest},
    NamespaceId, PublishScope, SiteId,
};
use cell::resource::*;
use cell::{Cell, Grant, RequestId, Secret, Tree};
use std::{collections::BTreeMap, fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    registry: Registry,
    owner: Secret,
    delegate: Secret,
    ns: NamespaceId,
    site: SiteId,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("resource-lifecycle-{}", RequestId::generate()));
        let registry = Registry::create(&root).unwrap();
        let owner = Secret::generate();
        let delegate = Secret::generate();
        let ns = registry.allocate_namespace(owner.public()).unwrap();
        let site = SiteId {
            namespace: ns.clone(),
            name: "demo".parse().unwrap(),
        };
        let f = Self {
            root,
            registry,
            owner,
            delegate,
            ns,
            site,
        };
        f.approve();
        f.registry
            .configure_public_base("https://sites.example.com".parse().unwrap())
            .unwrap();
        f.publish();
        f
    }
    fn approve(&self) {
        let grant = Grant::Publish(PublishScope::Namespace(self.ns.clone()));
        let tree = Tree::new([
            (Cell::new("owner".parse().unwrap(), [grant.clone()]), None),
            (
                Cell::new("delegate".parse().unwrap(), [grant]),
                Some("owner".parse().unwrap()),
            ),
        ])
        .unwrap();
        self.registry
            .approve_tree(
                &tree,
                &[
                    ("owner".parse().unwrap(), self.owner.public()),
                    ("delegate".parse().unwrap(), self.delegate.public()),
                ]
                .into(),
            )
            .unwrap();
    }
    fn signed(&self, command: Command) -> SignedRequest {
        SignedRequest::sign(
            &self.owner,
            self.registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            command,
        )
    }
    fn publish(&self) {
        let artifact = Artifact {
            files: vec![Entry {
                path: "index.html".into(),
                bytes: b"hello".to_vec(),
            }],
        };
        assert!(matches!(
            self.registry
                .execute_upload(
                    &self.signed(Command::Publish {
                        site: self.site.clone(),
                        artifact: artifact.hash().unwrap()
                    }),
                    &artifact
                )
                .unwrap(),
            Reply::Published(_)
        ));
    }
    fn resource(&self) -> ResourceRecord {
        self.registry
            .resources()
            .unwrap()
            .resources
            .into_iter()
            .find(|r| r.external_id.as_str().ends_with("/demo"))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn migration_preserves_urls_versions_keys_and_resource_ids() {
    let f = Fixture::new();
    let expected = f.resource();
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root.join("state.json")).unwrap()).unwrap();
    old["format"] = serde_json::json!(2);
    for field in ["resources", "resource_operations", "frozen", "cleanups"] {
        old.as_object_mut().unwrap().remove(field);
    }
    fs::write(f.root.join("state.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let registry = Registry::open(&f.root).unwrap();
    assert_eq!(
        registry.resources().unwrap().resources,
        vec![expected.clone()]
    );
    assert_eq!(
        registry.public_file(&f.site, "index.html").unwrap(),
        b"hello"
    );
    assert_eq!(
        Registry::open(&f.root)
            .unwrap()
            .resources()
            .unwrap()
            .resources,
        vec![expected]
    );
    registry
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .unwrap();
}
#[test]
fn transfer_preserves_identity_and_fences_broad_grants() {
    let f = Fixture::new();
    let before = f.resource();
    let plan = f
        .registry
        .plan_resource(
            before.id,
            ResourceIntent::Transfer {
                to: ResourceOwner::Operator,
                visibility: TransferVisibility::KeepCurrent,
            },
        )
        .unwrap();
    let operation = RequestId::generate();
    let receipt = f
        .registry
        .apply_resource(operation, &plan, plan.approval())
        .unwrap();
    assert_eq!(
        f.registry
            .apply_resource(operation, &plan, plan.approval())
            .unwrap(),
        receipt
    );
    let after = f.resource();
    assert_eq!(after.id, before.id);
    assert_eq!(after.external_id, before.external_id);
    assert_eq!(after.state, before.state);
    assert_eq!(after.owner, ResourceOwner::Operator);
    assert!(after.attachments.is_empty());
    assert!(f
        .registry
        .authorize(f.owner.public(), &f.site, SiteAction::Publish)
        .is_err());
    assert!(f
        .registry
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .is_err());
    f.approve(); // unchanged namespace grants cannot recapture a transferred site
    assert!(f
        .registry
        .authorize(f.owner.public(), &f.site, SiteAction::Publish)
        .is_err());
    f.registry
        .create_site(
            f.owner.public(),
            &SiteId {
                namespace: f.ns.clone(),
                name: "new".parse().unwrap(),
            },
        )
        .unwrap();
    assert!(f
        .registry
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .is_err());
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"hello"
    );
}
#[test]
fn stale_resource_or_inventory_plans_fail_without_side_effects() {
    let f = Fixture::new();
    let id = f.resource().id;
    let plan = f
        .registry
        .plan_resource(id, ResourceIntent::Unpublish)
        .unwrap();
    f.publish();
    assert!(f
        .registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .is_err());
    let cleanup = f
        .registry
        .plan_cleanup(CellIdentity(f.owner.public()), &BTreeMap::new())
        .unwrap();
    f.registry
        .create_site(
            f.owner.public(),
            &SiteId {
                namespace: f.ns.clone(),
                name: "late".parse().unwrap(),
            },
        )
        .unwrap();
    assert!(f
        .registry
        .apply_cleanup(RequestId::generate(), &cleanup, cleanup.approval(), false)
        .is_err());
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"hello"
    );
}
#[test]
fn owner_cleanup_is_atomic_durable_and_fences_signed_requests_and_creation() {
    let f = Fixture::new();
    let late = f.signed(Command::ReserveSite {
        site: SiteId {
            namespace: f.ns.clone(),
            name: "late".parse().unwrap(),
        },
    });
    let cell = CellIdentity(f.owner.public());
    let plan = f.registry.plan_cleanup(cell, &BTreeMap::new()).unwrap();
    let id = RequestId::generate();
    let receipt = f
        .registry
        .apply_cleanup(id, &plan, plan.approval(), false)
        .unwrap();
    let reopened = Registry::open(&f.root).unwrap();
    assert_eq!(
        reopened
            .apply_cleanup(id, &plan, plan.approval(), false)
            .unwrap(),
        receipt
    );
    assert!(reopened.public_file(&f.site, "index.html").is_err());
    assert_eq!(f.resource().owner, ResourceOwner::Operator);
    assert!(reopened.execute(&late).is_err());
    assert!(reopened.allocate_namespace(f.owner.public()).is_err());
    assert!(reopened
        .create_site(
            f.delegate.public(),
            &SiteId {
                namespace: f.ns.clone(),
                name: "late".parse().unwrap()
            }
        )
        .is_err());
    assert!(reopened
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .is_err());
    assert_eq!(reopened.cleanup_receipt(id).unwrap(), Some(receipt));
}
#[test]
fn delegate_cleanup_does_not_remove_owners_site_or_other_access() {
    let f = Fixture::new();
    let before = f.resource();
    let plan = f
        .registry
        .plan_cleanup(CellIdentity(f.delegate.public()), &BTreeMap::new())
        .unwrap();
    f.registry
        .apply_cleanup(RequestId::generate(), &plan, plan.approval(), false)
        .unwrap();
    assert_eq!(f.resource().owner, before.owner);
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"hello"
    );
    f.registry
        .authorize(f.owner.public(), &f.site, SiteAction::Publish)
        .unwrap();
    assert!(f
        .registry
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .is_err());
}
#[test]
fn cleanup_cannot_omit_owned_resources_or_reuse_operation_ids() {
    let f = Fixture::new();
    let mut plan = f
        .registry
        .plan_cleanup(CellIdentity(f.owner.public()), &BTreeMap::new())
        .unwrap();
    plan.changes.clear();
    assert!(f
        .registry
        .apply_cleanup(RequestId::generate(), &plan, plan.approval(), false)
        .is_err());
    let plan = f
        .registry
        .plan_resource(f.resource().id, ResourceIntent::Unpublish)
        .unwrap();
    let id = RequestId::generate();
    f.registry
        .apply_resource(id, &plan, plan.approval())
        .unwrap();
    let changed = f
        .registry
        .plan_resource(
            f.resource().id,
            ResourceIntent::Transfer {
                to: ResourceOwner::Operator,
                visibility: TransferVisibility::KeepCurrent,
            },
        )
        .unwrap();
    assert!(f
        .registry
        .apply_resource(id, &changed, changed.approval())
        .is_err());
}

#[test]
fn creating_another_site_does_not_restore_a_cleared_explicit_delegate() {
    let f = Fixture::new();
    let tree = Tree::new([
        (
            Cell::new(
                "owner".parse().unwrap(),
                [Grant::Publish(PublishScope::Namespace(f.ns.clone()))],
            ),
            None,
        ),
        (
            Cell::new(
                "delegate".parse().unwrap(),
                [Grant::Publish(PublishScope::Site(f.site.clone()))],
            ),
            Some("owner".parse().unwrap()),
        ),
    ])
    .unwrap();
    f.registry
        .approve_tree(
            &tree,
            &[
                ("owner".parse().unwrap(), f.owner.public()),
                ("delegate".parse().unwrap(), f.delegate.public()),
            ]
            .into(),
        )
        .unwrap();
    let plan = f
        .registry
        .plan_resource(
            f.resource().id,
            ResourceIntent::Transfer {
                to: ResourceOwner::Operator,
                visibility: TransferVisibility::KeepCurrent,
            },
        )
        .unwrap();
    f.registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .unwrap();
    f.registry
        .create_site(
            f.owner.public(),
            &SiteId {
                namespace: f.ns.clone(),
                name: "other".parse().unwrap(),
            },
        )
        .unwrap();
    assert!(f
        .registry
        .authorize(f.delegate.public(), &f.site, SiteAction::Publish)
        .is_err());
}
