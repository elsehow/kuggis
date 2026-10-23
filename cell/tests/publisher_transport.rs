#[path = "fixtures/common.rs"]
mod fixtures;
use cell::publish::{
    operator,
    registry::{Registry, RegistryError, SiteAction},
    transport::{self, Command, Reply, SignedRequest},
    NamespaceId, PublishScope, SiteId,
};
use cell::{CellId, Grant, Key, RequestId, Secret, Tree};
use std::{collections::BTreeMap, fs, path::PathBuf};

struct Fixture {
    path: PathBuf,
    registry: Registry,
    secret: Secret,
    namespace: NamespaceId,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cell-publisher-wire-{}", RequestId::generate()));
        let registry = Registry::create(&path).unwrap();
        let secret = Secret::generate();
        let namespace = registry.allocate_namespace(secret.public()).unwrap();
        let f = Self {
            path,
            registry,
            secret,
            namespace,
        };
        f.registry.approve_tree(&f.tree(), &f.pins()).unwrap();
        f
    }
    fn pins(&self) -> BTreeMap<CellId, Key> {
        [(
            "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
            self.secret.public(),
        )]
        .into()
    }
    fn tree(&self) -> Tree {
        Tree::new([(
            fixtures::cell(
                "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
                [Grant::Publish(PublishScope::Namespace(
                    self.namespace.clone(),
                ))],
            ),
            None,
        )])
        .unwrap()
    }
    fn site(&self) -> SiteId {
        SiteId {
            namespace: self.namespace.clone(),
            name: "news".parse().unwrap(),
        }
    }
    fn sign(&self, command: Command) -> SignedRequest {
        SignedRequest::sign(
            &self.secret,
            self.registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            command,
        )
    }
    fn reserve(&self) -> SignedRequest {
        self.sign(Command::ReserveSite { site: self.site() })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn signed_commands_are_bound_to_all_fields_and_cannot_borrow_a_signature() {
    let f = Fixture::new();
    let request = f.reserve();
    let value = serde_json::to_value(&request).unwrap();
    let stranger = Secret::generate();
    let mut mutations = vec![];
    for (field, replacement) in [
        ("audience", serde_json::json!(RequestId::generate())),
        ("caller", serde_json::json!(stranger.public())),
        ("request", serde_json::json!(RequestId::generate())),
        (
            "expires",
            serde_json::json!(transport::unix_time().unwrap() + 200),
        ),
        (
            "command",
            serde_json::json!(Command::Check {
                site: f.site(),
                action: SiteAction::Unpublish
            }),
        ),
    ] {
        let mut changed = value.clone();
        changed["body"][field] = replacement;
        mutations.push(changed);
    }
    let mut changed = value.clone();
    changed["body"]["command"]["reserve_site"]["site"]["name"] =
        serde_json::json!("70ee78ad-5e3a-510a-af20-6f485ecb6f16");
    mutations.push(changed);
    let mut changed = value.clone();
    changed["signature"] = serde_json::json!("00".repeat(64));
    mutations.push(changed);
    for changed in mutations {
        let forged: SignedRequest = serde_json::from_value(changed).unwrap();
        assert!(matches!(
            f.registry.execute(&forged),
            Err(RegistryError::Unauthenticated)
        ));
    }
    let forged = SignedRequest::sign(
        &stranger,
        f.registry.audience().unwrap(),
        RequestId::generate(),
        transport::unix_time().unwrap() + 240,
        Command::ReserveSite { site: f.site() },
    );
    assert!(matches!(
        f.registry.execute(&forged),
        Err(RegistryError::Denied)
    ));
    assert_eq!(f.registry.execute(&request).unwrap(), Reply::Reserved);
    let mut extra = value;
    extra["grants"] = serde_json::json!([{"publish":{"namespace":f.namespace}}]);
    assert!(serde_json::from_value::<SignedRequest>(extra).is_err());
}

#[test]
fn expiry_audience_and_durable_replay_checks_fail_closed() {
    let f = Fixture::new();
    let command = Command::ReserveSite { site: f.site() };
    let now = transport::unix_time().unwrap();
    for expires in [0, now, now + 3600, u64::MAX] {
        let signed = SignedRequest::sign(
            &f.secret,
            f.registry.audience().unwrap(),
            RequestId::generate(),
            expires,
            command.clone(),
        );
        assert!(matches!(
            f.registry.execute(&signed),
            Err(RegistryError::Unauthenticated)
        ));
    }
    let wrong_audience = SignedRequest::sign(
        &f.secret,
        RequestId::generate(),
        RequestId::generate(),
        now + 240,
        command,
    );
    assert!(matches!(
        f.registry.execute(&wrong_audience),
        Err(RegistryError::Unauthenticated)
    ));
    let request = f.reserve();
    assert_eq!(f.registry.execute(&request).unwrap(), Reply::Reserved);
    let restarted = Registry::open(&f.path).unwrap();
    assert!(matches!(
        restarted.execute(&request),
        Err(RegistryError::Replay)
    ));
    // Same caller+request ID, different signed command still cannot reuse it.
    let value = serde_json::to_value(&request).unwrap();
    let reused = SignedRequest::sign(
        &f.secret,
        restarted.audience().unwrap(),
        serde_json::from_value(value["body"]["request"].clone()).unwrap(),
        now + 240,
        Command::Check {
            site: f.site(),
            action: SiteAction::Status,
        },
    );
    assert!(matches!(
        restarted.execute(&reused),
        Err(RegistryError::Replay)
    ));
    let status = f.sign(Command::Check {
        site: f.site(),
        action: SiteAction::Status,
    });
    assert_eq!(restarted.execute(&status).unwrap(), Reply::Authorized);
}

#[test]
fn signed_requests_do_not_cache_permission_past_revocation() {
    let f = Fixture::new();
    let request = f.reserve();
    f.registry
        .approve_tree(&Tree::new([]).unwrap(), &BTreeMap::new())
        .unwrap();
    assert!(matches!(
        f.registry.execute(&request),
        Err(RegistryError::Denied)
    ));
}

#[test]
fn replay_record_and_site_reservation_commit_once_under_concurrency() {
    let f = Fixture::new();
    let request = f.reserve();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let request = &request;
                let path = &f.path;
                scope.spawn(move || Registry::open(path).unwrap().execute(request))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Ok(Reply::Reserved)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(RegistryError::Replay)))
                .count(),
            7
        );
    });
}

#[test]
fn operator_approval_binds_pins_topology_grants_audience_and_previous_policy() {
    let f = Fixture::new();
    let plan = f.registry.plan_policy(&f.tree(), &f.pins()).unwrap();
    let different_key = Secret::generate().public();
    let other_pins = [(
        "7721fe29-1976-53fa-8250-32afa9e9bb35".parse().unwrap(),
        different_key,
    )]
    .into();
    assert!(f
        .registry
        .apply_policy(&f.tree(), &other_pins, &plan.hash())
        .is_err());
    let empty = Tree::new([]).unwrap();
    assert!(f
        .registry
        .apply_policy(&empty, &BTreeMap::new(), &plan.hash())
        .is_err());
    // Even restoring the same proposed policy requires a new approval if the
    // current policy has changed since preview.
    f.registry.approve_tree(&empty, &BTreeMap::new()).unwrap();
    assert!(f
        .registry
        .apply_policy(&f.tree(), &f.pins(), &plan.hash())
        .is_err());
    let current = f.registry.plan_policy(&f.tree(), &f.pins()).unwrap();
    f.registry
        .apply_policy(&f.tree(), &f.pins(), &current.hash())
        .unwrap();

    let parent = f.tree().iter().next().unwrap().clone();
    let child = fixtures::cell("e3e321ad-3f5e-5221-9ec8-ccc87947084b".parse().unwrap(), []);
    let mut pins = f.pins();
    pins.insert(child.id.clone(), different_key);
    let nested = Tree::new([
        (parent.clone(), None),
        (child.clone(), Some(parent.id.clone())),
    ])
    .unwrap();
    let roots = Tree::new([(parent, None), (child, None)]).unwrap();
    let nested_plan = f.registry.plan_policy(&nested, &pins).unwrap();
    assert!(f
        .registry
        .apply_policy(&roots, &pins, &nested_plan.hash())
        .is_err());

    let other = Fixture::new();
    let empty_plan = f.registry.plan_policy(&empty, &BTreeMap::new()).unwrap();
    assert!(other
        .registry
        .apply_policy(&empty, &BTreeMap::new(), &empty_plan.hash())
        .is_err());
}

#[test]
fn policy_loader_requires_existing_regular_pins_and_a_valid_tree() {
    let f = Fixture::new();
    let tree_dir = f.path.join("tree");
    let pins_dir = tree_dir.join(".pins");
    fs::create_dir_all(&pins_dir).unwrap();
    let cell = f.tree().iter().next().unwrap().clone();
    fs::write(
        tree_dir.join("7721fe29-1976-53fa-8250-32afa9e9bb35.json"),
        serde_json::to_vec(&cell).unwrap(),
    )
    .unwrap();
    assert!(operator::load(&tree_dir, &pins_dir).is_err());
    let pin = pins_dir.join("7721fe29-1976-53fa-8250-32afa9e9bb35.pub");
    fs::write(&pin, f.secret.public().to_string()).unwrap();
    let (tree, pins) = operator::load(&tree_dir, &pins_dir).unwrap();
    assert_eq!(tree, f.tree());
    assert_eq!(pins, f.pins());
    fs::remove_file(&pin).unwrap();
    let target = f.path.join("key.pub");
    fs::write(&target, f.secret.public().to_string()).unwrap();
    std::os::unix::fs::symlink(target, pin).unwrap();
    assert!(operator::load(&tree_dir, &pins_dir).is_err());
}

async fn http(address: std::net::SocketAddr, path: &str, body: String, extra: &str) -> String {
    let path = path.to_owned();
    let extra = extra.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }).await.unwrap()
}

#[tokio::test]
async fn real_http_endpoint_requires_signatures_limits_bodies_and_has_no_admin_routes() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let registry = Registry::open(&f.path).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, transport::router(registry))
            .await
            .unwrap()
    });
    let signed = serde_json::to_string(&f.reserve()).unwrap();
    let valid = http(address, "/v1/requests", signed.clone(), "").await;
    assert!(valid.starts_with("HTTP/1.1 200"), "{valid}");
    assert!(valid.contains("reserved"));
    assert!(http(address, "/v1/requests", signed, "")
        .await
        .starts_with("HTTP/1.1 409"));
    let mut forged = serde_json::to_value(f.reserve()).unwrap();
    forged["signature"] = serde_json::json!("00".repeat(64));
    assert!(http(
        address,
        "/v1/requests",
        forged.to_string(),
        "X-Exedev-Source-Vm: 7721fe29-1976-53fa-8250-32afa9e9bb35\r\n"
    )
    .await
    .starts_with("HTTP/1.1 401"));
    assert!(http(address, "/v1/requests", "{}".into(), "")
        .await
        .starts_with("HTTP/1.1 422"));
    assert!(http(
        address,
        "/v1/requests",
        "x".repeat(transport::MAX_REQUEST_BYTES + 1),
        ""
    )
    .await
    .starts_with("HTTP/1.1 413"));
    for admin in ["/v1/policy", "/v1/namespace", "/v1/approve", "/state.json"] {
        assert!(http(address, admin, "{}".into(), "")
            .await
            .starts_with("HTTP/1.1 404"));
    }
    assert!(transport::serve(
        Registry::open(&f.path).unwrap(),
        "0.0.0.0:0".parse().unwrap()
    )
    .await
    .is_err());
    server.abort();
}

#[test]
fn pre_transport_registry_upgrade_preserves_ownership_grants_and_reservations() {
    let f = Fixture::new();
    f.registry
        .create_site(f.secret.public(), &f.site())
        .unwrap();
    let path = f.path.join("state.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old["format"] = serde_json::json!(1);
    for field in ["resources", "resource_operations", "frozen", "cleanups"] {
        old.as_object_mut().unwrap().remove(field);
    }
    old.as_object_mut().unwrap().remove("public_base");
    old.as_object_mut().unwrap().remove("publications");
    old.as_object_mut().unwrap().remove("audience");
    old.as_object_mut().unwrap().remove("used_requests");
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let upgraded = Registry::open(&f.path).unwrap();
    assert_eq!(upgraded.owner(&f.namespace).unwrap(), f.secret.public());
    upgraded
        .authorize(f.secret.public(), &f.site(), SiteAction::Publish)
        .unwrap();
    assert!(matches!(
        upgraded.create_site(f.secret.public(), &f.site()),
        Err(RegistryError::SiteExists)
    ));
    let audience = upgraded.audience().unwrap();
    assert_eq!(
        Registry::open(&f.path).unwrap().audience().unwrap(),
        audience
    );
    let persisted: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted["format"], 3);
}

#[test]
fn replay_capacity_failure_does_not_commit_a_reservation_and_expiry_reclaims_space() {
    let f = Fixture::new();
    let state_path = f.path.join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let entries = state["used_requests"].as_object_mut().unwrap();
    let expires = transport::unix_time().unwrap() + 240;
    for i in 0_u32..10_000 {
        entries.insert(
            cell::Hash::of(&i.to_le_bytes()).to_string(),
            serde_json::json!(expires),
        );
    }
    let original = serde_json::to_vec(&state).unwrap();
    fs::write(&state_path, &original).unwrap();
    let request = f.reserve();
    assert!(matches!(
        f.registry.execute(&request),
        Err(RegistryError::Busy)
    ));
    assert_eq!(fs::read(&state_path).unwrap(), original);
    for expiry in state["used_requests"].as_object_mut().unwrap().values_mut() {
        *expiry = serde_json::json!(0);
    }
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(f.registry.execute(&request).unwrap(), Reply::Reserved);
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    assert_eq!(saved["used_requests"].as_object().unwrap().len(), 1);
}
