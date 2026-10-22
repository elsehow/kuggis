//! Resource gateway. Connections come only from private operator configuration;
//! request bodies cannot introduce filesystem paths, SSH destinations or keys.
use super::*;
use crate::publish::registry::{resources::ResourceInventory, Registry};
use crate::{
    resource::*,
    resource_rpc::{self, Command as Rpc, Reply},
};
use std::{
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path as FsPath,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Location {
    Local { registry: PathBuf },
    Ssh { vm: crate::exe::VmName },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Backend {
    pub name: Name,
    pub provider: ProviderRef,
    location: Location,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connections {
    backends: Vec<Backend>,
}
pub(super) fn backends(tree: &FsPath) -> Result<Vec<Backend>> {
    let path = tree.join(".resources.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(fail(e)),
    };
    if !metadata.is_file() || metadata.permissions().mode() & 0o022 != 0 {
        return Err(bad(
            "Resource connections must be a regular operator-owned file, not group/world writable",
        ));
    }
    let config: Connections =
        serde_json::from_slice(&fs::read(path).map_err(fail)?).map_err(bad)?;
    let mut names = std::collections::BTreeSet::new();
    let mut providers = std::collections::BTreeSet::new();
    for b in &config.backends {
        if !names.insert(b.name.clone()) || !providers.insert(b.provider) {
            return Err(bad(
                "Resource backend names and provider identities must be unique",
            ));
        }
    }
    Ok(config.backends)
}
impl Backend {
    pub(super) fn rpc(&self, command: Rpc) -> Result<Reply> {
        let reply = match &self.location {
            Location::Local { registry } => {
                resource_rpc::execute(&Registry::open(registry).map_err(fail)?, command)
                    .map_err(fail)?
            }
            Location::Ssh { vm } => {
                let bytes = serde_json::to_vec(&command).map_err(fail)?;
                if bytes.len() > resource_rpc::MAX_RPC_BYTES {
                    return Err(bad("Resource command exceeds limit"));
                }
                let mut child=std::process::Command::new("ssh").args(["-T","-o","BatchMode=yes","-o","ConnectTimeout=10","-o","ServerAliveInterval=5","-o","ServerAliveCountMax=2"])
     .arg(format!("{vm}.exe.xyz")).arg("sudo -n /usr/local/bin/cell publisher resource-rpc /var/lib/cell-publisher")
     .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(fail)?;
                let mut input = child.stdin.take().unwrap();
                let output = child.stdout.take().unwrap();
                let errors = child.stderr.take().unwrap();
                let writer = std::thread::spawn(move || input.write_all(&bytes));
                let reader = std::thread::spawn(move || {
                    let mut b = vec![];
                    output
                        .take(resource_rpc::MAX_RPC_BYTES as u64 + 1)
                        .read_to_end(&mut b)
                        .map(|_| b)
                });
                let error_reader = std::thread::spawn(move || {
                    let mut b = vec![];
                    errors.take(8193).read_to_end(&mut b).map(|_| b)
                });
                let deadline = Instant::now() + Duration::from_secs(30);
                let status = loop {
                    match child.try_wait().map_err(fail)? {
                        Some(status) => break status,
                        None if Instant::now() >= deadline => {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(fail("Resource backend timed out; outcome may be unknown. Retry the same operation."));
                        }
                        None => std::thread::sleep(Duration::from_millis(20)),
                    }
                };
                let written = writer
                    .join()
                    .map_err(|_| fail("Resource input worker failed"))?;
                let bytes = reader
                    .join()
                    .map_err(|_| fail("Resource output worker failed"))?
                    .map_err(fail)?;
                let errors = error_reader
                    .join()
                    .map_err(|_| fail("Resource error worker failed"))?
                    .map_err(fail)?;
                if !status.success() {
                    return Err(fail(format!(
                        "Resource backend failed: {}",
                        String::from_utf8_lossy(&errors)
                    )));
                }
                written.map_err(fail)?;
                if bytes.len() > resource_rpc::MAX_RPC_BYTES {
                    return Err(fail("Resource backend response exceeds limit"));
                }
                serde_json::from_slice(&bytes).map_err(fail)?
            }
        };
        Ok(reply)
    }
    pub(super) fn inventory(&self) -> Result<ResourceInventory> {
        match self.rpc(Rpc::Inventory)? {
            Reply::Inventory(i) if i.provider == self.provider => Ok(i),
            _ => Err(fail(
                "Resource backend identity does not match configuration",
            )),
        }
    }
}
#[derive(Serialize, ts_rs::TS)]
pub(crate) struct BackendView {
    pub name: Name,
    pub provider: ProviderRef,
    pub inventory: Option<ResourceInventory>,
    pub error: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub(crate) struct ResourceCell {
    pub name: Name,
    pub identity: CellIdentity,
}
#[derive(Serialize, ts_rs::TS)]
pub(crate) struct ResourcesView {
    pub backends: Vec<BackendView>,
    pub cells: Vec<ResourceCell>,
}
fn cell_targets(op: &Operator) -> Result<Vec<ResourceCell>> {
    let tree = op.tree()?.0;
    let mut cells = vec![];
    for cell in tree.iter() {
        if deletion::ensure_available(&op.config.state, &cell.name).is_err() {
            continue;
        }
        let path = op
            .config
            .tree
            .join(".pins")
            .join(format!("{}.pub", cell.name));
        match fs::read_to_string(path) {
            Ok(pin) => cells.push(ResourceCell {
                name: cell.name.clone(),
                identity: CellIdentity(pin.parse().map_err(bad)?),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(fail(e)),
        }
    }
    Ok(cells)
}
fn validate_target(op: &Operator, intent: &ResourceIntent) -> Result<()> {
    if let ResourceIntent::Transfer {
        to: ResourceOwner::Cell(identity),
        ..
    } = intent
    {
        if !cell_targets(op)?.iter().any(|c| &c.identity == identity) {
            return Err(bad(
                "Transfer target must be a currently pinned, non-deleting cell",
            ));
        }
    }
    Ok(())
}
pub(super) async fn list(State(op): State<Arc<Operator>>) -> Result<Json<ResourcesView>> {
    let backends = backends(&op.config.tree)?;
    let cells = cell_targets(&op)?;
    let backends = tokio::task::spawn_blocking(move || {
        backends
            .into_iter()
            .map(|b| match b.inventory() {
                Ok(inventory) => BackendView {
                    name: b.name,
                    provider: b.provider,
                    inventory: Some(inventory),
                    error: None,
                },
                Err(error) => BackendView {
                    name: b.name,
                    provider: b.provider,
                    inventory: None,
                    error: Some(error.1),
                },
            })
            .collect()
    })
    .await
    .map_err(fail)?;
    Ok(Json(ResourcesView { backends, cells }))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    id: RequestId,
    backend: Backend,
    plan: ResourcePlan,
    approval: Hash,
    receipt: Option<ResourceOperation>,
}
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub(crate) struct ActionView {
    pub id: RequestId,
    pub backend: Name,
    pub plan: ResourcePlan,
    pub approval: Hash,
    pub receipt: Option<ResourceOperation>,
}
impl Action {
    fn view(&self) -> ActionView {
        ActionView {
            id: self.id,
            backend: self.backend.name.clone(),
            plan: self.plan.clone(),
            approval: self.approval,
            receipt: self.receipt.clone(),
        }
    }
    fn hash(backend: &Backend, plan: &ResourcePlan) -> Hash {
        Hash::of(&crate::canon::bytes(&(backend, plan)))
    }
    fn path(state: &FsPath, id: RequestId) -> PathBuf {
        state.join("resource-actions").join(format!("{id}.json"))
    }
    fn save(&self, state: &FsPath) -> Result<()> {
        let path = Self::path(state, self.id);
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent).map_err(fail)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(fail)?;
        let temp = parent.join(format!(".{}", RequestId::generate()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(fail)?;
        file.write_all(&serde_json::to_vec(self).map_err(fail)?)
            .map_err(fail)?;
        file.sync_all().map_err(fail)?;
        fs::rename(&temp, &path).map_err(fail)?;
        fs::File::open(parent)
            .map_err(fail)?
            .sync_all()
            .map_err(fail)?;
        Ok(())
    }
    fn load(state: &FsPath, id: RequestId) -> Result<Self> {
        let job: Self = serde_json::from_slice(&fs::read(Self::path(state, id)).map_err(fail)?)
            .map_err(fail)?;
        if job.id != id || job.approval != Self::hash(&job.backend, &job.plan) {
            return Err(fail("Resource action record does not match approval"));
        }
        Ok(job)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanBody {
    backend: Name,
    resource: ResourceId,
    intent: ResourceIntent,
}
pub(super) async fn plan(
    State(op): State<Arc<Operator>>,
    Json(body): Json<PlanBody>,
) -> Result<Json<ActionView>> {
    let _lock = op
        .applying
        .try_lock()
        .map_err(|_| bad("Another operator change is in progress"))?;
    validate_target(&op, &body.intent)?;
    let backend = backends(&op.config.tree)?
        .into_iter()
        .find(|b| b.name == body.backend)
        .ok_or_else(|| bad("Unknown resource backend"))?;
    let call = backend.clone();
    let plan = tokio::task::spawn_blocking(move || {
        call.inventory()?;
        match call.rpc(Rpc::Plan {
            resource: body.resource,
            intent: body.intent,
        })? {
            Reply::Plan(plan) => Ok(plan),
            _ => Err(fail("Unexpected resource plan response")),
        }
    })
    .await
    .map_err(fail)??;
    plan.validate().map_err(bad)?;
    let action = Action {
        id: RequestId::generate(),
        approval: Action::hash(&backend, &plan),
        backend,
        plan,
        receipt: None,
    };
    action.save(&op.config.state)?;
    Ok(Json(action.view()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplyBody {
    approval: Hash,
}
pub(super) async fn get(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
) -> Result<Json<ActionView>> {
    Ok(Json(Action::load(&op.config.state, id)?.view()))
}
pub(super) async fn apply(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
    Json(body): Json<ApplyBody>,
) -> Result<Json<ActionView>> {
    let _lock = op
        .applying
        .try_lock()
        .map_err(|_| bad("Another operator change is in progress"))?;
    let mut action = Action::load(&op.config.state, id)?;
    if body.approval != action.approval {
        return Err(bad("Resource approval mismatch"));
    }
    if action.receipt.is_some() {
        return Ok(Json(action.view()));
    }
    if !backends(&op.config.tree)?.contains(&action.backend) {
        return Err(bad("Resource connection changed; review again"));
    }
    let worker = op.clone();
    let backend = action.backend.clone();
    let plan = action.plan.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        backend.inventory()?;
        match backend.rpc(Rpc::OperationReceipt { operation: id })? {
            Reply::OperationReceipt(Some(receipt)) => return Ok(receipt),
            Reply::OperationReceipt(None) => (),
            _ => return Err(fail("Unexpected resource receipt response")),
        }
        validate_target(&worker, &plan.intent)?;
        match backend.rpc(Rpc::Apply {
            operation: id,
            approval: plan.approval(),
            plan,
        })? {
            Reply::Applied(receipt) => Ok(receipt),
            _ => Err(fail("Unexpected resource action response")),
        }
    })
    .await
    .map_err(fail)??;
    if receipt.id != id
        || receipt.approval != action.plan.approval()
        || receipt.resource != action.plan.resource.id
        || receipt.phase != OperationPhase::Succeeded
    {
        return Err(fail("Invalid resource operation receipt"));
    }
    action.receipt = Some(receipt);
    action.save(&op.config.state)?;
    Ok(Json(action.view()))
}
