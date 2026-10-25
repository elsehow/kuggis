//! Operator UI and cell-scoped reverse proxy. SSH carries HTTP to each cell's
//! private Unix socket; stopping this process leaves cell sessions running.
use crate::{
    apply::{self, Edge},
    exe::Ssh,
    Approved, Cell, Hash, Name, Plan, RequestId, Tree,
};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap, fs, net::SocketAddr, os::unix::fs::PermissionsExt, path::PathBuf,
    process::Stdio, sync::Arc,
};
use tokio::{net::UnixStream, process::Command, sync::Mutex};
use tower_http::services::ServeDir;
mod access;
pub(crate) mod health;
pub(crate) mod creation;
pub(crate) mod deletion;
mod oauth;
mod identity;
#[cfg(test)]
mod hosts_tests;
pub(crate) mod resources;
pub(crate) mod resource_cleanup;
pub use access::Access;

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Local,
    Cells,
}

#[derive(Serialize, ts_rs::TS)]
pub(crate) struct CellView {
    id: String,
    binding: Name,
    can_rename: bool,
    external_host: Option<String>,
    name: Name,
    parent: Option<String>,
    grants: std::collections::BTreeSet<crate::Grant>,
    vm: Option<String>,
    lifecycle: Lifecycle,
    creation: Option<creation::Creation>,
    deletion: Option<deletion::DeletionView>,
}

/// The cell's current actionable state. Operation records remain available as
/// history, but the UI never infers availability from whichever record happens
/// to be present.
#[derive(Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Lifecycle {
    Declared,
    Provisioning { step: creation::CreationStep },
    Available,
    Deleting { step: deletion::DeletionStep },
    Blocked { message: String },
}

/// What the operator can safely ask the user to do after an operation fails.
/// Older persisted failures do not contain this field and default to retrying
/// the same approved operation.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryAction {
    #[default]
    Retry,
    Review,
}

fn lifecycle(
    creation: Option<&creation::CreationPhase>,
    deletion: Option<&deletion::DeletionPhase>,
    available: bool,
) -> Lifecycle {
    if let Some(job) = deletion {
        return match job {
            deletion::DeletionPhase::Deleting { step } => {
                Lifecycle::Deleting { step: step.clone() }
            }
            deletion::DeletionPhase::Failed { message, .. } => Lifecycle::Blocked {
                message: message.clone(),
            },
            deletion::DeletionPhase::Review => Lifecycle::Blocked { message: "Resource cleanup has started; review deletion to finish".into() },
            _ => Lifecycle::Declared,
        };
    }
    if let Some(job) = creation {
        match job {
            creation::CreationPhase::Provisioning { step } => {
                return Lifecycle::Provisioning { step: step.clone() }
            }
            creation::CreationPhase::Failed { message, .. } => {
                return Lifecycle::Blocked {
                    message: message.clone(),
                }
            }
            _ => {}
        }
    }
    if available {
        Lifecycle::Available
    } else {
        Lifecycle::Declared
    }
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CellsView {
    cells: Vec<CellView>,
    mode: Mode,
    can_apply: bool,
    can_create: bool,
    can_delete: bool,
    can_migrate_identity: bool,
}

#[derive(Serialize, ts_rs::TS)]
struct AppliedChange {
    actions: usize,
}

#[derive(Serialize, Deserialize)]
struct ErrorBody {
    error: String,
}

pub struct Config {
    pub tree: PathBuf,
    pub state: PathBuf,
    pub assets: PathBuf,
    pub listen: SocketAddr,
    pub access: Access,
    pub binary: Option<PathBuf>,
    /// Local Unix endpoints for integration tests and explicit local development.
    pub sockets: Option<PathBuf>,
}
trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}
struct Change {
    identity_revision: Option<Hash>,
    before: Tree,
    after: Tree,
    cell: Cell,
    path: PathBuf,
    plans: BTreeMap<Name, Plan>,
    host_plan: Option<crate::ssh_host::Plan>,
}
struct Operator {
    config: Config,
    authority: String,
    relays: Mutex<BTreeMap<String, oauth::Relay>>,
    vms: tokio::sync::RwLock<BTreeMap<Name, String>>,
    edge: Arc<dyn Edge + Send + Sync>,
    changes: Mutex<BTreeMap<RequestId, Change>>,
    applying: Arc<Mutex<()>>,
    stopping: tokio::sync::watch::Sender<bool>,
}
#[derive(Debug)]
struct Error(StatusCode, String);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad(e: impl ToString) -> Error {
    Error(StatusCode::BAD_REQUEST, e.to_string())
}
fn fail(e: impl ToString) -> Error {
    Error(StatusCode::BAD_GATEWAY, e.to_string())
}

impl Operator {
    fn identities(&self) -> Result<crate::identity::Registry> {
        crate::identity::Registry::load(&self.config.tree).map_err(bad)
    }
    fn resolve(&self, route: &Name) -> Result<Name> {
        let identities = self.identities()?;
        let binding = identities.by_route(route).map(|c| c.binding.clone()).unwrap_or_else(|| route.clone());
        if self.tree()?.0.get(&binding).is_none() && (identities.by_route(route).is_some() ||
            !creation::list(&self.config.state).map_err(fail)?.iter().any(|c| c.cell.name == binding && c.is_current())) {
            return Err(Error(StatusCode::NOT_FOUND, "Cell not found".into()));
        }
        identities.check_pin(&self.config.tree, &binding).map_err(bad)?;
        Ok(binding)
    }
    async fn vm_for(&self, name: &Name) -> Result<String> {
        if crate::host_binding::Binding::load(&self.config.tree, name).map_err(bad)?.is_some() {
            return Err(bad("External hosts cannot use an exe.dev VM route"));
        }
        let registry = self.identities()?;
        if let Some(identity) = registry.by_binding(name) {
            let expected = identity.vm.clone().ok_or_else(|| bad("Local identity cannot target a remote VM"))?;
            let edge = self.edge.clone(); let binding = name.clone();
            return tokio::task::spawn_blocking(move || {
                let (vms, _) = edge.listing().map_err(|e| e.to_string())?;
                let bindings = crate::exe::cell_vms(&vms).map_err(|e| e.to_string())?;
                let vm = bindings.get(&binding).ok_or("Cell VM is unavailable")?;
                if vm.vm_name != expected.name || vm.created_at.as_deref() != Some(&expected.created_at) {
                    return Err("Cell VM incarnation changed; explicit identity repair is required".to_owned());
                }
                Ok(vm.vm_name.clone())
            }).await.map_err(fail)?.map_err(bad);
        }
        self.vms.read().await.get(name).cloned().ok_or_else(|| Error(StatusCode::SERVICE_UNAVAILABLE, "Cell has no VM; apply its declaration first".into()))
    }
    async fn remote_command(&self, name: &Name, command: &str) -> Result<(Command, Option<crate::ssh_host::TrustSnapshot>)> {
        self.identities()?.check_pin(&self.config.tree, name).map_err(bad)?;
        if let Some(binding) = crate::host_binding::Binding::load(&self.config.tree, name).map_err(bad)? {
            let (command, snapshot) = binding.command(command).map_err(bad)?;
            return Ok((Command::from(command), Some(snapshot)));
        }
        let vm = self.vm_for(name).await?;
        let guarded = match self.identities()?.by_binding(name) {
            Some(identity) => format!("test \"$(cat /etc/cell/key.pub)\" = '{}' && {command}", identity.key),
            None => command.to_owned(),
        };
        let mut remote = Command::new("ssh");
        remote.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"])
            .arg(format!("{vm}.exe.xyz")).arg(guarded);
        Ok((remote, None))
    }
    fn tree(&self) -> Result<(Tree, BTreeMap<Name, PathBuf>)> {
        let loaded = crate::tree::load(&self.config.tree).map_err(bad)?;
        self.identities()?.check_active(&self.config.tree, &loaded.0).map_err(bad)?;
        Ok(loaded)
    }
    async fn forward(&self, name: &Name, mut request: Request) -> Result<Response> {
        self.identities()?.check_pin(&self.config.tree, name).map_err(bad)?;
        deletion::ensure_available(&self.config.state, name)?;
        if self.tree()?.0.get(name).is_none() {
            return Err(Error(StatusCode::NOT_FOUND, "Cell not found".into()));
        }
        let mut child = None;
        let mut trust = None;
        let stream: Box<dyn Transport> = if let Some(dir) = &self.config.sockets {
            Box::new(
                UnixStream::connect(dir.join(format!("{name}.sock")))
                    .await
                    .map_err(fail)?,
            )
        } else {
            // exe.dev keeps its existing group-authorized relay. An external
            // bootstrap account reaches the private socket through sudo.
            let relay = if crate::host_binding::Binding::load(&self.config.tree, name).map_err(bad)?.is_some() {
                "sudo -n /usr/local/bin/cell relay"
            } else {
                "/usr/local/bin/cell relay"
            };
            let (mut command, snapshot) = self.remote_command(name, relay).await?;
            trust = snapshot;
            let mut process = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .map_err(fail)?;
            let stream = tokio::io::join(
                process.stdout.take().unwrap(),
                process.stdin.take().unwrap(),
            );
            child = Some(process);
            Box::new(stream)
        };
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(fail)?;
        let mut stopping = self.stopping.subscribe();
        tokio::spawn(async move {
            let _trust = trust;
            tokio::select! { _ = connection => {}, _ = stopping.changed() => {} }
            if let Some(mut child) = child {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        });
        access::strip_browser_headers(request.headers_mut());
        request
            .headers_mut()
            .insert(header::HOST, "cell.internal".parse().unwrap());
        let response = sender.send_request(request).await.map_err(|_| {
            fail("Chat service unavailable. Check the cell's chat health and recovery options.")
        })?;
        let (parts, body) = response.into_parts();
        Ok(Response::from_parts(parts, Body::new(body)))
    }

    async fn refresh_vms(&self) -> Result<()> {
        if self.config.sockets.is_some() {
            return Ok(());
        }
        let (tree, _) = self.tree()?;
        if tree.iter().next().is_some() && tree.iter().all(|c| crate::host_binding::Binding::load(&self.config.tree, &c.name).is_ok_and(|b| b.is_some())) {
            self.vms.write().await.clear();
            return Ok(());
        }
        let edge = self.edge.clone();
        let vms = tokio::task::spawn_blocking(move || {
            edge.listing()
                .and_then(|(vms, _)| {
                    crate::exe::cell_vms(&vms)
                        .map(|bindings| {
                            bindings
                                .into_iter()
                                .map(|(name, vm)| (name, vm.vm_name.clone()))
                                .collect()
                        })
                        .map_err(Into::into)
                })
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(fail)?
        .map_err(fail)?;
        *self.vms.write().await = vms;
        Ok(())
    }
}

async fn cells(State(op): State<Arc<Operator>>) -> Result<Json<CellsView>> {
    let (tree, _) = op.tree()?;
    let identities = op.identities()?;
    for cell in tree.iter() { identities.check_pin(&op.config.tree, &cell.name).map_err(bad)?; }
    let jobs = creation::list(&op.config.state).map_err(fail)?;
    let deletions = deletion::list(&op.config.state).map_err(fail)?;
    let hosts = tree.iter().map(|c| Ok((c.name.clone(), crate::host_binding::Binding::load(&op.config.tree, &c.name).map_err(bad)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let vms = op.vms.read().await;
    if hosts.iter().any(|(name, host)| host.is_some() && vms.contains_key(name)) {
        return Err(bad("An external cell also has an exe.dev VM binding"));
    }
    let row = |cell: &Cell, parent: Option<Name>, creation: Option<&creation::Creation>| {
        let deletion = deletions
            .iter()
            .find(|j| j.cell().name == cell.name && j.active());
        let vm = vms.get(&cell.name).cloned();
        let socket_available = op
            .config
            .sockets
            .as_ref()
            .is_some_and(|dir| dir.join(format!("{}.sock", cell.name)).exists());
        let lifecycle = lifecycle(
            creation.map(|j| &j.phase),
            deletion.map(|j| &j.phase),
            vm.is_some() || socket_available || hosts.get(&cell.name).is_some_and(Option::is_some),
        );
        CellView {
            id: identities.route(&cell.name),
            binding: cell.name.clone(),
            can_rename: identities.by_binding(&cell.name).is_some(),
            external_host: hosts.get(&cell.name).and_then(|h| h.as_ref()).map(|h| h.host.hostname.clone()),
            name: identities.by_binding(&cell.name).map(|c| c.name.clone()).or_else(|| creation.and_then(|c| c.display_name.clone())).unwrap_or_else(|| cell.name.clone()),
            parent: parent.map(|p| identities.route(&p)),
            grants: cell.grants.clone(),
            vm,
            lifecycle,
            creation: creation.cloned(),
            deletion: deletion.map(|j| j.view()),
        }
    };
    let mut cells: Vec<_> = tree
        .iter()
        .map(|cell| {
            row(
                cell,
                tree.parent(&cell.name).map(|c| c.name.clone()),
                jobs.iter()
                    .find(|j| j.cell.name == cell.name && j.is_current()),
            )
        })
        .collect();
    for job in jobs.iter().filter(|j| {
        matches!(
            j.phase,
            creation::CreationPhase::Provisioning { .. } | creation::CreationPhase::Failed { .. }
        ) && tree.get(&j.cell.name).is_none()
    }) {
        cells.push(row(&job.cell, job.parent.clone(), Some(job)));
    }
    for job in deletions.iter().filter(|j| j.active()) {
        if !cells.iter().any(|c| c.binding == job.cell().name) {
            cells.push(row(job.cell(), job.parent().clone(), None));
        }
    }
    let can_migrate_identity = !op.config.tree.join(".identities.json").exists() || tree.iter().any(|c| identities.by_binding(&c.name).is_none());
    Ok(Json(CellsView {
        cells,
        mode: if op.config.sockets.is_some() {
            Mode::Local
        } else {
            Mode::Cells
        },
        can_apply: op.config.binary.is_some(),
        can_create: op.config.binary.is_some() && op.config.sockets.is_none(),
        can_delete: op.config.sockets.is_none(),
        can_migrate_identity,
    }))
}

async fn proxy(
    State(op): State<Arc<Operator>>,
    Path((name, tail)): Path<(Name, String)>,
    mut request: Request,
) -> Result<Response> {
    let name = op.resolve(&name)?;
    // Only the cell API is reachable. The caller cannot supply a remote path,
    // hostname, SSH command, or arbitrary VM target.
    if !["pi/", "sessions/", "settings", "info"]
        .iter()
        .any(|p| tail.starts_with(p))
    {
        return Err(Error(StatusCode::NOT_FOUND, "No such API".into()));
    }
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    *request.uri_mut() = format!("/api/{tail}{query}").parse().map_err(bad)?;
    let auth_view = request.method() == axum::http::Method::GET
        && tail.starts_with("settings/providers/")
        && tail.contains("/login/");
    let path = request.uri().path().to_owned();
    let response = op.forward(&name, request).await?;
    if auth_view && response.status().is_success() {
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .map_err(|_| fail("Invalid sign-in status"))?;
        let mut view: crate::chat::oauth::AuthFlow =
            serde_json::from_slice(&bytes).map_err(|_| fail("Invalid sign-in status"))?;
        oauth::prepare(&op, &name, &path, &mut view).await;
        return Ok(Json(view).into_response());
    }
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    grants: std::collections::BTreeSet<crate::Grant>,
}
#[derive(Serialize, ts_rs::TS)]
pub(crate) struct Preview {
    id: RequestId,
    approval: Hash,
    plans: BTreeMap<Name, Plan>,
}

async fn preview(
    State(op): State<Arc<Operator>>,
    Path(name): Path<Name>,
    Json(edit): Json<Edit>,
) -> Result<Json<Preview>> {
    let identity_revision = identity::revision(&op)?;
    let name = op.resolve(&name)?;
    deletion::ensure_available(&op.config.state, &name)?;
    let (before, paths) = op.tree()?;
    let path = paths
        .get(&name)
        .ok_or_else(|| bad("Cell not found"))?
        .clone();
    let cell = Cell::new(name.clone(), edit.grants);
    let after = Tree::new(before.iter().map(|c| {
        (
            if c.name == name {
                cell.clone()
            } else {
                c.clone()
            },
            before.parent(&c.name).map(|p| p.name.clone()),
        )
    }))
    .map_err(bad)?;
    op.config
        .binary
        .as_ref()
        .ok_or_else(|| bad("Start cell ui with --bin LINUX_CELL to apply configuration"))?;
    let binding = crate::host_binding::Binding::load(&op.config.tree, &name).map_err(bad)?;
    let (plans, host_plan) = if let Some(binding) = binding {
        binding.host.runtime.validate_cell(&cell).map_err(bad)?;
        let binary_path = op.config.binary.clone().unwrap();
        let cell = cell.clone();
        let plan = tokio::task::spawn_blocking(move || {
            let binary = crate::ssh_host::linux_binary(&binary_path).map_err(|e| e.to_string())?;
            binding.plan(cell, &binary).map_err(|e| e.to_string())
        }).await.map_err(fail)?.map_err(bad)?;
        let view = Plan { want: plan.cell.hash(), have: plan.before.cell.as_ref().map(Cell::hash).unwrap_or(Hash::ZERO), bin: plan.binary,
            actions: plan.actions.iter().map(|_| crate::Action::Setup).collect() };
        ([(name, view)].into(), Some(plan))
    } else {
        let pins = op.config.tree.join(".pins");
        let edge = op.edge.clone();
        let target = after.clone();
        let names = vec![name];
        let plans = tokio::task::spawn_blocking(move || {
            apply::plans(edge.as_ref(), &pins, &target, &names).map_err(|e| e.to_string())
        }).await.map_err(fail)?.map_err(fail)?;
        (plans, None)
    };
    let id = RequestId::generate();
    let result = Preview {
        id,
        approval: host_plan.as_ref().map(crate::ssh_host::Plan::hash).unwrap_or_else(|| Approved::hash(&plans)),
        plans: plans.clone(),
    };
    op.changes.lock().await.insert(
        id,
        Change {
            identity_revision,
            before,
            after,
            cell,
            path,
            plans,
            host_plan,
        },
    );
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Apply {
    approval: Hash,
}
async fn apply_change(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
    Json(body): Json<Apply>,
) -> Result<Json<AppliedChange>> {
    let _lock = op.applying.lock().await;
    let change = op
        .changes
        .lock()
        .await
        .remove(&id)
        .ok_or_else(|| bad("Preview expired; review the change again"))?;
    identity::check_revision(&op, change.identity_revision)?;
    deletion::ensure_available(&op.config.state, &change.cell.name)?;
    if op.tree()?.0 != change.before {
        return Err(Error(
            StatusCode::CONFLICT,
            "The tree changed; review the change again".into(),
        ));
    }
    let approved = if let Some(plan) = &change.host_plan {
        if plan.hash() != body.approval { return Err(bad("Host plan approval mismatch")); }
        None
    } else { Some(Approved::new(change.plans.clone(), &body.approval).map_err(bad)?) };
    // Never restart a cell that has an active conversation.
    let request = Request::builder()
        .uri("/api/pi/threads?includeArchived=true")
        .body(Body::empty())
        .unwrap();
    let response = op.forward(&change.cell.name, request).await?;
    if !response.status().is_success() {
        return Err(fail("Cannot inspect active cell sessions"));
    }
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(fail)?;
    check_idle_sessions(&bytes, &change.cell.name)?;
    if op.tree()?.0 != change.before {
        return Err(Error(
            StatusCode::CONFLICT,
            "The tree changed; review the change again".into(),
        ));
    }
    if let Some(plan) = change.host_plan {
        let binding = crate::host_binding::Binding::load(&op.config.tree, &change.cell.name).map_err(bad)?
            .ok_or_else(|| bad("External host binding disappeared"))?;
        let binary_path = op.config.binary.clone().ok_or_else(|| bad("Deployment binary missing"))?;
        let state = op.config.state.join("hosts").join(change.cell.name.as_str());
        let actions = tokio::task::spawn_blocking(move || {
            let binary = crate::ssh_host::linux_binary(&binary_path).map_err(|e| e.to_string())?;
            binding.apply(&plan, body.approval, &binary, &state).map_err(|e| e.to_string())
        }).await.map_err(fail)?.map_err(fail)?;
        crate::identity::atomic(&change.path, &change.cell.canonical()).map_err(fail)?;
        return Ok(Json(AppliedChange { actions }));
    }
    let path = change.path;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, change.cell.canonical()).map_err(fail)?;
    fs::rename(tmp, path).map_err(fail)?;
    let edge = op.edge.clone();
    let pins = op.config.tree.join(".pins");
    let actions = tokio::task::spawn_blocking(move || {
        apply::apply(
            edge.as_ref(),
            &pins,
            &change.after,
            &approved.expect("exe.dev plan"),
            &mut Vec::new(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(fail)?
    .map_err(fail)?;
    Ok(Json(AppliedChange { actions }))
}

fn check_idle_sessions(bytes: &[u8], cell: &Name) -> Result<()> {
    use crate::chat::types::{RunStatus, SessionSummary};
    let sessions: Vec<SessionSummary> = serde_json::from_slice(bytes).map_err(fail)?;
    if sessions.iter().any(|s| &s.cell != cell) {
        return Err(fail("Cell returned sessions belonging to another cell"));
    }
    if sessions.iter().any(|s| s.status == RunStatus::Running) {
        return Err(Error(
            StatusCode::CONFLICT,
            "Stop active runs before applying settings".into(),
        ));
    }
    Ok(())
}

async fn guard(State(op): State<Arc<Operator>>, request: Request, next: Next) -> Response {
    if !op.config.access.allows(&request, &op.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'self'".parse().unwrap());
    response
}

pub async fn serve(mut config: Config) -> crate::apply::Fallible {
    if !config.listen.ip().is_loopback() {
        return Err("UI must listen on loopback".into());
    }
    config.tree = config.tree.canonicalize()?;
    crate::tree::load(&config.tree)?;
    fs::create_dir_all(&config.state)?;
    config.state = config.state.canonicalize()?;
    fs::set_permissions(&config.state, fs::Permissions::from_mode(0o700))?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.state.join("lock"))?;
    lock.try_lock()
        .map_err(|_| "another UI owns this state directory")?;
    creation::recover(&config.state)?;
    deletion::recover(&config.state)?;
    let edge = Arc::new(Ssh {
        bin: config.binary.clone(),
    });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let authority = listener.local_addr()?.to_string();
    let op = Arc::new(Operator {
        config,
        authority,
        relays: Mutex::new(BTreeMap::new()),
        vms: tokio::sync::RwLock::new(BTreeMap::new()),
        edge,
        changes: Mutex::new(BTreeMap::new()),
        applying: Arc::new(Mutex::new(())),
        stopping: tokio::sync::watch::channel(false).0,
    });
    op.refresh_vms().await.map_err(|e| e.1)?;
    if op.config.sockets.is_none() {
        tokio::spawn(context_loop(op.clone()));
    }
    let app = router(op.clone());
    eprintln!("Kuggis UI: {}", op.config.access.origin(&op.authority));
    let shutdown = op.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            shutdown.stopping.send_replace(true);
        })
        .await?;
    drop(lock);
    Ok(())
}

/// Reconcile discovery independently of configuration hashes (names are mutable).
/// Failed publications are retried, including after operator restart. Hold the
/// mutation lock while publishing so an older snapshot cannot overwrite a rename.
async fn context_loop(op: Arc<Operator>) {
    let mut publishing = None;
    let mut publishing_at = std::time::Instant::now() - std::time::Duration::from_secs(61);
    let mut publishing_tree = None;
    let mut sent = BTreeMap::new();
    loop {
        if *op.stopping.borrow() {
            break;
        }
        {
            let _mutation = op.applying.lock().await;
            let result = async {
                op.refresh_vms().await?;
                let (tree, _) = op.tree()?;
                let pins = op.config.tree.join(".pins");
                let fingerprint = Hash::of(&crate::canon::bytes(&tree.iter().map(|c| (c, tree.parent(&c.name).map(|p| &p.name))).collect::<Vec<_>>()));
                if publishing_tree != Some(fingerprint) || publishing_at.elapsed().as_secs() >= 60 {
                    let worker = op.clone();
                    publishing = match tokio::task::spawn_blocking(move || resources::publishing(&worker)).await {
                        Ok(Ok(discovery)) => discovery,
                        failure => {
                            eprintln!("publisher reconciliation failed: {failure:?}");
                            Some(tree.iter().map(|c| (c.name.clone(), crate::publish::provision::Discovery::unavailable("Publisher connection unavailable; the operator must restore it"))).collect())
                        }
                    };
                    publishing_tree = Some(fingerprint);
                    publishing_at = std::time::Instant::now();
                }
                for cell in tree.iter() {
                    let host = crate::host_binding::Binding::load(&op.config.tree, &cell.name).map_err(bad)?;
                    let vm = match if let Some(host) = &host { Ok(format!("ssh:{}", Hash::of(&crate::canon::bytes(host)))) } else { op.vm_for(&cell.name).await } {
                        Ok(vm) => vm,
                        Err(e) => { eprintln!("cell {} context target unavailable: {}", cell.name, e.1); continue; }
                    };
                    if deletion::ensure_available(&op.config.state, &cell.name).is_err() {
                        continue;
                    }
                    let mut context = match crate::context::Context::available(&tree, &cell.name, &pins) {
                        Ok(Some(context)) => context,
                        Ok(None) => continue,
                        Err(e) => {
                            eprintln!("cell {} context unavailable: {e}", cell.name);
                            continue;
                        }
                    };
                    context.publishing = if host.is_some() { None } else { publishing.as_ref().and_then(|all| all.get(&cell.name)).cloned() };
                    let value = (vm.clone(), context.clone());
                    if sent
                        .get(&cell.name)
                        .is_some_and(|(old, at): &(_, std::time::Instant)| {
                            old == &value && at.elapsed().as_secs() < 60
                        })
                    {
                        continue;
                    }
                    if let Some(host) = host {
                        let payload = crate::canon::bytes(&context);
                        let result = tokio::task::spawn_blocking(move || -> std::result::Result<(), String> {
                            use std::io::Write;
                            let (mut command, _trust) = host.command(r#"sudo -n sh -c 'set -eu; mkdir -p /usr/local/lib/cell; t=$(mktemp /usr/local/lib/cell/.context.XXXXXX); trap "rm -f \"$t\"" EXIT; cat > "$t"; chmod 644 "$t"; mv -f "$t" /usr/local/lib/cell/context.json'"#).map_err(|e| e.to_string())?;
                            let mut child = command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
                            let mut input = child.stdin.take().unwrap();
                            let written = input.write_all(&payload); drop(input);
                            let status = child.wait().map_err(|e| e.to_string())?;
                            written.map_err(|e| e.to_string())?;
                            if !status.success() { return Err("external cell context publication failed".into()); }
                            Ok(())
                        }).await;
                        if matches!(result, Ok(Ok(()))) { sent.insert(cell.name.clone(), (value, std::time::Instant::now())); }
                        else { eprintln!("cell {} context publication failed: {result:?}", cell.name); }
                        continue;
                    }
                    let edge = op.edge.clone();
                    let target = vm.clone();
                    match tokio::task::spawn_blocking(move || {
                        edge.context(&target, &context).map_err(|e| e.to_string())
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            sent.insert(cell.name.clone(), (value, std::time::Instant::now()));
                        }
                        failure => eprintln!(
                            "cell {} context publication failed; will retry: {:?}",
                            cell.name, failure
                        ),
                    }
                }
                sent.retain(|id, _| tree.get(id).is_some());
                Ok::<_, Error>(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("cell context refresh: {}", e.1);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

fn router(op: Arc<Operator>) -> Router {
    let assets = op.config.assets.clone();
    Router::new()
        .route("/api/cells", get(cells))
        .route("/api/identity/plan", post(identity::plan))
        .route("/api/identity/migrations/:id", get(identity::get))
        .route("/api/identity/migrations/:id/apply", post(identity::apply))
        .route("/api/cells/:name/rename", post(identity::rename))
        .route("/api/resources", get(resources::list))
        .route("/api/publishing", get(resources::publishing_endpoint))
        .route("/api/resources/plan", post(resources::plan))
        .route("/api/resources/register-plan", post(resources::register_plan))
        .route("/api/resources/actions/:id", get(resources::get))
        .route("/api/resources/actions/:id/apply", post(resources::apply))
        .route("/api/cells/plan", post(creation::preview))
        .route("/api/creations/:id", get(creation::get))
        .route("/api/creations/:id/apply", post(creation::start))
        .route("/api/cells/:name/plan", post(preview))
        .route("/api/cells/:name/health", get(health::get))
        .route("/api/cells/:name/recovery", post(health::recover))
        .route("/api/cells/:name/delete-plan", post(deletion::preview_request))
        .route("/api/deletions/:id", get(deletion::get))
        .route("/api/deletions/:id/apply", post(deletion::start))
        .route("/api/changes/:id/apply", post(apply_change))
        .route("/api/cells/:name/*tail", any(proxy))
        .fallback_service(ServeDir::new(assets))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(op.clone(), guard))
        .with_state(op.clone())
}

/// Bridge stdin/stdout to one local Unix socket. OS permissions authorize the
/// operator. It neither starts Pi nor reads any credentials.
pub fn relay(socket: &std::path::Path) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut remote = std::os::unix::net::UnixStream::connect(socket)?;
    let mut writer = remote.try_clone()?;
    // A detached input thread lets the command exit when the server closes,
    // even if SSH is still holding stdin open. Tokio's blocking stdin would
    // keep its runtime waiting after the HTTP connection had ended.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
        let _ = writer.shutdown(std::net::Shutdown::Write);
    });
    let mut output = std::io::stdout().lock();
    let mut buffer = [0; 8192];
    loop {
        let n = remote.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n])?;
        // JSON bodies need not end in LF; stdout's line buffering must not
        // hold a response until the next SSE event or HTTP connection close.
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_uses_current_observation_and_active_operation() {
        use creation::{CreationPhase as C, CreationStep as CS};
        use deletion::{DeletionPhase as D, DeletionStep as DS};
        assert_eq!(lifecycle(None, None, false), Lifecycle::Declared);
        assert_eq!(lifecycle(None, None, true), Lifecycle::Available);
        assert_eq!(
            lifecycle(
                Some(&C::Provisioning {
                    step: CS::CreatingVm
                }),
                None,
                true
            ),
            Lifecycle::Provisioning {
                step: CS::CreatingVm
            }
        );
        assert_eq!(
            lifecycle(
                Some(&C::Ready {
                    session: crate::SessionRef {
                        cell: "dev".parse().unwrap(),
                        id: crate::SessionId::generate()
                    }
                }),
                None,
                false
            ),
            Lifecycle::Declared
        );
        assert_eq!(
            lifecycle(
                Some(&C::Ready {
                    session: crate::SessionRef {
                        cell: "dev".parse().unwrap(),
                        id: crate::SessionId::generate()
                    }
                }),
                Some(&D::Deleting {
                    step: DS::DeletingVm
                }),
                true
            ),
            Lifecycle::Deleting {
                step: DS::DeletingVm
            }
        );
        assert_eq!(
            lifecycle(
                None,
                Some(&D::Failed {
                    message: "drift".into(),
                    step: None,
                    recovery: RecoveryAction::Retry
                }),
                true
            ),
            Lifecycle::Blocked {
                message: "drift".into()
            }
        );
    }

    #[test]
    fn restart_requires_complete_session_status_and_correct_owner() {
        let cell: Name = "dev".parse().unwrap();
        let session = json!({"id":crate::SessionId::generate(),"cell":"dev","status":"idle","title":"Ignored extra metadata"});
        let check = |value| check_idle_sessions(&serde_json::to_vec(&value).unwrap(), &cell);
        assert!(check(json!([])).is_ok());
        assert!(check(json!([session])).is_ok());
        for status in [Value::Null, json!(false), json!("unknown")] {
            let mut malformed = session.clone();
            malformed["status"] = status;
            assert_eq!(
                check(json!([malformed])).unwrap_err().0,
                StatusCode::BAD_GATEWAY
            );
        }
        let mut malformed = session.clone();
        malformed.as_object_mut().unwrap().remove("status");
        assert!(check(json!([malformed])).is_err());
        let mut running = session.clone();
        running["status"] = json!("running");
        assert_eq!(check(json!([running])).unwrap_err().0, StatusCode::CONFLICT);
        let mut other = session;
        other["cell"] = json!("other");
        assert_eq!(
            check(json!([other])).unwrap_err().0,
            StatusCode::BAD_GATEWAY
        );
    }
}
