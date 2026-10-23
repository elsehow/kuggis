//! Explicit, durable deletion. Provisioning's apply loop remains non-destructive.
//! Approval binds a leaf, its VM incarnation, and the exact owned attachments.
use super::resource_cleanup::{self, CleanupView, Coverage};
use super::*;
use crate::exe::{Integration, Vm};
use crate::publish::registry::resources::CleanupReceipt;
use crate::resource::{ResourceId, ResourceOwner};
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
    parent: Option<CellId>,
    vm: Option<VmIdentity>,
    attachments: Vec<Attachment>,
    door: Option<Door>,
    pin: Option<Hash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resources: Option<Coverage>,
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
    CleaningUpResources,
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
    #[serde(default)]
    resources_done: BTreeMap<RequestId, CleanupReceipt>,
    #[serde(default)]
    superseded: bool,
    #[serde(default)]
    begun: bool,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub(crate) struct DeletionView {
    pub id: RequestId,
    cell: Cell,
    parent: Option<CellId>,
    vm: Option<String>,
    approval: Hash,
    phase: DeletionPhase,
    resources: Vec<CleanupView>,
    resources_reviewed: bool,
}
impl Deletion {
    pub fn cell(&self) -> &Cell {
        &self.target.cell
    }
    pub fn parent(&self) -> &Option<CellId> {
        &self.target.parent
    }
    pub fn active(&self) -> bool {
        !self.superseded
            && !matches!(self.phase, DeletionPhase::Deleted)
            && (self.begun
                || !self.resources_done.is_empty()
                || matches!(
                    self.phase,
                    DeletionPhase::Deleting { .. }
                        | DeletionPhase::Failed {
                            recovery: RecoveryAction::Retry,
                            ..
                        }
                ))
    }
    pub fn view(&self) -> DeletionView {
        DeletionView {
            id: self.id,
            cell: self.target.cell.clone(),
            parent: self.target.parent.clone(),
            vm: self.target.vm.as_ref().map(|v| v.name.to_string()),
            approval: self.approval,
            phase: self.phase.clone(),
            resources: self
                .target
                .resources
                .as_ref()
                .map(|r| r.view(&self.resources_done))
                .unwrap_or_default(),
            resources_reviewed: self.target.resources.is_some(),
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
pub(super) fn ensure_available(state: &FsPath, name: &CellId) -> Result<()> {
    if list(state)
        .map_err(fail)?
        .iter()
        .any(|j| j.active() && j.cell().id == *name)
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "This cell is being deleted. Finish its deletion first.".into(),
        ));
    }
    Ok(())
}

fn pin(tree: &FsPath, name: &CellId) -> std::io::Result<Option<Hash>> {
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

fn identity(vms: &[Vm], name: &CellId) -> Result<Option<VmIdentity>> {
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
    cell: &CellId,
    parent: &Option<CellId>,
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
    if let Some(current) = tree.get(&target.cell.id) {
        if current != &target.cell
            || tree.parent(&current.id).map(|p| &p.id) != target.parent.as_ref()
        {
            return Err(Error(
                StatusCode::CONFLICT,
                "This cell changed. Review its deletion again.".into(),
            ));
        }
        if tree.children(&current.id).next().is_some() {
            return Err(bad("Delete this cell's subcells first"));
        }
    }
    if creation::list(&op.config.state)
        .map_err(fail)?
        .iter()
        .any(|j| j.is_current() && j.parent.as_ref() == Some(&target.cell.id))
    {
        return Err(bad("Delete this cell's subcells first"));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Options {
    #[serde(default)]
    choices: BTreeMap<ResourceId, ResourceOwner>,
}
pub(super) async fn preview_request(
    State(op): State<Arc<Operator>>,
    Path(name): Path<CellId>,
    body: axum::body::Bytes,
) -> Result<Json<DeletionView>> {
    if body.len() > crate::resource_rpc::MAX_RPC_BYTES {
        return Err(bad("Deletion choices exceed limit"));
    }
    let options = if body.is_empty() {
        Options::default()
    } else {
        serde_json::from_slice(&body).map_err(bad)?
    };
    preview_with(op, name, options).await
}
#[cfg(test)]
pub(super) async fn preview(
    State(op): State<Arc<Operator>>,
    Path(name): Path<CellId>,
) -> Result<Json<DeletionView>> {
    preview_with(op, name, Options::default()).await
}
async fn preview_with(
    op: Arc<Operator>,
    name: CellId,
    options: Options,
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
    let prior = list(&op.config.state)
        .map_err(fail)?
        .into_iter()
        .filter(|j| j.cell().id == name && !matches!(j.phase, DeletionPhase::Deleted))
        .max_by_key(|j| (j.resources_done.len(), j.active()));
    if let Some(job) = prior
        .as_ref()
        .filter(|j| matches!(j.phase, DeletionPhase::Deleting { .. }))
    {
        return Ok(Json(job.view()));
    }
    let (tree, _) = op.tree()?;
    let (cell, parent) = if let Some(cell) = tree.get(&name) {
        (cell.clone(), tree.parent(&name).map(|p| p.id.clone()))
    } else {
        let job = creation::list(&op.config.state)
            .map_err(fail)?
            .into_iter()
            .find(|j| j.cell.id == name && j.is_current())
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
        resources: None,
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
    let done = prior
        .as_ref()
        .map(|j| j.resources_done.clone())
        .unwrap_or_default();
    let previous = prior.as_ref().and_then(|j| j.target.resources.clone());
    let worker = op.clone();
    let cell = target.cell.clone();
    let (coverage, done) = tokio::task::spawn_blocking(move || {
        let mut done = done;
        let coverage = resource_cleanup::prepare(
            &worker,
            &cell,
            &options.choices,
            previous.as_ref(),
            &mut done,
        )?;
        Ok::<_, Error>((coverage, done))
    })
    .await
    .map_err(fail)??;
    target.resources = Some(coverage);
    // New approvals supersede failed/review jobs for this cell, but not their
    // historical resource receipts. They remain inspectable and cannot run.
    for mut old in list(&op.config.state)
        .map_err(fail)?
        .into_iter()
        .filter(|j| j.cell().id == name && !matches!(j.phase, DeletionPhase::Deleted))
    {
        old.superseded = true;
        old.phase = DeletionPhase::Failed {
            message: "Superseded by 904254dc-1b8f-5e80-9755-e5afc5b195c5 new deletion review"
                .into(),
            step: None,
            recovery: RecoveryAction::Review,
        };
        old.save(&op.config.state).map_err(fail)?;
    }
    let job = Deletion {
        id: RequestId::generate(),
        approval: ApprovedHash::of(&target),
        target,
        phase: DeletionPhase::Review,
        resources_done: done,
        superseded: false,
        begun: prior.as_ref().is_some_and(|j| j.begun),
    };
    job.save(&op.config.state).map_err(fail)?;
    Ok(Json(job.view()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Confirm {
    approval: Hash,
    name: Name,
    #[serde(default)]
    acknowledge_manual: bool,
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
    if job.superseded {
        return Err(bad(
            "This deletion approval was superseded; use the new review",
        ));
    }
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
        .any(|j| j.active() && j.cell().id == job.cell().id && j.id != id)
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "This cell already has 904254dc-1b8f-5e80-9755-e5afc5b195c5 deletion in progress"
                .into(),
        ));
    }
    let preflight = async {
        check_tree(&op, &job.target)?;
        let check = op.clone();
        let target = job.target.clone();
        let done = job.resources_done.clone();
        tokio::task::spawn_blocking(move || {
            remaining(&check, &target)?;
            resource_cleanup::check(&check, target.resources.as_ref(), &done)
        })
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
            recovery: if error.0 == StatusCode::BAD_GATEWAY {
                RecoveryAction::Retry
            } else {
                RecoveryAction::Review
            },
        };
        job.save(&op.config.state).map_err(fail)?;
        return Ok(Json(job.view()));
    }
    if job.target.resources.as_ref().is_some_and(Coverage::manual) && !body.acknowledge_manual {
        return Err(bad(
            "Acknowledge continuing costs and manual provider cleanup before deleting",
        ));
    }
    job.begun = true;
    job.phase = DeletionPhase::Deleting {
        step: DeletionStep::CleaningUpResources,
    };
    job.save(&op.config.state).map_err(fail)?;
    let initial = job.view();
    tokio::spawn(async move {
        let worker = op.clone();
        let work = job.clone();
        let result = tokio::task::spawn_blocking(move || {
            execute_with_manual(&worker, ConfirmedDeletion(work), body.acknowledge_manual)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                op.vms.write().await.remove(&job.cell().id);
            }
            failure => {
                let recovery = match &failure {
                    Ok(Err(Error(status, _)))
                        if *status == StatusCode::BAD_REQUEST
                            || *status == StatusCode::CONFLICT =>
                    {
                        RecoveryAction::Review
                    }
                    _ => RecoveryAction::Retry,
                };
                if let Ok(persisted) = Deletion::load(&op.config.state, job.id) {
                    job = persisted;
                }
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
                    recovery,
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
    let live = identity(&vms, &target.cell.id)?;
    if live.is_some() && live != target.vm {
        return Err(bad("The VM changed since this deletion was reviewed"));
    }
    if let Some(vm) = &target.vm {
        if vms.iter().any(|v| {
            v.vm_name == vm.name.as_str()
                && (v.created_at.as_ref() != Some(&vm.created_at)
                    || !v.tags.contains(&target.cell.tag()))
        }) {
            return Err(bad(
                "The VM name now belongs to 904254dc-1b8f-5e80-9755-e5afc5b195c5 different VM",
            ));
        }
    }
    let current_pin = pin(&op.config.tree, &target.cell.id).map_err(fail)?;
    if current_pin != target.pin && !(live.is_none() && current_pin.is_none()) {
        return Err(bad("The cell's pinned identity changed"));
    }
    let (attachments, door) = owned(
        &vms,
        &integrations,
        &target.cell.id,
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

fn execute_with_manual(
    op: &Operator,
    ConfirmedDeletion(mut job): ConfirmedDeletion,
    acknowledge_manual: bool,
) -> Result<()> {
    let target = job.target.clone();
    check_tree(op, &target)?;
    remaining(op, &target)?;
    resource_cleanup::check(op, target.resources.as_ref(), &job.resources_done)?;
    let coverage = target
        .resources
        .as_ref()
        .ok_or_else(|| bad("Resource review required"))?;
    for binding in &coverage.bindings {
        if job.resources_done.contains_key(&binding.operation) {
            continue;
        }
        job.phase = DeletionPhase::Deleting {
            step: DeletionStep::CleaningUpResources,
        };
        job.save(&op.config.state).map_err(fail)?;
        let receipt = resource_cleanup::perform(op, binding, acknowledge_manual)?;
        job.resources_done.insert(binding.operation, receipt);
        job.save(&op.config.state).map_err(fail)?;
    }
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
    creation::retire(&op.config.state, &target.cell.id).map_err(fail)?;
    let (_, paths) = op.tree()?;
    if let Some(path) = paths.get(&target.cell.id) {
        fs::remove_file(path).map_err(fail)?;
        fs::File::open(path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(fail)?;
    }
    let pin_path = op
        .config
        .tree
        .join(".pins")
        .join(format!("{}.pub", target.cell.id));
    let current_pin = pin(&op.config.tree, &target.cell.id).map_err(fail)?;
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
        before_command: StdMutex<Option<Box<dyn Fn() + Send + Sync>>>,
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
            if let Some(check) = self.before_command.lock().unwrap().as_ref() {
                check();
            }
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
        secret: crate::Secret,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("cell-delete-{}", RequestId::generate()));
            fs::create_dir_all(dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819")).unwrap();
            fs::create_dir_all(dir.join("tree/.pins")).unwrap();
            fs::create_dir_all(dir.join("state/creations")).unwrap();
            fs::write(
                dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819.json"),
                br#"{"id":"44fc36e4-5adc-51aa-a6e3-3db15ade9819","name":"44fc36e4-5adc-51aa-a6e3-3db15ade9819","grants":[]}"#,
            )
            .unwrap();
            fs::write(
                dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json"),
                br#"{"id":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","name":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","grants":[]}"#,
            )
            .unwrap();
            let secret = crate::Secret::generate();
            fs::write(
                dir.join("tree/.pins/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.pub"),
                secret.public().to_string(),
            )
            .unwrap();
            let creation = RequestId::generate();
            fs::write(dir.join("state/creations").join(format!("{creation}.json")), json!({
                "id":creation,"cell":{"id":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","name":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","grants":[]},"parent":"44fc36e4-5adc-51aa-a6e3-3db15ade9819","before":Hash::ZERO,"approval":Hash::ZERO,"plans":{},
                "phase":{"state":"ready","session":{"cell":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","id":"a".repeat(32)}}
            }).to_string()).unwrap();
            let edge = Arc::new(Fake::default());
            *edge.vms.lock().unwrap() = vec![
                vm("44fc36e4-5adc-51aa-a6e3-3db15ade9819"),
                vm("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21"),
                vm("unrelated"),
            ];
            *edge.integrations.lock().unwrap() = vec![
                Integration {
                    name: "shared-repo".into(),
                    kind: "github".into(),
                    attachments: vec![
                        "tag:cell-e95cc7f0-21bb-5212-8fa9-abbfde3f6b21".into(),
                        "vm:e95cc7f0-21bb-5212-8fa9-abbfde3f6b21".into(),
                        "tag:70ee78ad-5e3a-510a-af20-6f485ecb6f16".into(),
                        "vm:unrelated".into(),
                    ],
                    config: json!({"repositories":["0153ed41-b34f-506b-837e-1610aeac2bce/repo"],"readonly":true}),
                },
                Integration {
                    name: "door-e95cc7f0-21bb-5212-8fa9-abbfde3f6b21".into(),
                    kind: "http-proxy".into(),
                    attachments: vec!["tag:cell-44fc36e4-5adc-51aa-a6e3-3db15ade9819".into()],
                    config: json!({"peer_vm":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21","target":"https://e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.exe.xyz/"}),
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
                authority: "ed7ed6fb-d4e5-52bc-bf5a-cec3e60ce5ef".into(),
                relays: Mutex::new(BTreeMap::new()),
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
                secret,
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
                    acknowledge_manual: false,
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
        let job = f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap();
        assert_eq!(job.phase, DeletionPhase::Review);
        assert!(f.edge.calls.lock().unwrap().is_empty());
        for (name, approval) in [
            ("44fc36e4-5adc-51aa-a6e3-3db15ade9819", job.approval),
            ("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21", Hash::ZERO),
        ] {
            assert!(start(
                State(f.op.clone()),
                Path(job.id),
                Json(Confirm {
                    name: name.parse().unwrap(),
                    approval,
                    acknowledge_manual: false,
                })
            )
            .await
            .is_err());
        }
        assert!(f.edge.calls.lock().unwrap().is_empty());
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        assert!(!f.dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json").exists());
        assert!(f
            .dir
            .join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819.json")
            .exists());
        assert!(!f
            .dir
            .join("tree/.pins/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.pub")
            .exists());
        assert_eq!(
            f.edge
                .vms
                .lock()
                .unwrap()
                .iter()
                .map(|v| v.vm_name.as_str())
                .collect::<Vec<_>>(),
            vec!["44fc36e4-5adc-51aa-a6e3-3db15ade9819", "unrelated"]
        );
        let integrations = f.edge.integrations.lock().unwrap();
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name, "shared-repo");
        assert_eq!(
            integrations[0].attachments,
            vec!["tag:70ee78ad-5e3a-510a-af20-6f485ecb6f16", "vm:unrelated"]
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
            ..vm("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
        });
        let _ = f.start(&job).await.unwrap();
        assert_eq!(
            f.edge.calls.lock().unwrap().len(),
            calls,
            "old deletion cannot delete 904254dc-1b8f-5e80-9755-e5afc5b195c5 reused name"
        );
        assert!(f
            .edge
            .vms
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.vm_name == "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21"));
    }

    #[tokio::test]
    async fn a_lost_delete_acknowledgement_recovers_without_deleting_twice() {
        let f = Fixture::new();
        let job = f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap();
        f.edge.lose_delete_reply.store(true, Ordering::SeqCst);
        let _ = f.start(&job).await.unwrap();
        let mut failed = f.settled(job.id).await;
        assert!(matches!(failed.phase, DeletionPhase::Failed { .. }));
        assert!(f.dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json").exists());
        assert!(ensure_available(&f.op.config.state, &job.cell().id).is_err());
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
            .preview("44fc36e4-5adc-51aa-a6e3-3db15ade9819")
            .await
            .unwrap_err()
            .1
            .contains("subcells"));
        f.edge.vms.lock().unwrap().push(Vm {
            vm_name: "duplicate".into(),
            ..vm("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
        });
        assert!(f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap_err()
            .1
            .contains("Multiple VMs"));
        f.edge.vms.lock().unwrap().pop();
        f.edge.vms.lock().unwrap()[1]
            .tags
            .push("cell-someone-else".into());
        assert!(f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap_err()
            .1
            .contains("multiple cells"));
        f.edge.vms.lock().unwrap()[1].tags.pop();
        f.edge.vms.lock().unwrap()[1].created_at = None;
        assert!(f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
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
            "e3e321ad-3f5e-5221-9ec8-ccc87947084b",
        ] {
            let f = Fixture::new();
            let job = f
                .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
                .await
                .unwrap();
            match change {
                "vm" => f.edge.vms.lock().unwrap()[1].created_at = Some("replacement".into()),
                "declaration" => {
                    fs::rename(
                        f.dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json"),
                        f.dir.join("tree/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json"),
                    )
                    .unwrap();
                }
                "pin" => fs::write(
                    f.dir
                        .join("tree/.pins/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.pub"),
                    "replacement-pin",
                )
                .unwrap(),
                "connection" => {
                    f.edge.integrations.lock().unwrap()[0].config =
                        json!({"repositories":["70ee78ad-5e3a-510a-af20-6f485ecb6f16/private"]})
                }
                "new-attachment" => f.edge.integrations.lock().unwrap().push(Integration {
                    name: "new-grant".into(),
                    kind: "github".into(),
                    attachments: vec!["tag:cell-e95cc7f0-21bb-5212-8fa9-abbfde3f6b21".into()],
                    config: json!({}),
                }),
                "e3e321ad-3f5e-5221-9ec8-ccc87947084b" => {
                    fs::create_dir_all(f.dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")).unwrap();
                    fs::write(
                        f.dir.join("tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/nested.json"),
                        br#"{"id":"36224229-3ce0-542f-8f98-1b371e7efc3a","name":"nested","grants":[]}"#,
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
                "must require 904254dc-1b8f-5e80-9755-e5afc5b195c5 fresh review after {change}"
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
        let job = f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap();
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        let integrations = f.edge.integrations.lock().unwrap();
        let door = integrations
            .iter()
            .find(|i| i.name == "door-e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
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
            .retain(|v| v.vm_name != "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21");
        // A VM-specific attachment can survive outside our target's tag; with
        // no bound VM identity it is deliberately left alone.
        let job = f
            .preview("e95cc7f0-21bb-5212-8fa9-abbfde3f6b21")
            .await
            .unwrap();
        assert!(job.target.vm.is_none());
        let _ = f.start(&job).await.unwrap();
        assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
        assert!(f.edge.calls.lock().unwrap().iter().all(|a| a[0] != "rm"));
    }
    mod resources;
}
