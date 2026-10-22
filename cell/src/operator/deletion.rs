//! Explicit, durable deletion. Provisioning's apply loop remains non-destructive.
//! Approval binds a leaf, its VM incarnation, and the exact owned attachments.
use super::*;
use crate::exe::{Integration, Vm};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path as FsPath};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct VmIdentity {
    name: crate::exe::VmName,
    created_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Attachment {
    integration: String,
    spec: String,
    configuration: Hash,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Door {
    name: String,
    configuration: Hash,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Target {
    cell: Cell,
    parent: Option<Name>,
    vm: Option<VmIdentity>,
    attachments: Vec<Attachment>,
    door: Option<Door>,
    pin: Option<Hash>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum DeletionPhase {
    Review,
    Deleting {
        step: DeletionStep,
    },
    Failed {
        message: String,
        #[serde(default)]
        step: Option<DeletionStep>,
        #[serde(default)]
        recovery: RecoveryAction,
    },
    Deleted,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeletionStep {
    Disconnecting,
    DeletingVm,
    RemovingDeclaration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Deletion {
    pub id: RequestId,
    target: Target,
    pub approval: Hash,
    pub phase: DeletionPhase,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub(crate) struct DeletionView {
    pub id: RequestId,
    cell: Cell,
    parent: Option<Name>,
    vm: Option<String>,
    approval: Hash,
    phase: DeletionPhase,
}
impl Deletion {
    pub fn cell(&self) -> &Cell {
        &self.target.cell
    }
    pub fn parent(&self) -> &Option<Name> {
        &self.target.parent
    }
    pub fn active(&self) -> bool {
        matches!(self.phase, DeletionPhase::Deleting { .. })
            || matches!(
                self.phase,
                DeletionPhase::Failed {
                    recovery: RecoveryAction::Retry,
                    ..
                }
            )
    }
    pub fn view(&self) -> DeletionView {
        DeletionView {
            id: self.id,
            cell: self.target.cell.clone(),
            parent: self.target.parent.clone(),
            vm: self.target.vm.as_ref().map(|v| v.name.to_string()),
            approval: self.approval,
            phase: self.phase.clone(),
        }
    }
    fn save(&self, state: &FsPath) -> std::io::Result<()> {
        let dir = state.join("deletions");
        fs::create_dir_all(&dir)?;
        let temp = dir.join(format!(".{}.{}.tmp", self.id, RequestId::generate()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        fs::rename(temp, dir.join(format!("{}.json", self.id)))?;
        fs::File::open(dir)?.sync_all()
    }
    fn load(state: &FsPath, id: RequestId) -> Result<Self> {
        let path = state.join("deletions").join(format!("{id}.json"));
        let bytes = fs::read(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error(StatusCode::NOT_FOUND, "Deletion not found".into())
            } else {
                fail(e)
            }
        })?;
        let job: Self = serde_json::from_slice(&bytes).map_err(fail)?;
        if job.id != id || ApprovedHash::of(&job.target) != job.approval {
            return Err(fail("Deletion record does not match its approval"));
        }
        Ok(job)
    }
}
// Separate from an Approved provisioning plan: apply can never delete a VM.
struct ApprovedHash;
impl ApprovedHash {
    fn of(target: &Target) -> Hash {
        Hash::of(&crate::canon::bytes(target))
    }
}

pub(super) fn list(state: &FsPath) -> std::io::Result<Vec<Deletion>> {
    let dir = state.join("deletions");
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
        if matches!(job.phase, DeletionPhase::Deleting { .. }) {
            job.phase = DeletionPhase::Failed {
                message: "Deletion was interrupted. Retry to finish.".into(),
                step: match job.phase {
                    DeletionPhase::Deleting { ref step } => Some(step.clone()),
                    _ => None,
                },
                recovery: RecoveryAction::Retry,
            };
            job.save(state)?;
        }
    }
    Ok(())
}
pub(super) fn ensure_available(state: &FsPath, name: &Name) -> Result<()> {
    if list(state)
        .map_err(fail)?
        .iter()
        .any(|j| j.active() && j.cell().name == *name)
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "This cell is being deleted. Finish its deletion first.".into(),
        ));
    }
    Ok(())
}

fn pin(tree: &FsPath, name: &Name) -> std::io::Result<Option<Hash>> {
    match fs::read(tree.join(".pins").join(format!("{name}.pub"))) {
        Ok(bytes) => Ok(Some(Hash::of(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn config(integration: &Integration) -> Hash {
    Hash::of(&crate::canon::bytes(&(
        &integration.kind,
        &integration.config,
    )))
}

fn identity(vms: &[Vm], name: &Name) -> Result<Option<VmIdentity>> {
    let Some(vm) = crate::exe::cell_vm(vms, name).map_err(bad)? else {
        return Ok(None);
    };
    let created_at = vm
        .created_at
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("Cannot verify the VM's creation identity"))?
        .clone();
    Ok(Some(VmIdentity {
        name: crate::exe::VmName::parse(vm.vm_name.clone()).map_err(bad)?,
        created_at,
    }))
}

fn owned(
    vms: &[Vm],
    integrations: &[Integration],
    cell: &Name,
    parent: &Option<Name>,
    vm: &Option<VmIdentity>,
) -> Result<(Vec<Attachment>, Option<Door>)> {
    let own_tag = format!("cell-{cell}");
    let door_name = crate::exe::door_name(cell);
    let mut door = None;
    let mut attachments = Vec::new();
    for integration in integrations {
        let is_door = integration.name == door_name && parent.is_some();
        if is_door {
            // Only the dedicated parent door is ours to clean up. A reused name
            // targeting something else is never authority to detach/remove it.
            let expected = vm
                .as_ref()
                .map(|v| v.name.as_str())
                .unwrap_or(cell.as_str());
            if integration.kind() != crate::exe::IntegrationKind::HttpProxy
                || integration.config["peer_vm"] != expected
            {
                return Err(bad(
                    "The parent connection no longer targets this cell; review it before deleting",
                ));
            }
            door = Some(Door {
                name: integration.name.clone(),
                configuration: config(integration),
            });
        }
        let parent_vm = if is_door {
            let p = parent.as_ref().unwrap();
            identity(vms, p)?.map(|v| format!("vm:{}", v.name))
        } else {
            None
        };
        for spec in &integration.attachments {
            let target = crate::exe::AttachmentTarget::parse(spec);
            let is_own = match &target {
                crate::exe::AttachmentTarget::Tag(tag) => tag == &own_tag,
                crate::exe::AttachmentTarget::Vm(name) => {
                    vm.as_ref().is_some_and(|v| &v.name == name)
                }
                _ => false,
            };
            let is_parent_door_target = is_door
                && match &target {
                    crate::exe::AttachmentTarget::Tag(tag) => {
                        parent.as_ref().is_some_and(|p| tag == &format!("cell-{p}"))
                    }
                    crate::exe::AttachmentTarget::Vm(name) => parent_vm
                        .as_ref()
                        .is_some_and(|v| v == &format!("vm:{name}")),
                    _ => false,
                };
            if is_own || is_parent_door_target {
                attachments.push(Attachment {
                    integration: integration.name.clone(),
                    spec: spec.clone(),
                    configuration: config(integration),
                });
            }
        }
    }
    attachments.sort_by(|a, b| (&a.integration, &a.spec).cmp(&(&b.integration, &b.spec)));
    attachments.dedup();
    Ok((attachments, door))
}

/// Validate the local target on every retry, including when its declaration
/// has already been removed by a partially completed deletion.
fn check_tree(op: &Operator, target: &Target) -> Result<()> {
    let (tree, _) = op.tree()?;
    if let Some(current) = tree.get(&target.cell.name) {
        if current != &target.cell
            || tree.parent(&current.name).map(|p| &p.name) != target.parent.as_ref()
        {
            return Err(Error(
                StatusCode::CONFLICT,
                "This cell changed. Review its deletion again.".into(),
            ));
        }
        if tree.children(&current.name).next().is_some() {
            return Err(bad("Delete this cell's subcells first"));
        }
    }
    if creation::list(&op.config.state)
        .map_err(fail)?
        .iter()
        .any(|j| j.is_current() && j.parent.as_ref() == Some(&target.cell.name))
    {
        return Err(bad("Delete this cell's subcells first"));
    }
    Ok(())
}

pub(super) async fn preview(
    State(op): State<Arc<Operator>>,
    Path(name): Path<Name>,
) -> Result<Json<DeletionView>> {
    let _lock = op.applying.try_lock().map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "Another cell change is in progress".into(),
        )
    })?;
    if op.config.sockets.is_some() {
        return Err(bad("Local development workspaces cannot be deleted here"));
    }
    if let Some(job) = list(&op.config.state)
        .map_err(fail)?
        .into_iter()
        .find(|j| j.active() && j.cell().name == name)
    {
        return Ok(Json(job.view()));
    }
    let (tree, _) = op.tree()?;
    let (cell, parent) = if let Some(cell) = tree.get(&name) {
        (cell.clone(), tree.parent(&name).map(|p| p.name.clone()))
    } else {
        let job = creation::list(&op.config.state)
            .map_err(fail)?
            .into_iter()
            .find(|j| j.cell.name == name && j.is_current())
            .ok_or_else(|| Error(StatusCode::NOT_FOUND, "Cell not found".into()))?;
        (job.cell, job.parent)
    };
    let mut target = Target {
        cell,
        parent,
        vm: None,
        attachments: vec![],
        door: None,
        pin: pin(&op.config.tree, &name).map_err(fail)?,
    };
    check_tree(&op, &target)?;
    let edge = op.edge.clone();
    let (vms, integrations) =
        tokio::task::spawn_blocking(move || edge.listing().map_err(|e| e.to_string()))
            .await
            .map_err(fail)?
            .map_err(fail)?;
    target.vm = identity(&vms, &name)?;
    (target.attachments, target.door) =
        owned(&vms, &integrations, &name, &target.parent, &target.vm)?;
    let job = Deletion {
        id: RequestId::generate(),
        approval: ApprovedHash::of(&target),
        target,
        phase: DeletionPhase::Review,
    };
    job.save(&op.config.state).map_err(fail)?;
    Ok(Json(job.view()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Confirm {
    approval: Hash,
    name: Name,
}
pub(super) async fn get(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
) -> Result<Json<DeletionView>> {
    Ok(Json(Deletion::load(&op.config.state, id)?.view()))
}
pub(super) async fn start(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
    Json(body): Json<Confirm>,
) -> Result<Json<DeletionView>> {
    let mut job = Deletion::load(&op.config.state, id)?;
    if body.approval != job.approval || body.name != job.target.cell.name {
        return Err(bad("Confirm the cell name and its deletion plan"));
    }
    if matches!(
        job.phase,
        DeletionPhase::Deleted | DeletionPhase::Deleting { .. }
    ) {
        return Ok(Json(job.view()));
    }
    let lock = op.applying.clone().try_lock_owned().map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "Another cell change is in progress".into(),
        )
    })?;
    if list(&op.config.state)
        .map_err(fail)?
        .iter()
        .any(|j| j.active() && j.cell().name == job.cell().name && j.id != id)
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "This cell already has a deletion in progress".into(),
        ));
    }
    let preflight = async {
        check_tree(&op, &job.target)?;
        let check = op.clone();
        let target = job.target.clone();
        tokio::task::spawn_blocking(move || remaining(&check, &target))
            .await
            .map_err(fail)??;
        check_tree(&op, &job.target)?;
        Ok::<(), Error>(())
    }
    .await;
    if let Err(error) = preflight {
        job.phase = DeletionPhase::Failed {
            message: error.1,
            step: None,
            recovery: RecoveryAction::Review,
        };
        job.save(&op.config.state).map_err(fail)?;
        return Ok(Json(job.view()));
    }
    job.phase = DeletionPhase::Deleting {
        step: DeletionStep::Disconnecting,
    };
    job.save(&op.config.state).map_err(fail)?;
    let initial = job.view();
    tokio::spawn(async move {
        let worker = op.clone();
        let work = job.clone();
        let result =
            tokio::task::spawn_blocking(move || execute(&worker, ConfirmedDeletion(work))).await;
        match result {
            Ok(Ok(())) => {
                op.vms.write().await.remove(&job.cell().name);
            }
            failure => {
                let message = match failure {
                    Ok(Err(e)) => e.1,
                    Err(e) => e.to_string(),
                    _ => unreachable!(),
                };
                let step = match Deletion::load(&op.config.state, job.id)
                    .ok()
                    .map(|persisted| persisted.phase)
                {
                    Some(DeletionPhase::Deleting { step }) => Some(step),
                    Some(DeletionPhase::Failed { step, .. }) => step,
                    _ => None,
                };
                job.phase = DeletionPhase::Failed {
                    message,
                    step,
                    recovery: RecoveryAction::Retry,
                };
                if let Err(e) = job.save(&op.config.state) {
                    eprintln!("Could not save deletion failure: {e}");
                }
            }
        }
        drop(lock);
    });
    Ok(Json(initial))
}

struct Remaining {
    live: Option<VmIdentity>,
    integrations: Vec<Integration>,
    attachments: Vec<Attachment>,
    door: Option<Door>,
}

fn remaining(op: &Operator, target: &Target) -> Result<Remaining> {
    let (vms, integrations) = op.edge.listing().map_err(fail)?;
    let live = identity(&vms, &target.cell.name)?;
    if live.is_some() && live != target.vm {
        return Err(bad("The VM changed since this deletion was reviewed"));
    }
    if let Some(vm) = &target.vm {
        if vms.iter().any(|v| {
            v.vm_name == vm.name.as_str()
                && (v.created_at.as_ref() != Some(&vm.created_at)
                    || !v.tags.contains(&target.cell.tag()))
        }) {
            return Err(bad("The VM name now belongs to a different VM"));
        }
    }
    let current_pin = pin(&op.config.tree, &target.cell.name).map_err(fail)?;
    if current_pin != target.pin && !(live.is_none() && current_pin.is_none()) {
        return Err(bad("The cell's pinned identity changed"));
    }
    let (attachments, door) = owned(
        &vms,
        &integrations,
        &target.cell.name,
        &target.parent,
        &target.vm,
    )?;
    if attachments.iter().any(|a| !target.attachments.contains(a))
        || (door.is_some() && door != target.door)
    {
        return Err(bad(
            "This cell's connections changed. Review them before continuing deletion.",
        ));
    }
    Ok(Remaining {
        live,
        integrations,
        attachments,
        door,
    })
}

struct ConfirmedDeletion(Deletion);

fn execute(op: &Operator, ConfirmedDeletion(mut job): ConfirmedDeletion) -> Result<()> {
    let target = job.target.clone();
    let mut previous = None;
    loop {
        check_tree(op, &target)?;
        let Remaining {
            live,
            integrations,
            attachments,
            door,
        } = remaining(op, &target)?;
        let (step, args): (DeletionStep, Vec<String>) = if let Some(a) = attachments.first() {
            (
                DeletionStep::Disconnecting,
                vec![
                    "integrations".into(),
                    "detach".into(),
                    a.integration.clone(),
                    a.spec.clone(),
                ],
            )
        } else if let Some(d) = door.filter(|d| {
            integrations
                .iter()
                .any(|i| i.name == d.name && i.attachments.is_empty())
        }) {
            (
                DeletionStep::Disconnecting,
                vec!["integrations".into(), "remove".into(), d.name],
            )
        } else if let Some(vm) = live {
            (
                DeletionStep::DeletingVm,
                vec!["rm".into(), vm.name.to_string()],
            )
        } else {
            break;
        };
        if previous.as_ref() == Some(&args) {
            return Err(fail(
                "The deletion step has not settled yet. Retry to check it again.",
            ));
        }
        job.phase = DeletionPhase::Deleting { step };
        job.save(&op.config.state).map_err(fail)?;
        op.edge
            .run(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .map_err(fail)?;
        previous = Some(args);
    }
    job.phase = DeletionPhase::Deleting {
        step: DeletionStep::RemovingDeclaration,
    };
    job.save(&op.config.state).map_err(fail)?;
    check_tree(op, &target)?;
    // Retire old creation receipts before unlinking the declaration so they
    // cannot resurrect this cell, including after a crash or name reuse.
    creation::retire(&op.config.state, &target.cell.name).map_err(fail)?;
    let (_, paths) = op.tree()?;
    if let Some(path) = paths.get(&target.cell.name) {
        fs::remove_file(path).map_err(fail)?;
        fs::File::open(path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(fail)?;
    }
    let pin_path = op
        .config
        .tree
        .join(".pins")
        .join(format!("{}.pub", target.cell.name));
    let current_pin = pin(&op.config.tree, &target.cell.name).map_err(fail)?;
    if current_pin.is_some() && current_pin != target.pin {
        return Err(bad("The cell pin changed during deletion"));
    }
    match fs::remove_file(&pin_path) {
        Ok(()) => fs::File::open(pin_path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(fail)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(fail(e)),
    }
    job.phase = DeletionPhase::Deleted;
    job.save(&op.config.state).map_err(fail)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_failed_record_defaults_to_same_approved_retry() {
        let phase: DeletionPhase = serde_json::from_value(serde_json::json!({
            "state": "failed",
            "message": "legacy failure"
        }))
        .unwrap();
        assert_eq!(
            phase,
            DeletionPhase::Failed {
                message: "legacy failure".into(),
                step: None,
                recovery: RecoveryAction::Retry,
            }
        );
    }
    use crate::said::{Key, Said, Statement};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    };

    #[derive(Default)]
    struct Fake {
        vms: StdMutex<Vec<Vm>>,
        integrations: StdMutex<Vec<Integration>>,
        calls: StdMutex<Vec<Vec<String>>>,
        lose_delete_reply: AtomicBool,
    }
    impl Edge for Fake {
        fn listing(&self) -> apply::Fallible<(Vec<Vm>, Vec<Integration>)> {
            Ok((
                self.vms.lock().unwrap().clone(),
                self.integrations.lock().unwrap().clone(),
            ))
        }
        fn binary(&self) -> apply::Fallible<Option<Hash>> {
            Ok(None)
        }
        fn run(&self, args: &[&str]) -> apply::Fallible<String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            match args {
                ["integrations", "detach", name, spec] => self
                    .integrations
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|i| i.name == *name)
                    .unwrap()
                    .attachments
                    .retain(|s| s != spec),
                ["integrations", "remove", name] => {
                    let mut integrations = self.integrations.lock().unwrap();
                    assert!(integrations
                        .iter()
                        .find(|i| i.name == *name)
                        .unwrap()
                        .attachments
                        .is_empty());
                    integrations.retain(|i| i.name != *name);
                }
                ["rm", name] => {
                    self.vms.lock().unwrap().retain(|v| v.vm_name != *name);
                    if self.lose_delete_reply.swap(false, Ordering::SeqCst) {
                        return Err("connection lost after VM deletion".into());
                    }
                }
                _ => panic!("unexpected command: {args:?}"),
            }
            Ok(String::new())
        }
        fn log(&self, _: &str) -> apply::Fallible<Option<(Key, Vec<Statement>)>> {
            Ok(None)
        }
        fn setup(&self, _: &str, _: &Cell) -> apply::Fallible<String> {
            panic!("deletion must never run setup")
        }
        fn knock(&self, _: &str, _: &Said) -> apply::Fallible<u64> {
            unreachable!()
        }
    }
    fn vm(name: &str) -> Vm {
        Vm {
            vm_name: name.into(),
            created_at: Some(format!("created-{name}")),
            tags: vec![format!("cell-{name}")],
            status: "running".into(),
        }
    }
    struct Fixture {
        op: Arc<Operator>,
        edge: Arc<Fake>,
        dir: PathBuf,
        creation: RequestId,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("cell-delete-{}", RequestId::generate()));
            fs::create_dir_all(dir.join("tree/project")).unwrap();
            fs::create_dir_all(dir.join("tree/.pins")).unwrap();
            fs::create_dir_all(dir.join("state/creations")).unwrap();
            fs::write(
                dir.join("tree/project.json"),
                br#"{"name":"project","grants":[]}"#,
            )
            .unwrap();
            fs::write(
                dir.join("tree/project/project-dev.json"),
                br#"{"name":"project-dev","grants":[]}"#,
            )
            .unwrap();
            fs::write(dir.join("tree/.pins/project-dev.pub"), "original-pin").unwrap();
            let creation = RequestId::generate();
            fs::write(dir.join("state/creations").join(format!("{creation}.json")), json!({
                "id":creation,"cell":{"name":"project-dev","grants":[]},"parent":"project","before":Hash::ZERO,"approval":Hash::ZERO,"plans":{},
                "phase":{"state":"ready","session":{"cell":"project-dev","id":"a".repeat(32)}}
            }).to_string()).unwrap();
            let edge = Arc::new(Fake::default());
            *edge.vms.lock().unwrap() = vec![vm("project"), vm("project-dev"), vm("unrelated")];
            *edge.integrations.lock().unwrap() = vec![
                Integration {
                    name: "shared-repo".into(),
                    kind: "github".into(),
                    attachments: vec![
                        "tag:cell-project-dev".into(),
                        "vm:project-dev".into(),
                        "tag:other".into(),
                        "vm:unrelated".into(),
                    ],
                    config: json!({"repositories":["owner/repo"],"readonly":true}),
                },
                Integration {
                    name: "door-project-dev".into(),
                    kind: "http-proxy".into(),
                    attachments: vec!["tag:cell-project".into()],
                    config: json!({"peer_vm":"project-dev","target":"https://project-dev.exe.xyz/"}),
                },
            ];
            let op = Arc::new(Operator {
                config: Config {
                    tree: dir.join("tree"),
                    state: dir.join("state"),
                    assets: dir.clone(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    binary: None,
                    sockets: None,
                    access: Access::Local,
                },
                authority: "fixture".into(),
                vms: tokio::sync::RwLock::new(BTreeMap::new()),
                edge: edge.clone(),
                changes: Mutex::new(BTreeMap::new()),
                applying: Arc::new(Mutex::new(())),
                stopping: tokio::sync::watch::channel(false).0,
            });
            Self {
                op,
                edge,
                dir,
                creation,
            }
        }
        async fn preview(&self, name: &str) -> Result<Deletion> {
            let response = preview(State(self.op.clone()), Path(name.parse().unwrap()))
                .await?
                .0;
            Deletion::load(&self.op.config.state, response.id)
        }
        async fn start(&self, job: &Deletion) -> Result<Json<DeletionView>> {
            start(
                State(self.op.clone()),
                Path(job.id),
                Json(Confirm {
                    name: job.cell().name.clone(),
                    approval: job.approval,
                }),
            )
            .await
        }
        async fn settled(&self, id: RequestId) -> Deletion {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let job = Deletion::load(&self.op.config.state, id).unwrap();
                    if !matches!(job.phase, DeletionPhase::Deleting { .. })
                        && self.op.applying.try_lock().is_ok()
                    {
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
    async fn confirmed_leaf_deletion_cleans_only_owned_resources_and_retires_creation() {
        let f = Fixture::new();
        let job = f.preview("project-dev").await.unwrap();
        assert_eq!(job.phase, DeletionPhase::Review);
        assert!(f.edge.calls.lock().unwrap().is_empty());
        for (name, approval) in [("project", job.approval), ("project-dev", Hash::ZERO)] {
            assert!(start(
                State(f.op.clone()),
                Path(job.id),
                Json(Confirm {
                    name: name.parse().unwrap(),
                    approval
                })
            )
            .await
            .is_err());
        }
        assert!(f.edge.calls.lock().unwrap().is_empty());
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        assert!(!f.dir.join("tree/project/project-dev.json").exists());
        assert!(f.dir.join("tree/project.json").exists());
        assert!(!f.dir.join("tree/.pins/project-dev.pub").exists());
        assert_eq!(
            f.edge
                .vms
                .lock()
                .unwrap()
                .iter()
                .map(|v| v.vm_name.as_str())
                .collect::<Vec<_>>(),
            vec!["project", "unrelated"]
        );
        let integrations = f.edge.integrations.lock().unwrap();
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name, "shared-repo");
        assert_eq!(
            integrations[0].attachments,
            vec!["tag:other", "vm:unrelated"]
        );
        drop(integrations);
        assert!(matches!(
            creation::list(&f.op.config.state).unwrap()[0].phase,
            creation::CreationPhase::Deleted
        ));
        let denied = creation::start(
            State(f.op.clone()),
            Path(f.creation),
            Json(Apply {
                approval: Hash::ZERO,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            denied.0,
            StatusCode::GONE,
            "old creation cannot resurrect the deleted cell"
        );
        let calls = f.edge.calls.lock().unwrap().len();
        f.edge.vms.lock().unwrap().push(Vm {
            created_at: Some("new-incarnation".into()),
            ..vm("project-dev")
        });
        let _ = f.start(&job).await.unwrap();
        assert_eq!(
            f.edge.calls.lock().unwrap().len(),
            calls,
            "old deletion cannot delete a reused name"
        );
        assert!(f
            .edge
            .vms
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.vm_name == "project-dev"));
    }

    #[tokio::test]
    async fn a_lost_delete_acknowledgement_recovers_without_deleting_twice() {
        let f = Fixture::new();
        let job = f.preview("project-dev").await.unwrap();
        f.edge.lose_delete_reply.store(true, Ordering::SeqCst);
        let _ = f.start(&job).await.unwrap();
        let mut failed = f.settled(job.id).await;
        assert!(matches!(failed.phase, DeletionPhase::Failed { .. }));
        assert!(f.dir.join("tree/project/project-dev.json").exists());
        assert!(ensure_available(&f.op.config.state, &job.cell().name).is_err());
        // Restart after dispatch, before a completed receipt was written.
        failed.phase = DeletionPhase::Deleting {
            step: DeletionStep::DeletingVm,
        };
        failed.save(&f.op.config.state).unwrap();
        recover(&f.op.config.state).unwrap();
        assert!(matches!(
            Deletion::load(&f.op.config.state, job.id).unwrap().phase,
            DeletionPhase::Failed { .. }
        ));
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        assert_eq!(
            f.edge
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|args| args[0] == "rm")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn children_and_ambiguous_vm_ownership_are_refused() {
        let f = Fixture::new();
        assert!(f
            .preview("project")
            .await
            .unwrap_err()
            .1
            .contains("subcells"));
        f.edge.vms.lock().unwrap().push(Vm {
            vm_name: "duplicate".into(),
            ..vm("project-dev")
        });
        assert!(f
            .preview("project-dev")
            .await
            .unwrap_err()
            .1
            .contains("Multiple VMs"));
        f.edge.vms.lock().unwrap().pop();
        f.edge.vms.lock().unwrap()[1]
            .tags
            .push("cell-someone-else".into());
        assert!(f
            .preview("project-dev")
            .await
            .unwrap_err()
            .1
            .contains("multiple cells"));
        f.edge.vms.lock().unwrap()[1].tags.pop();
        f.edge.vms.lock().unwrap()[1].created_at = None;
        assert!(f
            .preview("project-dev")
            .await
            .unwrap_err()
            .1
            .contains("creation identity"));
        assert!(f.edge.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_vm_declaration_pin_and_connections_stop_before_any_action() {
        for change in [
            "vm",
            "declaration",
            "pin",
            "connection",
            "new-attachment",
            "child",
        ] {
            let f = Fixture::new();
            let job = f.preview("project-dev").await.unwrap();
            match change {
                "vm" => f.edge.vms.lock().unwrap()[1].created_at = Some("replacement".into()),
                "declaration" => {
                    fs::rename(
                        f.dir.join("tree/project/project-dev.json"),
                        f.dir.join("tree/project-dev.json"),
                    )
                    .unwrap();
                }
                "pin" => {
                    fs::write(f.dir.join("tree/.pins/project-dev.pub"), "replacement-pin").unwrap()
                }
                "connection" => {
                    f.edge.integrations.lock().unwrap()[0].config =
                        json!({"repositories":["other/private"]})
                }
                "new-attachment" => f.edge.integrations.lock().unwrap().push(Integration {
                    name: "new-grant".into(),
                    kind: "github".into(),
                    attachments: vec!["tag:cell-project-dev".into()],
                    config: json!({}),
                }),
                "child" => {
                    fs::create_dir_all(f.dir.join("tree/project/project-dev")).unwrap();
                    fs::write(
                        f.dir.join("tree/project/project-dev/nested.json"),
                        br#"{"name":"nested","grants":[]}"#,
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let response = f.start(&job).await.unwrap();
            assert!(
                matches!(
                    &response.0.phase,
                    DeletionPhase::Failed {
                        recovery: RecoveryAction::Review,
                        ..
                    }
                ),
                "must require a fresh review after {change}"
            );
            assert!(
                f.edge.calls.lock().unwrap().is_empty(),
                "must not act after {change}"
            );
            assert_eq!(
                Deletion::load(&f.op.config.state, job.id).unwrap().phase,
                response.0.phase
            );
        }
    }

    #[tokio::test]
    async fn a_shared_parent_door_is_detached_but_not_removed() {
        let f = Fixture::new();
        f.edge.integrations.lock().unwrap()[1]
            .attachments
            .push("vm:unrelated".into());
        let job = f.preview("project-dev").await.unwrap();
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        let integrations = f.edge.integrations.lock().unwrap();
        let door = integrations
            .iter()
            .find(|i| i.name == "door-project-dev")
            .unwrap();
        assert_eq!(door.attachments, vec!["vm:unrelated"]);
    }

    #[tokio::test]
    async fn an_already_absent_vm_can_be_removed_from_the_tree() {
        let f = Fixture::new();
        f.edge
            .vms
            .lock()
            .unwrap()
            .retain(|v| v.vm_name != "project-dev");
        // A VM-specific attachment can survive outside our target's tag; with
        // no bound VM identity it is deliberately left alone.
        let job = f.preview("project-dev").await.unwrap();
        assert!(job.target.vm.is_none());
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        assert!(f.edge.calls.lock().unwrap().iter().all(|a| a[0] != "rm"));
    }
}
