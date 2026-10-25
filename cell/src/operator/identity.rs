//! Trusted supervisor migration: one additive atomic registry commit. Existing
//! runtime protocols remain frozen, so signed logs and active runs are untouched.
use super::*;
use crate::identity::{Migration, Observation};
use crate::CellId;

pub(super) fn revision(op: &Operator) -> Result<Option<Hash>> {
    let registry = op.identities()?;
    if !op.config.tree.join(".identities.json").exists() { return Ok(None); }
    let hosts = crate::host_binding::revision(&op.config.tree).map_err(bad)?;
    Ok(Some(if hosts.is_zero() { registry.revision() } else { Hash::of(&crate::canon::bytes(&(registry.revision(), hosts))) }))
}
pub(super) fn check_revision(op: &Operator, reviewed: Option<Hash>) -> Result<()> {
    if revision(op)? != reviewed {
        return Err(bad("Cell identities changed; review this operation again"));
    }
    Ok(())
}
fn settled(op: &Operator) -> Result<()> {
    if creation::list(&op.config.state)
        .map_err(fail)?
        .iter()
        .any(|c| {
            !matches!(
                c.phase,
                creation::CreationPhase::Review
                    | creation::CreationPhase::Ready { .. }
                    | creation::CreationPhase::Deleted
            )
        })
        || deletion::list(&op.config.state)
            .map_err(fail)?
            .iter()
            .any(|c| c.active())
    {
        return Err(bad(
            "Finish pending cell setup or deletion before migrating identities",
        ));
    }
    Ok(())
}
async fn observe(op: &Operator) -> Result<Vec<Observation>> {
    let tree = op.config.tree.clone();
    let local = op.config.sockets.is_some();
    let edge = op.edge.clone();
    tokio::task::spawn_blocking(move || {
        let vms = if local {
            None
        } else {
            Some(edge.listing().map_err(|e| e.to_string())?.0)
        };
        let observed =
            crate::identity::observe(&tree, vms.as_deref()).map_err(|e| e.to_string())?;
        for cell in &observed {
            if let Some(binding) = crate::host_binding::Binding::load(&tree, &cell.cell.name).map_err(|e| e.to_string())? {
                binding.observe(&cell.cell).map_err(|e| e.to_string())?;
            }
            if let Some(vm) = &cell.vm {
                let (key, statements) = edge
                    .log(&vm.name)
                    .map_err(|e| e.to_string())?
                    .ok_or("Cell has no host identity")?;
                if key != cell.key {
                    return Err("VM key does not match the pinned cell identity".to_owned());
                }
                let mut sequence = None;
                for statement in statements {
                    if statement.cell != cell.cell.name
                        || !statement.follows(sequence)
                        || statement.open(&key).is_err()
                    {
                        return Err(
                            "Cell signed history does not verify under its pinned identity"
                                .to_owned(),
                        );
                    }
                    sequence = Some(statement.seq);
                }
            }
        }
        Ok(observed)
    })
    .await
    .map_err(fail)?
    .map_err(bad)
}
fn path(op: &Operator, id: RequestId) -> PathBuf {
    op.config
        .state
        .join("identity-migrations")
        .join(format!("{id}.json"))
}
fn load(op: &Operator, id: RequestId) -> Result<Migration> {
    let plan: Migration = serde_json::from_slice(
        &fs::read(path(op, id)).map_err(|_| bad("Identity migration not found"))?,
    )
    .map_err(bad)?;
    if plan.id != id {
        return Err(bad("Identity migration mismatch"));
    }
    Ok(plan)
}
pub(super) async fn plan(State(op): State<Arc<Operator>>) -> Result<Json<Migration>> {
    let _lock = op
        .applying
        .try_lock()
        .map_err(|_| bad("Another cell change is in progress"))?;
    settled(&op)?;
    let observed = observe(&op).await?;
    let plan = Migration::plan(&op.config.tree, &observed).map_err(bad)?;
    fs::create_dir_all(op.config.state.join("identity-migrations")).map_err(fail)?;
    crate::identity::atomic(&path(&op, plan.id), &crate::canon::bytes(&plan)).map_err(fail)?;
    Ok(Json(plan))
}
pub(super) async fn get(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
) -> Result<Json<Migration>> {
    Ok(Json(load(&op, id)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approve {
    approval: Hash,
}
pub(super) async fn apply(
    State(op): State<Arc<Operator>>,
    Path(id): Path<RequestId>,
    Json(body): Json<Approve>,
) -> Result<Json<Migration>> {
    let _lock = op
        .applying
        .try_lock()
        .map_err(|_| bad("Another cell change is in progress"))?;
    settled(&op)?;
    let plan = load(&op, id)?;
    let observed = observe(&op).await?;
    plan.apply(&op.config.tree, &observed, body.approval)
        .map_err(bad)?;
    op.changes.lock().await.clear();
    Ok(Json(plan))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rename {
    name: Name,
}
pub(super) async fn rename(
    State(op): State<Arc<Operator>>,
    Path(route): Path<Name>,
    Json(body): Json<Rename>,
) -> Result<Json<Value>> {
    let _lock = op
        .applying
        .try_lock()
        .map_err(|_| bad("Another cell change is in progress"))?;
    settled(&op)?;
    let binding = op.resolve(&route)?;
    deletion::ensure_available(&op.config.state, &binding)?;
    let mut registry = op.identities()?;
    let id: CellId = registry
        .by_binding(&binding)
        .ok_or_else(|| bad("Migrate this cell's identity before renaming"))?
        .id
        .clone();
    registry
        .rename(&op.config.tree, &id, body.name.clone())
        .map_err(bad)?;
    op.changes.lock().await.clear();
    Ok(Json(json!({"id":id,"name":body.name})))
}

pub(super) async fn enroll(op: &Operator, binding: &Name, label: &Name) -> Result<()> {
    let observed = observe(op).await?;
    let proof = observed
        .iter()
        .find(|o| &o.cell.name == binding)
        .ok_or_else(|| bad("New cell identity is missing"))?;
    let id = binding.as_str().parse::<CellId>().map_err(bad)?;
    let mut registry = op.identities()?;
    if let Some(existing) = registry.by_binding(binding) {
        if existing.id != id || existing.key != proof.key || existing.vm != proof.vm {
            return Err(bad("New cell identity changed"));
        }
        return Ok(());
    }
    registry.cells.push(crate::identity::Identity {
        id,
        binding: binding.clone(),
        name: label.clone(),
        key: proof.key,
        vm: proof.vm.clone(),
    });
    registry.save(&op.config.tree).map_err(bad)
}
