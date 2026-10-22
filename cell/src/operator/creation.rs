//! A creation is a durable, approved provisioning operation. The browser can
//! leave at any point; retry reconciles the same cell instead of creating another.
use super::*;
use crate::{plan::Action, Grant, SessionRef};
use std::{collections::BTreeSet, io::Write, os::unix::fs::OpenOptionsExt, path::Path as FsPath};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewCell {
    name: Name,
    parent: Option<Name>,
    grants: BTreeSet<Grant>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CreationPhase {
    Review,
    Provisioning {
        step: CreationStep,
    },
    Failed {
        message: String,
        #[serde(default)]
        step: Option<CreationStep>,
        #[serde(default)]
        recovery: RecoveryAction,
    },
    Ready {
        session: SessionRef,
    },
    Deleted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CreationStep {
    Preparing,
    CreatingVm,
    ConfiguringAccess,
    InstallingRuntime,
    ConnectingParent,
    OpeningChat,
}

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub(crate) struct Creation {
    pub id: RequestId,
    pub cell: Cell,
    pub parent: Option<Name>,
    before: Hash,
    pub approval: Hash,
    plans: BTreeMap<Name, Plan>,
    pub phase: CreationPhase,
}
impl Creation {
    pub fn is_review(&self) -> bool {
        matches!(self.phase, CreationPhase::Review)
    }
    pub fn is_current(&self) -> bool {
        !matches!(self.phase, CreationPhase::Review | CreationPhase::Deleted)
            && !matches!(
                self.phase,
                CreationPhase::Failed {
                    recovery: RecoveryAction::Review,
                    ..
                }
            )
    }
    fn save(&self, state: &FsPath) -> std::io::Result<()> {
        let dir = state.join("creations");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.id));
        let temp = dir.join(format!("{}.{}.tmp", self.id, RequestId::generate()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        fs::File::open(dir)?.sync_all()
    }
    fn load(state: &FsPath, id: RequestId) -> Result<Self> {
        let bytes = fs::read(state.join("creations").join(format!("{id}.json"))).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error(StatusCode::NOT_FOUND, "Cell setup not found".into())
            } else {
                fail(e)
            }
        })?;
        let job: Self = serde_json::from_slice(&bytes).map_err(fail)?;
        if job.id != id {
            return Err(fail("Cell setup identity mismatch"));
        }
        Ok(job)
    }
}

pub(super) fn list(state: &FsPath) -> std::io::Result<Vec<Creation>> {
    let dir = state.join("creations");
    fs::create_dir_all(&dir)?;
    let mut jobs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|x| x == "json") {
            jobs.push(serde_json::from_slice(&fs::read(entry.path())?)?);
        }
    }
    Ok(jobs)
}

pub(super) fn recover(state: &FsPath) -> std::io::Result<()> {
    for mut job in list(state)? {
        if matches!(job.phase, CreationPhase::Provisioning { .. }) {
            job.phase = CreationPhase::Failed {
                message: "Setup was interrupted. Retry to continue.".into(),
                step: match job.phase {
                    CreationPhase::Provisioning { ref step } => Some(step.clone()),
                    _ => None,
                },
                recovery: RecoveryAction::Retry,
            };
            job.save(state)?;
        }
    }
    Ok(())
}

pub(super) fn retire(state: &FsPath, name: &Name) -> std::io::Result<()> {
    for mut job in list(state)?.into_iter().filter(|j| &j.cell.name == name) {
        job.phase = CreationPhase::Deleted;
        job.save(state)?;
    }
    Ok(())
}

fn fingerprint(tree: &Tree) -> Hash {
    Hash::of(&crate::canon::bytes(
        &tree
            .iter()
            .map(|c| (c, tree.parent(&c.name).map(|p| &p.name)))
            .collect::<Vec<_>>(),
    ))
}

fn add(tree: &Tree, cell: &Cell, parent: &Option<Name>) -> Result<Tree> {
    Tree::new(
        tree.iter()
            .map(|c| (c.clone(), tree.parent(&c.name).map(|p| p.name.clone())))
            .chain(std::iter::once((cell.clone(), parent.clone()))),
    )
    .map_err(bad)
}

fn destination(
    root: &FsPath,
    paths: &BTreeMap<Name, PathBuf>,
    parent: &Option<Name>,
    name: &Name,
) -> Result<PathBuf> {
    let dir = match parent {
        None => root.to_owned(),
        Some(parent) => paths
            .get(parent)
            .ok_or_else(|| bad("Parent cell not found"))?
            .parent()
            .ok_or_else(|| bad("Parent declaration has no directory"))?
            .join(parent.as_str()),
    };
    Ok(dir.join(format!("{name}.json")))
}

pub(super) async fn preview(
    State(op): State<Arc<Operator>>,
    Json(input): Json<NewCell>,
) -> Result<Json<Creation>> {
    let _lock = op.applying.try_lock().map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "Another cell change is in progress".into(),
        )
    })?;
    if op.config.binary.is_none() || op.config.sockets.is_some() {
        return Err(bad(
            "Cell creation requires a deployment binary and an exe.dev connection",
        ));
    }
    let (before, paths) = op.tree()?;
    let cell = Cell::new(input.name, input.grants);
    super::deletion::ensure_available(&op.config.state, &cell.name)?;
    if let Some(parent) = &input.parent {
        super::deletion::ensure_available(&op.config.state, parent)?;
    }
    if before.get(&cell.name).is_some()
        || list(&op.config.state)
            .map_err(fail)?
            .iter()
            .any(|j| j.cell.name == cell.name && j.is_current())
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "A cell with this name already exists".into(),
        ));
    }
    let after = add(&before, &cell, &input.parent)?;
    destination(&op.config.tree, &paths, &input.parent, &cell.name)?;
    let edge = op.edge.clone();
    let pins = op.config.tree.join(".pins");
    let mut names = vec![cell.name.clone()];
    names.extend(input.parent.iter().cloned());
    let name = cell.name.clone();
    let plans = tokio::task::spawn_blocking(move || -> std::result::Result<_, String> {
        let (vms, _) = edge.listing().map_err(|e| e.to_string())?;
        if vms.iter().any(|v| v.vm_name == name.as_str() || v.cell_name().as_ref() == Some(&name)) {
            return Err("A VM with this cell's name already exists; choose another name".into());
        }
        let plans = apply::plans(edge.as_ref(), &pins, &after, &names).map_err(|e| e.to_string())?;
        // Creation may connect the parent to its new child. It does not silently
        // reconcile unrelated parent drift or restart an existing workspace.
        for (target, plan) in &plans {
            if *target != name && plan.actions.iter().any(|a| *a != Action::Open(name.clone())) {
                return Err(format!("{target} has pending configuration changes. Apply those before adding a subcell."));
            }
        }
        Ok(plans)
    }).await.map_err(fail)?.map_err(bad)?;
    let job = Creation {
        id: RequestId::generate(),
        cell,
        parent: input.parent,
        before: fingerprint(&before),
        approval: Approved::hash(&plans),
        plans,
        phase: CreationPhase::Review,
    };
    job.save(&op.config.state).map_err(fail)?;
    Ok(Json(job))
}

pub(super) async fn get(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
) -> Result<Json<Creation>> {
    Ok(Json(Creation::load(&op.config.state, id)?))
}

/// Insert a declaration without overwriting any existing file. Retrying may
/// reuse only the identical declaration in the identical tree position.
fn declare(op: &Operator, job: &Creation) -> Result<Tree> {
    let (tree, paths) = op.tree()?;
    if let Some(existing) = tree.get(&job.cell.name) {
        if existing != &job.cell
            || tree.parent(&job.cell.name).map(|p| &p.name) != job.parent.as_ref()
        {
            return Err(Error(
                StatusCode::CONFLICT,
                "This cell's declaration changed; setup cannot overwrite it".into(),
            ));
        }
        return Ok(tree);
    }
    let after = add(&tree, &job.cell, &job.parent)?;
    let path = destination(&op.config.tree, &paths, &job.parent, &job.cell.name)?;
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).map_err(fail)?;
    // A complete temporary file is linked into place atomically, without the
    // overwrite behavior of rename. Tree readers never see a partial JSON file.
    let temp = dir.join(format!(".{}.tmp", job.id));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .map_err(fail)?;
    file.write_all(&job.cell.canonical()).map_err(fail)?;
    file.sync_all().map_err(fail)?;
    fs::hard_link(&temp, &path).map_err(fail)?;
    fs::remove_file(temp).map_err(fail)?;
    fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(fail)?;
    fs::File::open(&op.config.tree)
        .and_then(|f| f.sync_all())
        .map_err(fail)?;
    Ok(after)
}

pub(super) async fn start(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
    Json(body): Json<Apply>,
) -> Result<Json<Creation>> {
    let job = Creation::load(&op.config.state, id)?;
    if matches!(job.phase, CreationPhase::Deleted) {
        return Err(Error(StatusCode::GONE, "This cell was deleted".into()));
    }
    super::deletion::ensure_available(&op.config.state, &job.cell.name)?;
    if let Some(parent) = &job.parent {
        super::deletion::ensure_available(&op.config.state, parent)?;
    }
    if body.approval != job.approval {
        return Err(bad("The creation plan changed; review it again"));
    }
    if matches!(
        job.phase,
        CreationPhase::Ready { .. } | CreationPhase::Provisioning { .. }
    ) {
        return Ok(Json(job));
    }
    let lock = op.applying.clone().try_lock_owned().map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "Another cell change is in progress".into(),
        )
    })?;
    let mut job = Creation::load(&op.config.state, id)?;
    let (tree, _) = op.tree()?;
    if job.is_review() && fingerprint(&tree) != job.before {
        job.phase = CreationPhase::Failed {
            message: "The tree changed; review the new cell again".into(),
            step: None,
            recovery: RecoveryAction::Review,
        };
        job.save(&op.config.state).map_err(fail)?;
        return Ok(Json(job));
    }
    let approved = Approved::new(job.plans.clone(), &body.approval).map_err(bad)?;
    job.phase = CreationPhase::Provisioning {
        step: CreationStep::Preparing,
    };
    job.save(&op.config.state).map_err(fail)?;
    let initial = job.clone();
    tokio::spawn(async move {
        let result = provision(&op, &mut job, approved).await;
        if let Err(error) = result {
            let step = match &job.phase {
                CreationPhase::Provisioning { step } => Some(step.clone()),
                _ => None,
            };
            job.phase = CreationPhase::Failed {
                message: error.1,
                step,
                recovery: RecoveryAction::Retry,
            };
            if let Err(e) = job.save(&op.config.state) {
                eprintln!("Could not save cell setup failure: {e}");
            }
        }
        drop(lock);
    });
    Ok(Json(initial))
}

async fn provision(op: &Arc<Operator>, job: &mut Creation, approved: Approved) -> Result<()> {
    let tree = declare(op, job)?;
    let edge = op.edge.clone();
    let state = op.config.state.clone();
    let pins = op.config.tree.join(".pins");
    let mut progress = job.clone();
    tokio::task::spawn_blocking(move || {
        apply::apply_with_progress(
            edge.as_ref(),
            &pins,
            &tree,
            &approved,
            &mut std::io::sink(),
            &mut |name, action| {
                let step = if *name != progress.cell.name {
                    CreationStep::ConnectingParent
                } else {
                    match action {
                        Action::Create => CreationStep::CreatingVm,
                        Action::Setup => CreationStep::InstallingRuntime,
                        _ => CreationStep::ConfiguringAccess,
                    }
                };
                progress.phase = CreationPhase::Provisioning { step };
                progress.save(&state)?;
                Ok(())
            },
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(fail)?
    .map_err(fail)?;
    op.refresh_vms().await?;
    job.phase = CreationPhase::Provisioning {
        step: CreationStep::OpeningChat,
    };
    job.save(&op.config.state).map_err(fail)?;
    // If the gateway stopped just after creating the first chat, discover that
    // chat on retry rather than creating a second one.
    let threads: Vec<crate::chat::types::SessionSummary> =
        cell_json(op, &job.cell.name, "GET", "/api/pi/threads", None).await?;
    if threads.iter().any(|s| s.cell != job.cell.name) {
        return Err(fail("Cell returned chats belonging to another cell"));
    }
    let id = match threads.first() {
        Some(session) => session.id,
        None => {
            let created: crate::chat::types::CreatedThread = cell_json(
                op,
                &job.cell.name,
                "POST",
                "/api/pi/threads",
                Some(json!({"title":"New chat"})),
            )
            .await?;
            if created.metadata.cell != job.cell.name {
                return Err(fail("Cell returned a chat belonging to another cell"));
            }
            created.metadata.id
        }
    };
    job.phase = CreationPhase::Ready {
        session: SessionRef {
            cell: job.cell.name.clone(),
            id,
        },
    };
    job.save(&op.config.state).map_err(fail)?;
    Ok(())
}

async fn cell_json<T: serde::de::DeserializeOwned>(
    op: &Operator,
    name: &Name,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<T> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        op.forward(name, request),
    )
    .await
    .map_err(|_| fail("The cell is taking too long to respond. Retry setup."))??;
    let status = response.status();
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        axum::body::to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .map_err(|_| fail("The cell response timed out. Retry setup."))?
    .map_err(fail)?;
    if !status.is_success() {
        return Err(fail(
            serde_json::from_slice::<ErrorBody>(&bytes)
                .map(|body| body.error)
                .unwrap_or_else(|_| "Could not open the cell's first chat".into()),
        ));
    }
    serde_json::from_slice(&bytes).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_failed_record_defaults_to_safe_same_approval_retry() {
        let phase: CreationPhase = serde_json::from_value(serde_json::json!({
            "state": "failed",
            "message": "legacy failure"
        }))
        .unwrap();
        assert_eq!(
            phase,
            CreationPhase::Failed {
                message: "legacy failure".into(),
                step: None,
                recovery: RecoveryAction::Retry,
            }
        );
    }
    use crate::SessionId;
    use crate::{
        exe::{Integration, Vm},
        said::{Key, Said, Secret, Statement},
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex as StdMutex,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct FakeEdge {
        vms: StdMutex<Vec<Vm>>,
        integrations: StdMutex<Vec<Integration>>,
        logs: StdMutex<BTreeMap<String, (Secret, Vec<Statement>)>>,
        creates: AtomicUsize,
        setups: AtomicUsize,
        fail_setup: AtomicBool,
    }
    impl Edge for FakeEdge {
        fn listing(&self) -> apply::Fallible<(Vec<Vm>, Vec<Integration>)> {
            Ok((
                self.vms.lock().unwrap().clone(),
                self.integrations.lock().unwrap().clone(),
            ))
        }
        fn binary(&self) -> apply::Fallible<Option<Hash>> {
            Ok(Some(Hash::of(b"fixture")))
        }
        fn run(&self, args: &[&str]) -> apply::Fallible<String> {
            let arg = |prefix: &str| {
                args.iter()
                    .find_map(|a| a.strip_prefix(prefix))
                    .unwrap()
                    .to_string()
            };
            match args {
                ["new", ..] => {
                    let name = arg("--name=");
                    let mut vms = self.vms.lock().unwrap();
                    assert!(
                        !vms.iter().any(|v| v.vm_name == name),
                        "retry must not recreate a VM"
                    );
                    vms.push(Vm {
                        created_at: Some("fixture-incarnation".into()),
                        vm_name: name,
                        tags: vec![arg("--tag=")],
                        status: "running".into(),
                    });
                    self.creates.fetch_add(1, Ordering::SeqCst);
                }
                ["integrations", "add", "http-proxy", ..] => {
                    self.integrations.lock().unwrap().push(Integration {
                        name: arg("--name="),
                        kind: "http-proxy".into(),
                        attachments: vec![arg("--attach=")],
                        config: json!({"target":arg("--target="),"peer":true}),
                    });
                }
                _ => return Err(format!("unexpected edge action: {args:?}").into()),
            }
            Ok(String::new())
        }
        fn log(&self, vm: &str) -> apply::Fallible<Option<(Key, Vec<Statement>)>> {
            Ok(self
                .logs
                .lock()
                .unwrap()
                .get(vm)
                .map(|(secret, log)| (secret.public(), log.clone())))
        }
        fn setup(&self, vm: &str, cell: &Cell) -> apply::Fallible<String> {
            if self.fail_setup.swap(false, Ordering::SeqCst) {
                return Err("temporary setup failure".into());
            }
            self.setups.fetch_add(1, Ordering::SeqCst);
            let mut logs = self.logs.lock().unwrap();
            let (secret, log) = logs
                .entry(vm.into())
                .or_insert_with(|| (Secret::generate(), Vec::new()));
            log.push(Statement::sign(
                secret,
                cell.name.clone(),
                log.len() as u64 + 1,
                Said::Checked {
                    cell: cell.hash(),
                    ok: true,
                    report: String::new(),
                    binary: Hash::of(b"fixture"),
                    signed: BTreeSet::new(),
                },
            ));
            Ok("WALL OK".into())
        }
        fn knock(&self, _: &str, _: &Said) -> apply::Fallible<u64> {
            unreachable!()
        }
    }
    struct Fixture {
        op: Arc<Operator>,
        edge: Arc<FakeEdge>,
        dir: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = PathBuf::from("/tmp").join(format!("cell-create-{}", RequestId::generate()));
            fs::create_dir_all(dir.join("tree")).unwrap();
            fs::create_dir_all(dir.join("sockets")).unwrap();
            let parent: Cell = serde_json::from_value(
                json!({"name":"project","grants":[{"provider":"anthropic"}]}),
            )
            .unwrap();
            fs::write(dir.join("tree/project.json"), parent.canonical()).unwrap();
            let edge = Arc::new(FakeEdge::default());
            edge.vms.lock().unwrap().push(Vm {
                created_at: Some("fixture-incarnation".into()),
                vm_name: "project".into(),
                tags: vec!["cell-project".into()],
                status: "running".into(),
            });
            edge.setup("project", &parent).unwrap();
            let op = Arc::new(Operator {
                config: Config {
                    tree: dir.join("tree"),
                    state: dir.join("state"),
                    assets: dir.clone(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    binary: Some(dir.join("binary")),
                    sockets: None,
                },
                authority: "fixture".into(),
                relays: Mutex::new(BTreeMap::new()),
                vms: tokio::sync::RwLock::new(BTreeMap::new()),
                edge: edge.clone(),
                changes: Mutex::new(BTreeMap::new()),
                applying: Arc::new(Mutex::new(())),
                stopping: tokio::sync::watch::channel(false).0,
            });
            Self { op, edge, dir }
        }
        async fn preview(
            &self,
            name: &str,
            parent: Option<&str>,
            grants: Value,
        ) -> Result<Creation> {
            preview(
                State(self.op.clone()),
                Json(
                    serde_json::from_value(json!({"name":name,"parent":parent,"grants":grants}))
                        .unwrap(),
                ),
            )
            .await
            .map(|j| j.0)
        }
        async fn settled(&self, id: RequestId) -> Creation {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    let job = Creation::load(&self.op.config.state, id).unwrap();
                    if !matches!(job.phase, CreationPhase::Provisioning { .. }) {
                        return job;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn creation_validates_tree_names_ownership_and_parent_drift_before_mutating() {
        let fixture = Fixture::new();
        assert!(serde_json::from_value::<NewCell>(
            json!({"name":"../outside","parent":null,"grants":[]})
        )
        .is_err());
        assert!(fixture
            .preview("project-dev", Some("missing"), json!([]))
            .await
            .is_err());
        assert!(fixture
            .preview(
                "project-dev",
                Some("project"),
                json!([{"provider":"openai"}])
            )
            .await
            .unwrap_err()
            .1
            .contains("not covered"));
        assert_eq!(
            fixture
                .preview("project", None, json!([]))
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        fixture.edge.vms.lock().unwrap().push(Vm {
            created_at: Some("fixture-incarnation".into()),
            vm_name: "taken".into(),
            tags: vec![],
            status: "running".into(),
        });
        assert!(fixture
            .preview("taken", None, json!([]))
            .await
            .unwrap_err()
            .1
            .contains("already exists"));
        let job = fixture
            .preview("project-dev", Some("project"), json!([]))
            .await
            .unwrap();
        assert_eq!(
            job.plans[&"project".parse().unwrap()].actions,
            vec![Action::Open("project-dev".parse().unwrap())]
        );
        assert!(
            !fixture.dir.join("tree/project/project-dev.json").exists(),
            "preview must not declare or provision"
        );
        assert!(start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: Hash::ZERO
            })
        )
        .await
        .is_err());
        fs::write(
            fixture.dir.join("tree/another.json"),
            br#"{"name":"another","grants":[]}"#,
        )
        .unwrap();
        let stale = start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: job.approval,
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            &stale.0.phase,
            CreationPhase::Failed {
                recovery: RecoveryAction::Review,
                ..
            }
        ));
        assert!(!fixture.dir.join("tree/project/project-dev.json").exists());
        let reviewed = fixture
            .preview("project-dev", Some("project"), json!([]))
            .await
            .unwrap();
        assert_ne!(
            reviewed.id, job.id,
            "review creates a fresh operation and approval"
        );
        fixture.edge.logs.lock().unwrap().clear();
        assert!(fixture
            .preview("project-other", Some("project"), json!([]))
            .await
            .unwrap_err()
            .1
            .contains("pending configuration"));
        assert_eq!(fixture.edge.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn interrupted_setup_retries_the_same_vm_and_opens_one_durable_chat() {
        let mut fixture = Fixture::new();
        let job = fixture
            .preview(
                "project-dev",
                Some("project"),
                json!([{"provider":"anthropic"}]),
            )
            .await
            .unwrap();
        // Production uses SSH; keep the exact HTTP transport with a local socket
        // while the reconciler still uses signed box observations and real plans.
        Arc::get_mut(&mut fixture.op).unwrap().config.sockets = Some(fixture.dir.join("sockets"));
        fixture.edge.fail_setup.store(true, Ordering::SeqCst);
        let _ = start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: job.approval,
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            fixture.settled(job.id).await.phase,
            CreationPhase::Failed { .. }
        ));
        assert_eq!(fixture.edge.creates.load(Ordering::SeqCst), 1);
        assert!(fixture.dir.join("tree/project/project-dev.json").exists());

        let listener =
            tokio::net::UnixListener::bind(fixture.dir.join("sockets/project-dev.sock")).unwrap();
        let chats = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let creates = Arc::new(AtomicUsize::new(0));
        let stored = chats.clone();
        let count = creates.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8(request).unwrap();
                let body = if text.starts_with("POST ") {
                    count.fetch_add(1, Ordering::SeqCst);
                    let chat =
                        json!({"id":SessionId::generate(),"cell":"project-dev","title":"New chat","status":"idle"});
                    stored.lock().unwrap().push(chat.clone());
                    json!({"metadata":chat})
                } else {
                    json!(*stored.lock().unwrap())
                }
                .to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let _ = start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: job.approval,
            }),
        )
        .await
        .unwrap();
        let ready = fixture.settled(job.id).await;
        assert!(
            matches!(ready.phase, CreationPhase::Ready { .. }),
            "{:?}",
            ready.phase
        );
        assert_eq!(fixture.edge.creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.edge.setups.load(Ordering::SeqCst),
            2,
            "the parent must not restart"
        );
        assert_eq!(
            fixture.edge.integrations.lock().unwrap().len(),
            1,
            "the parent gets its child door"
        );
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        let parent_bytes = fs::read(fixture.dir.join("tree/project.json")).unwrap();
        // Simulate shutdown after the chat was created, before Ready was saved.
        let mut interrupted = ready.clone();
        interrupted.phase = CreationPhase::Provisioning {
            step: CreationStep::OpeningChat,
        };
        interrupted.save(&fixture.op.config.state).unwrap();
        recover(&fixture.op.config.state).unwrap();
        assert!(matches!(
            Creation::load(&fixture.op.config.state, job.id)
                .unwrap()
                .phase,
            CreationPhase::Failed { .. }
        ));
        let valid_chats = chats.lock().unwrap().clone();
        // A malformed list is not an empty list. Retrying must not create a
        // duplicate chat when a runtime omits a required field.
        chats.lock().unwrap()[0]
            .as_object_mut()
            .unwrap()
            .remove("id");
        let _ = start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: job.approval,
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            fixture.settled(job.id).await.phase,
            CreationPhase::Failed { .. }
        ));
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        *chats.lock().unwrap() = valid_chats;
        let _ = start(
            State(fixture.op.clone()),
            Path(job.id),
            Json(Apply {
                approval: job.approval,
            }),
        )
        .await
        .unwrap();
        assert_eq!(fixture.settled(job.id).await.phase, ready.phase);
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read(fixture.dir.join("tree/project.json")).unwrap(),
            parent_bytes
        );
        // A completed request is safely replayable.
        assert_eq!(
            start(
                State(fixture.op.clone()),
                Path(job.id),
                Json(Apply {
                    approval: job.approval
                })
            )
            .await
            .unwrap()
            .0
            .phase,
            ready.phase
        );
        server.abort();
    }

    #[test]
    fn declaration_retry_never_overwrites_a_changed_cell_or_moves_it() {
        let fixture = Fixture::new();
        let cell: Cell = serde_json::from_value(json!({"name":"project-dev","grants":[]})).unwrap();
        let job = Creation {
            id: RequestId::generate(),
            cell,
            parent: Some("project".parse().unwrap()),
            before: Hash::ZERO,
            approval: Hash::ZERO,
            plans: BTreeMap::new(),
            phase: CreationPhase::Review,
        };
        declare(&fixture.op, &job).unwrap();
        declare(&fixture.op, &job).unwrap();
        fs::write(
            fixture.dir.join("tree/project/project-dev.json"),
            br#"{"name":"project-dev","grants":[{"provider":"anthropic"}]}"#,
        )
        .unwrap();
        assert_eq!(
            declare(&fixture.op, &job).unwrap_err().0,
            StatusCode::CONFLICT
        );
        fs::remove_file(fixture.dir.join("tree/project/project-dev.json")).unwrap();
        fs::write(
            fixture.dir.join("tree/project-dev.json"),
            job.cell.canonical(),
        )
        .unwrap();
        assert_eq!(
            declare(&fixture.op, &job).unwrap_err().0,
            StatusCode::CONFLICT
        );
    }
}
