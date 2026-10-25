use super::*;
use crate::{
    host_binding::Binding,
    identity::{Identity, Registry},
    ssh_host, Secret,
};

struct NoExe;
impl Edge for NoExe {
    fn listing(&self) -> apply::Fallible<(Vec<crate::exe::Vm>, Vec<crate::exe::Integration>)> {
        panic!("external host used exe.dev inventory")
    }
    fn binary(&self) -> apply::Fallible<Option<Hash>> {
        panic!("external host used exe.dev binary")
    }
    fn run(&self, _: &[&str]) -> apply::Fallible<String> {
        panic!("external host mutated exe.dev")
    }
    fn log(&self, _: &str) -> apply::Fallible<Option<(crate::Key, Vec<crate::Statement>)>> {
        panic!("external host read exe.dev log")
    }
    fn setup(&self, _: &str, _: &Cell) -> apply::Fallible<String> {
        panic!("external host used exe.dev setup")
    }
    fn knock(&self, _: &str, _: &crate::Said) -> apply::Fallible<u64> {
        panic!("external host used exe.dev inbox")
    }
}
struct Fixture {
    root: PathBuf,
    op: Arc<Operator>,
    binding: Binding,
    name: Name,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("host-supervisor-{}", RequestId::generate()));
        let tree = root.join("tree");
        let state = root.join("state");
        fs::create_dir_all(tree.join(".hosts")).unwrap();
        fs::create_dir_all(tree.join(".pins")).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(
            root.join("known_hosts"),
            "fixture-host ssh-ed25519 fixture\n",
        )
        .unwrap();
        let name: Name = "worker-canary".parse().unwrap();
        let key = Secret::generate().public();
        let host = ssh_host::Config {
            hostname: "worker.example".into(),
            user: "operator".into(),
            port: 22,
            identity_file: root.join("identity"),
            known_hosts: root.join("known_hosts"),
            runtime: crate::host_runtime::Runtime::Ssh {
                dns: ["127.0.0.53".parse().unwrap()].into(),
                memory_max: 8 * 1024 * 1024 * 1024,
            },
        };
        let binding = Binding::new(
            host,
            ssh_host::Pin {
                machine_id: "1234567890abcdef1234567890abcdef".into(),
                cell: name.clone(),
                key,
            },
        )
        .unwrap();
        let cell = Cell::new(name.clone(), []);
        fs::write(tree.join(format!("{name}.json")), cell.canonical()).unwrap();
        crate::identity::atomic(
            &tree.join(".pins").join(format!("{name}.pub")),
            key.to_string().as_bytes(),
        )
        .unwrap();
        Registry {
            version: 1,
            cells: vec![Identity {
                id: crate::CellId::generate(),
                binding: name.clone(),
                name: name.clone(),
                key,
                vm: None,
            }],
        }
        .save(&tree)
        .unwrap();
        crate::identity::atomic(
            &tree.join(".hosts").join(format!("{name}.json")),
            &crate::canon::bytes(&binding),
        )
        .unwrap();
        let op = Arc::new(Operator {
            config: Config {
                tree,
                state,
                assets: root.clone(),
                listen: "127.0.0.1:0".parse().unwrap(),
                access: Access::Local,
                binary: Some(root.join("cell")),
                sockets: None,
            },
            authority: "127.0.0.1:8000".into(),
            relays: Mutex::new(BTreeMap::new()),
            vms: tokio::sync::RwLock::new(BTreeMap::new()),
            edge: Arc::new(NoExe),
            changes: Mutex::new(BTreeMap::new()),
            applying: Arc::new(Mutex::new(())),
            stopping: tokio::sync::watch::channel(false).0,
        });
        Self {
            root,
            op,
            binding,
            name,
        }
    }
    fn save(&self, binding: &Binding) {
        crate::identity::atomic(
            &self
                .op
                .config
                .tree
                .join(".hosts")
                .join(format!("{}.json", self.name)),
            &crate::canon::bytes(binding),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn external_inventory_and_routing_never_use_exe() {
    let f = Fixture::new();
    f.op.refresh_vms().await.unwrap();
    let view = cells(State(f.op.clone())).await.unwrap().0;
    assert_eq!(view.cells.len(), 1);
    assert_eq!(view.cells[0].lifecycle, Lifecycle::Available);
    assert_eq!(
        view.cells[0].external_host.as_deref(),
        Some("worker.example")
    );
    assert!(view.cells[0].vm.is_none());
    assert!(f.op.vm_for(&f.name).await.is_err());
    assert!(deletion::ensure_provider_owned(&f.op, &f.name).is_err());
    let (command, snapshot) =
        f.op.remote_command(&f.name, "sudo -n /usr/local/bin/cell relay")
            .await
            .unwrap();
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    assert!(snapshot.is_some());
    assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
    assert!(args.contains(&"ForwardAgent=no".into()));
    assert!(args.contains(&"worker.example".into()));
    let remote = args.last().unwrap();
    assert!(
        remote.contains(&f.binding.pin.machine_id)
            && remote.contains(&f.binding.pin.key.to_string())
            && remote.contains(f.name.as_str())
    );
    assert!(!args.iter().any(|a| a.contains(".exe.xyz")));
    // A disappearing binding cannot turn the registered external identity into
    // an unregistered VM that falls back to the cached inventory.
    fs::remove_file(
        f.op.config
            .tree
            .join(".hosts")
            .join(format!("{}.json", f.name)),
    )
    .unwrap();
    f.op.vms
        .write()
        .await
        .insert(f.name.clone(), "replacement".into());
    assert!(f.op.remote_command(&f.name, "true").await.is_err());
    assert!(deletion::ensure_provider_owned(&f.op, &f.name).is_err());
}

#[tokio::test]
async fn trust_key_machine_and_registry_drift_fail_closed() {
    let f = Fixture::new();
    let reviewed = identity::revision(&f.op).unwrap();
    let mut changed = f.binding.clone();
    changed.pin.machine_id = "abcdef1234567890abcdef1234567890".into();
    f.save(&changed);
    assert!(identity::check_revision(&f.op, reviewed).is_err());
    changed.pin.key = Secret::generate().public();
    f.save(&changed);
    assert!(f.op.remote_command(&f.name, "true").await.is_err());
    f.save(&f.binding);
    fs::write(&f.binding.host.known_hosts, "replaced trust").unwrap();
    assert!(f.op.remote_command(&f.name, "true").await.is_err());
    assert!(cells(State(f.op.clone())).await.is_err());
}

#[tokio::test]
async fn unsupported_authority_and_subcells_fail_before_connecting() {
    let f = Fixture::new();
    let edit = Edit {
        grants: [crate::Grant::Host(
            "door-child.int.exe.xyz".parse().unwrap(),
        )]
        .into(),
    };
    // Validation must precede binary reads and network access.
    let result = preview(State(f.op.clone()), Path(f.name.clone()), Json(edit)).await;
    assert!(result.is_err());
    let root = f.op.config.tree.clone();
    fs::create_dir(root.join(f.name.as_str())).unwrap();
    let child = Cell::new("child".parse().unwrap(), []);
    fs::write(
        root.join(f.name.as_str()).join("child.json"),
        child.canonical(),
    )
    .unwrap();
    assert!(f.op.tree().is_err());
}

#[test]
fn binding_refuses_conflicting_vm_identity_and_registration_adoption() {
    let f = Fixture::new();
    let cell = f.op.tree().unwrap().0.get(&f.name).unwrap().clone();
    assert!(crate::host_binding::registration(&f.op.config.tree, cell, f.binding.clone()).is_err());
    let mut registry = f.op.identities().unwrap();
    registry.cells[0].vm = Some(crate::identity::VmBinding {
        name: "wrong-vm".into(),
        created_at: "incarnation".into(),
    });
    registry.save(&f.op.config.tree).unwrap();
    assert!(Binding::load(&f.op.config.tree, &f.name).is_err());
}
