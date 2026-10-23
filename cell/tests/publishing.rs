#[path = "fixtures/common.rs"]
mod fixtures;
// Publisher authorization boundary, exercised through the trusted-code API.
// Transport authentication and public serving are deliberately not simulated.
use cell::publish::registry::{Registry, RegistryError, SiteAction};
use cell::publish::{NamespaceId, PublishScope, SiteId};
use cell::{CellId, Grant, Key, RequestId, Secret, Tree};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture {
    path: PathBuf,
    registry: Registry,
    owner: Key,
    child: Key,
    stranger: Key,
    namespace: NamespaceId,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cell-publisher-{}", RequestId::generate()));
        let registry = Registry::create(&path).unwrap();
        let owner = Secret::generate().public();
        let namespace = registry.allocate_namespace(owner).unwrap();
        Self {
            path,
            registry,
            owner,
            namespace,
            child: Secret::generate().public(),
            stranger: Secret::generate().public(),
        }
    }
    fn site(&self, name: &str) -> SiteId {
        SiteId {
            namespace: self.namespace.clone(),
            name: name.parse().unwrap(),
        }
    }
    fn approve(&self, child_scopes: Vec<PublishScope>) {
        let parent = fixtures::cell(
            "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
            [Grant::Publish(PublishScope::Namespace(
                self.namespace.clone(),
            ))],
        );
        let child = fixtures::cell(
            "e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(),
            child_scopes.into_iter().map(Grant::Publish),
        );
        let tree = Tree::new([
            (parent, None),
            (
                child,
                Some("7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap()),
            ),
        ])
        .unwrap();
        self.registry.approve_tree(&tree, &self.pins()).unwrap();
    }
    fn pins(&self) -> BTreeMap<CellId, Key> {
        [
            (
                "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
                self.owner,
            ),
            (
                "e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(),
                self.child,
            ),
        ]
        .into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
const ACTIONS: [SiteAction; 4] = [
    SiteAction::Publish,
    SiteAction::Rollback,
    SiteAction::Unpublish,
    SiteAction::Status,
];
fn denied<T: std::fmt::Debug>(result: Result<T, RegistryError>) {
    assert!(matches!(result, Err(RegistryError::Denied)), "{result:?}");
}

#[test]
fn namespace_ownership_is_not_implicit_permission_and_ids_are_not_credentials() {
    let f = Fixture::new();
    let site = f.site("news");
    assert_eq!(f.registry.owner(&f.namespace).unwrap(), f.owner);
    denied(f.registry.create_site(f.owner, &site));
    denied(f.registry.create_site(f.stranger, &site));
    f.approve(vec![]);
    f.registry.create_site(f.owner, &site).unwrap();
    for action in ACTIONS {
        f.registry.authorize(f.owner, &site, action).unwrap();
        denied(f.registry.authorize(f.stranger, &site, action));
        denied(f.registry.authorize(f.child, &site, action));
    }
    let unknown = SiteId {
        namespace: NamespaceId(cell::Hash::of(b"invented")),
        name: site.name,
    };
    denied(f.registry.authorize(f.owner, &unknown, SiteAction::Publish));
    denied(f.registry.create_site(f.owner, &unknown));
}

#[test]
fn site_delegation_is_narrow_and_does_not_change_ownership() {
    let f = Fixture::new();
    f.approve(vec![]);
    let news = f.site("news");
    let sibling = f.site("report");
    f.registry.create_site(f.owner, &news).unwrap();
    f.registry.create_site(f.owner, &sibling).unwrap();
    f.approve(vec![PublishScope::Site(news.clone())]);
    for action in ACTIONS {
        f.registry.authorize(f.child, &news, action).unwrap();
        denied(f.registry.authorize(f.child, &sibling, action));
    }
    denied(f.registry.create_site(f.child, &f.site("another")));
    denied(f.registry.create_site(f.child, &news));
    assert_eq!(f.registry.owner(&news.namespace).unwrap(), f.owner);
}

#[test]
fn namespace_delegates_can_create_multiple_sites_but_not_claim_other_namespaces() {
    let f = Fixture::new();
    let other = f.registry.allocate_namespace(f.stranger).unwrap();
    f.approve(vec![PublishScope::Namespace(f.namespace.clone())]);
    for name in ["news", "report"] {
        f.registry.create_site(f.child, &f.site(name)).unwrap();
        f.registry
            .authorize(f.child, &f.site(name), SiteAction::Publish)
            .unwrap();
    }
    let other_site = SiteId {
        namespace: other.clone(),
        name: "news".parse().unwrap(),
    };
    denied(f.registry.create_site(f.child, &other_site));
    denied(
        f.registry
            .authorize(f.child, &other_site, SiteAction::Unpublish),
    );
    assert_eq!(f.registry.owner(&f.namespace).unwrap(), f.owner);
    assert_eq!(f.registry.owner(&other).unwrap(), f.stranger);
}

#[test]
fn restart_preserves_ownership_reservations_and_delegation() {
    let f = Fixture::new();
    f.approve(vec![]);
    let site = f.site("news");
    f.registry.create_site(f.owner, &site).unwrap();
    f.approve(vec![PublishScope::Site(site.clone())]);
    let reopened = Registry::open(&f.path).unwrap();
    assert_eq!(reopened.owner(&f.namespace).unwrap(), f.owner);
    reopened
        .authorize(f.child, &site, SiteAction::Publish)
        .unwrap();
    assert!(matches!(
        reopened.create_site(f.owner, &site),
        Err(RegistryError::SiteExists)
    ));
    assert!(Registry::create(&f.path).is_err());
}

#[test]
fn revocation_reloads_across_handles_and_does_not_erase_ownership() {
    let f = Fixture::new();
    f.approve(vec![]);
    let site = f.site("news");
    f.registry.create_site(f.owner, &site).unwrap();
    f.approve(vec![PublishScope::Site(site.clone())]);
    let other_process = Registry::open(&f.path).unwrap();
    other_process
        .authorize(f.child, &site, SiteAction::Publish)
        .unwrap();
    f.approve(vec![]);
    for action in ACTIONS {
        denied(other_process.authorize(f.child, &site, action));
    }
    // Removal of the complete tree revokes the owner too, not just delegates.
    f.registry
        .approve_tree(&Tree::new([]).unwrap(), &BTreeMap::new())
        .unwrap();
    denied(other_process.authorize(f.owner, &site, SiteAction::Publish));
    denied(other_process.create_site(f.owner, &f.site("new")));
    assert_eq!(other_process.owner(&f.namespace).unwrap(), f.owner);
}

#[test]
fn a_recreated_cell_name_does_not_inherit_a_namespace() {
    let f = Fixture::new();
    f.approve(vec![]);
    let site = f.site("news");
    f.registry.create_site(f.owner, &site).unwrap();
    let replacement_key = Secret::generate().public();
    denied(
        f.registry
            .authorize(replacement_key, &site, SiteAction::Publish),
    );
    // Approving the new cell with no publishing grants still gives no access.
    let tree = Tree::new([(
        fixtures::cell("7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(), []),
        None,
    )])
    .unwrap();
    f.registry
        .approve_tree(
            &tree,
            &[(
                "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
                replacement_key,
            )]
            .into(),
        )
        .unwrap();
    denied(
        f.registry
            .authorize(replacement_key, &site, SiteAction::Publish),
    );
    denied(f.registry.authorize(f.owner, &site, SiteAction::Publish));
    assert_eq!(f.registry.owner(&f.namespace).unwrap(), f.owner);
    let fresh = f.registry.allocate_namespace(replacement_key).unwrap();
    assert_ne!(fresh, f.namespace);
}

#[test]
fn invalid_policy_updates_are_atomic_and_cannot_create_sites_by_delegation() {
    let f = Fixture::new();
    f.approve(vec![]);
    let site = f.site("news");
    f.registry.create_site(f.owner, &site).unwrap();
    let parent = fixtures::cell(
        "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
        [Grant::Publish(PublishScope::Namespace(f.namespace.clone()))],
    );
    let child = fixtures::cell(
        "e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(),
        [Grant::Publish(PublishScope::Site(f.site("missing")))],
    );
    let tree = Tree::new([(parent.clone(), None), (child, Some(parent.id.clone()))]).unwrap();
    assert!(matches!(
        f.registry.approve_tree(&tree, &f.pins()),
        Err(RegistryError::UnknownSite)
    ));
    f.registry
        .authorize(f.owner, &site, SiteAction::Publish)
        .unwrap();
    denied(f.registry.authorize(f.child, &site, SiteAction::Publish));
    let tree = Tree::new([(parent, None)]).unwrap();
    assert!(matches!(
        f.registry.approve_tree(&tree, &BTreeMap::new()),
        Err(RegistryError::InvalidPolicy(_))
    ));
    f.registry
        .authorize(f.owner, &site, SiteAction::Publish)
        .unwrap();
    let unknown = fixtures::cell(
        "81425432-d0f0-501e-b678-88ce11ee9c5e".parse().unwrap(),
        [Grant::Publish(PublishScope::Namespace(NamespaceId(
            cell::Hash::of(b"unknown"),
        )))],
    );
    assert!(matches!(
        f.registry.approve_tree(
            &Tree::new([(unknown, None)]).unwrap(),
            &[(
                "81425432-d0f0-501e-b678-88ce11ee9c5e".parse().unwrap(),
                f.stranger
            )]
            .into()
        ),
        Err(RegistryError::UnknownNamespace)
    ));
}

#[test]
fn shared_cell_keys_are_rejected_even_if_one_cell_has_no_grants() {
    let f = Fixture::new();
    let parent = fixtures::cell(
        "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
        [Grant::Publish(PublishScope::Namespace(f.namespace.clone()))],
    );
    let child = fixtures::cell("e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(), []);
    let tree = Tree::new([
        (parent, None),
        (
            child,
            Some("7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap()),
        ),
    ])
    .unwrap();
    let pins = [
        (
            "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
            f.owner,
        ),
        (
            "e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(),
            f.owner,
        ),
    ]
    .into();
    assert!(matches!(
        f.registry.approve_tree(&tree, &pins),
        Err(RegistryError::InvalidPolicy(_))
    ));
    denied(f.registry.create_site(f.owner, &f.site("news")));
}

#[test]
fn concurrent_writers_do_not_lose_reservations_or_double_create() {
    let f = Fixture::new();
    f.approve(vec![]);
    std::thread::scope(|scope| {
        let mut handles = vec![];
        for i in 0..12 {
            let fixture = &f;
            handles.push(scope.spawn(move || {
                let registry = Registry::open(&fixture.path).unwrap();
                registry
                    .create_site(fixture.owner, &fixture.site(&format!("site-{i}")))
                    .unwrap();
                registry.create_site(fixture.owner, &fixture.site("shared"))
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results
            .iter()
            .all(|r| r.is_ok() || matches!(r, Err(RegistryError::SiteExists))));
    });
    for i in 0..12 {
        f.registry
            .authorize(f.owner, &f.site(&format!("site-{i}")), SiteAction::Status)
            .unwrap();
    }
}

#[test]
fn state_is_private_and_missing_or_corrupt_state_fails_closed() {
    let f = Fixture::new();
    for (path, mode) in [
        (&f.path, 0o700),
        (&f.path.join("lock"), 0o600),
        (&f.path.join("state.json"), 0o600),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
    f.approve(vec![]);
    let state = f.path.join("state.json");
    let original = fs::read(&state).unwrap();
    // A crash before rename can leave a temporary file. It is never loaded.
    fs::write(f.path.join(".state-interrupted"), b"garbage").unwrap();
    Registry::open(&f.path).unwrap();
    for corrupt in [b"{".to_vec(), b"{}".to_vec(), {
        let mut v: serde_json::Value = serde_json::from_slice(&original).unwrap();
        v["format"] = serde_json::json!(99);
        serde_json::to_vec(&v).unwrap()
    }] {
        fs::write(&state, corrupt).unwrap();
        assert!(Registry::open(&f.path).is_err());
        assert!(f.registry.create_site(f.owner, &f.site("news")).is_err());
    }
    fs::remove_file(&state).unwrap();
    assert!(Registry::open(&f.path).is_err());
    assert!(Registry::create(&f.path).is_err());
}
