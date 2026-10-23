//! Operator UI and cell-scoped reverse proxy. SSH carries HTTP to each cell's
//! private Unix socket; stopping this process leaves cell sessions running.
use crate::{
    apply::{self, Edge},
    exe::Ssh,
    Approved, Cell, CellId, Hash, Name, Plan, RequestId, Tree,
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
pub(crate) mod creation;
pub(crate) mod deletion;
mod oauth;
pub(crate) mod resource_cleanup;
pub(crate) mod resources;
pub use access::Access;

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Local,
    Cells,
}

#[derive(Serialize, ts_rs::TS)]
pub(crate) struct CellView {
    id: CellId,
    name: Name,
    parent: Option<CellId>,
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
            deletion::DeletionPhase::Review => Lifecycle::Blocked {
                message: "Resource cleanup has started; review deletion to finish".into(),
            },
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
    before: Tree,
    after: Tree,
    cell: Cell,
    path: PathBuf,
    plans: BTreeMap<CellId, Plan>,
}
struct Operator {
    config: Config,
    authority: String,
    relays: Mutex<BTreeMap<String, oauth::Relay>>,
    vms: tokio::sync::RwLock<BTreeMap<CellId, String>>,
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
    fn tree(&self) -> Result<(Tree, BTreeMap<CellId, PathBuf>)> {
        crate::tree::load(&self.config.tree).map_err(bad)
    }
    async fn forward(&self, name: &CellId, mut request: Request) -> Result<Response> {
        deletion::ensure_available(&self.config.state, name)?;
        if self.tree()?.0.get(name).is_none() {
            return Err(Error(StatusCode::NOT_FOUND, "Cell not found".into()));
        }
        let mut child = None;
        let stream: Box<dyn Transport> = if let Some(dir) = &self.config.sockets {
            Box::new(
                UnixStream::connect(dir.join(format!("{name}.sock")))
                    .await
                    .map_err(fail)?,
            )
        } else {
            let vm = self.vms.read().await.get(name).cloned().ok_or_else(|| {
                Error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Cell has no VM; apply its declaration first".into(),
                )
            })?;
            // exe.dev's SSH gateway restricts forwarded Unix socket paths. An
            // operator-owned relay command reaches the socket with OS group checks.
            let mut process = Command::new("ssh")
                .args([
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=10",
                    "-o",
                    "ServerAliveInterval=15",
                    "-o",
                    "ServerAliveCountMax=2",
                ])
                .arg(format!("{vm}.exe.xyz"))
                .arg("/usr/local/bin/cell relay")
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
        let response = sender.send_request(request).await.map_err(fail)?;
        let (parts, body) = response.into_parts();
        Ok(Response::from_parts(parts, Body::new(body)))
    }

    async fn refresh_vms(&self) -> Result<()> {
        if self.config.sockets.is_some() {
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
    let jobs = creation::list(&op.config.state).map_err(fail)?;
    let deletions = deletion::list(&op.config.state).map_err(fail)?;
    let vms = op.vms.read().await;
    let row = |cell: &Cell, parent: Option<CellId>, creation: Option<&creation::Creation>| {
        let deletion = deletions
            .iter()
            .find(|j| j.cell().id == cell.id && j.active());
        let vm = vms.get(&cell.id).cloned();
        let socket_available = op
            .config
            .sockets
            .as_ref()
            .is_some_and(|dir| dir.join(format!("{}.sock", cell.id)).exists());
        let lifecycle = lifecycle(
            creation.map(|j| &j.phase),
            deletion.map(|j| &j.phase),
            vm.is_some() || socket_available,
        );
        CellView {
            id: cell.id.clone(),
            name: cell.name.clone(),
            parent,
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
                tree.parent(&cell.id).map(|c| c.id.clone()),
                jobs.iter().find(|j| j.cell.id == cell.id && j.is_current()),
            )
        })
        .collect();
    for job in jobs.iter().filter(|j| {
        matches!(
            j.phase,
            creation::CreationPhase::Provisioning { .. } | creation::CreationPhase::Failed { .. }
        ) && tree.get(&j.cell.id).is_none()
    }) {
        cells.push(row(&job.cell, job.parent.clone(), Some(job)));
    }
    for job in deletions.iter().filter(|j| j.active()) {
        if !cells.iter().any(|c| c.id == job.cell().id) {
            cells.push(row(job.cell(), job.parent().clone(), None));
        }
    }
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
    }))
}

async fn proxy(
    State(op): State<Arc<Operator>>,
    Path((name, tail)): Path<(CellId, String)>,
    mut request: Request,
) -> Result<Response> {
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
struct Rename {
    name: Name,
}

/// Labels are operator metadata, not runtime configuration. Never contact or
/// restart the VM, change a pin, or rewrite a signed statement for a rename.
async fn rename(
    State(op): State<Arc<Operator>>,
    Path(id): Path<CellId>,
    Json(input): Json<Rename>,
) -> Result<StatusCode> {
    let _lock = op.applying.try_lock().map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "Another cell change is in progress".into(),
        )
    })?;
    deletion::ensure_available(&op.config.state, &id)?;
    let jobs = creation::list(&op.config.state).map_err(fail)?;
    if jobs.iter().any(|j| {
        j.cell.id == id
            && matches!(
                j.phase,
                creation::CreationPhase::Provisioning { .. }
                    | creation::CreationPhase::Failed { .. }
            )
    }) {
        return Err(Error(
            StatusCode::CONFLICT,
            "Finish cell setup before renaming".into(),
        ));
    }
    let (tree, paths) = op.tree()?;
    let mut cell = tree
        .get(&id)
        .ok_or_else(|| Error(StatusCode::NOT_FOUND, "Cell not found".into()))?
        .clone();
    if tree.iter().any(|c| c.id != id && c.name == input.name)
        || jobs.iter().any(|j| {
            j.cell.id != id
                && j.is_current()
                && tree.get(&j.cell.id).is_none()
                && j.cell.name == input.name
        })
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "A cell with this name already exists".into(),
        ));
    }
    cell.name = input.name;
    let path = &paths[&id];
    let temp = path.with_file_name(format!(".{}.{}.tmp", id, RequestId::generate()));
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let permissions = fs::metadata(path).map_err(fail)?.permissions();
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(permissions)?;
        file.write_all(&cell.canonical())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(fail)?;
    Ok(StatusCode::NO_CONTENT)
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
    plans: BTreeMap<CellId, Plan>,
}

async fn preview(
    State(op): State<Arc<Operator>>,
    Path(name): Path<CellId>,
    Json(edit): Json<Edit>,
) -> Result<Json<Preview>> {
    deletion::ensure_available(&op.config.state, &name)?;
    let (before, paths) = op.tree()?;
    let path = paths
        .get(&name)
        .ok_or_else(|| bad("Cell not found"))?
        .clone();
    let mut cell = before
        .get(&name)
        .ok_or_else(|| bad("Cell not found"))?
        .clone();
    cell.grants = edit.grants;
    let after = Tree::new(before.iter().map(|c| {
        (
            if c.id == name {
                cell.clone()
            } else {
                c.clone()
            },
            before.parent(&c.id).map(|p| p.id.clone()),
        )
    }))
    .map_err(bad)?;
    op.config
        .binary
        .as_ref()
        .ok_or_else(|| bad("Start cell ui with --bin LINUX_CELL to apply configuration"))?;
    let pins = op.config.tree.join(".pins");
    let edge = op.edge.clone();
    let target = after.clone();
    let names = vec![name];
    let plans = tokio::task::spawn_blocking(move || {
        apply::plans(edge.as_ref(), &pins, &target, &names).map_err(|e| e.to_string())
    })
    .await
    .map_err(fail)?
    .map_err(fail)?;
    let id = RequestId::generate();
    let result = Preview {
        id,
        approval: Approved::hash(&plans),
        plans: plans.clone(),
    };
    op.changes.lock().await.insert(
        id,
        Change {
            before,
            after,
            cell,
            path,
            plans,
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
    deletion::ensure_available(&op.config.state, &change.cell.id)?;
    if op.tree()?.0 != change.before {
        return Err(Error(
            StatusCode::CONFLICT,
            "The tree changed; review the change again".into(),
        ));
    }
    let approved = Approved::new(change.plans, &body.approval).map_err(bad)?;
    // Never restart a cell that has an active conversation.
    let request = Request::builder()
        .uri("/api/pi/threads?includeArchived=true")
        .body(Body::empty())
        .unwrap();
    let response = op.forward(&change.cell.id, request).await?;
    if !response.status().is_success() {
        return Err(fail("Cannot inspect active cell sessions"));
    }
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(fail)?;
    check_idle_sessions(&bytes, &change.cell.id)?;
    if op.tree()?.0 != change.before {
        return Err(Error(
            StatusCode::CONFLICT,
            "The tree changed; review the change again".into(),
        ));
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
            &approved,
            &mut Vec::new(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(fail)?
    .map_err(fail)?;
    Ok(Json(AppliedChange { actions }))
}

fn check_idle_sessions(bytes: &[u8], cell: &CellId) -> Result<()> {
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
                let vms = op.vms.read().await.clone();
                let pins = op.config.tree.join(".pins");
                for cell in tree.iter() {
                    let Some(vm) = vms.get(&cell.id) else {
                        continue;
                    };
                    if deletion::ensure_available(&op.config.state, &cell.id).is_err() {
                        continue;
                    }
                    let context = match crate::context::Context::for_cell(&tree, &cell.id, &pins) {
                        Ok(context) => context,
                        Err(e) => {
                            eprintln!("cell {} context unavailable: {e}", cell.id);
                            continue;
                        }
                    };
                    let value = (vm.clone(), context.clone());
                    if sent
                        .get(&cell.id)
                        .is_some_and(|(old, at): &(_, std::time::Instant)| {
                            old == &value && at.elapsed().as_secs() < 60
                        })
                    {
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
                            sent.insert(cell.id.clone(), (value, std::time::Instant::now()));
                        }
                        failure => eprintln!(
                            "cell {} context publication failed; will retry: {:?}",
                            cell.id, failure
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
        .route("/api/resources", get(resources::list))
        .route("/api/resources/plan", post(resources::plan))
        .route("/api/resources/actions/:id", get(resources::get))
        .route("/api/resources/actions/:id/apply", post(resources::apply))
        .route("/api/cells/plan", post(creation::preview))
        .route("/api/creations/:id", get(creation::get))
        .route("/api/creations/:id/apply", post(creation::start))
        .route("/api/cells/:name/plan", post(preview))
        .route("/api/cells/:name/rename", post(rename))
        .route(
            "/api/cells/:name/delete-plan",
            post(deletion::preview_request),
        )
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
                        cell: "b48244b7-ed96-57fa-a3a2-e7629b8c921e".parse().unwrap(),
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
                        cell: "b48244b7-ed96-57fa-a3a2-e7629b8c921e".parse().unwrap(),
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
        let cell: CellId = "b48244b7-ed96-57fa-a3a2-e7629b8c921e".parse().unwrap();
        let session = json!({"id":crate::SessionId::generate(),"cell":"b48244b7-ed96-57fa-a3a2-e7629b8c921e","status":"idle","title":"Ignored extra metadata"});
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
        other["cell"] = json!("70ee78ad-5e3a-510a-af20-6f485ecb6f16");
        assert_eq!(
            check(json!([other])).unwrap_err().0,
            StatusCode::BAD_GATEWAY
        );
    }
}

#[cfg(test)]
#[path = "operator/identity_tests.rs"]
mod identity_tests;
