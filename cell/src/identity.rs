//! Additive identity migration. Runtime bindings are immutable compatibility
//! addresses, never display labels. No signed bytes, sessions or grants change.
use crate::{apply::Fallible, exe::Vm, Cell, CellId, Hash, Key, Name, RequestId};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct VmBinding {
    pub name: String,
    pub created_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: CellId,
    // Frozen v1 route, VM tag, pin filename and signed session identity.
    pub binding: Name,
    pub name: Name,
    #[ts(type = "string")]
    pub key: Key,
    pub vm: Option<VmBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u32,
    pub cells: Vec<Identity>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            cells: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub cell: Cell,
    pub parent: Option<Name>,
    #[ts(type = "string")]
    pub key: Key,
    pub vm: Option<VmBinding>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub id: RequestId,
    pub version: u32,
    pub before: Hash,
    pub snapshot: Hash,
    pub registry: Registry,
    pub approval: Hash,
}
const FILE: &str = ".identities.json";
fn read(path: &Path) -> Fallible<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.permissions().mode() & 0o022 != 0 {
        return Err("identity state must be a regular private operator-owned file".into());
    }
    Ok(fs::read(path)?)
}
pub fn atomic(path: &Path, bytes: &[u8]) -> Fallible {
    let parent = path.parent().ok_or("missing parent directory")?;
    let temp = parent.join(format!(".identity-{}.tmp", RequestId::generate()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
impl Registry {
    pub fn load(tree: &Path) -> Fallible<Self> {
        let path = tree.join(FILE);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
            Ok(_) => {
                let value: Self = serde_json::from_slice(&read(&path)?)?;
                value.validate()?;
                Ok(value)
            }
        }
    }
    pub fn revision(&self) -> Hash {
        Hash::of(&crate::canon::bytes(self))
    }
    pub fn validate(&self) -> Fallible {
        if self.version != 1 {
            return Err("unsupported identity registry version".into());
        }
        let mut ids = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        let mut names = BTreeSet::new();
        for cell in &self.cells {
            if !ids.insert(cell.id.clone())
                || !bindings.insert(cell.binding.clone())
                || !names.insert(cell.name.clone())
            {
                return Err("duplicate identity, runtime binding or display name".into());
            }
        }
        for cell in &self.cells {
            if self.cells.iter().any(|other| {
                other.id != cell.id
                    && (other.binding.as_str() == cell.id.as_str()
                        || other.id.as_str() == cell.name.as_str())
            }) {
                return Err("cell name or runtime binding conflicts with a reserved UUID".into());
            }
        }
        Ok(())
    }
    pub fn by_binding(&self, name: &Name) -> Option<&Identity> {
        self.cells.iter().find(|c| &c.binding == name)
    }
    pub fn by_route(&self, route: &Name) -> Option<&Identity> {
        self.cells
            .iter()
            .find(|c| c.id.as_str() == route.as_str() || &c.binding == route)
    }
    /// CLI names may mean a current label or a frozen runtime alias. Never
    /// silently choose between different cells after a display name is reused.
    pub fn resolve_cli(&self, tree: &crate::Tree, value: &Name) -> Fallible<Name> {
        if let Some(cell) = self.cells.iter().find(|c| c.id.as_str() == value.as_str()) {
            return Ok(cell.binding.clone());
        }
        let mut matches = BTreeSet::new();
        if let Some(cell) = self.by_binding(value) {
            matches.insert(cell.binding.clone());
        }
        for cell in tree.iter() {
            if &self.label(&cell.name) == value || &cell.name == value {
                matches.insert(cell.name.clone());
            }
        }
        if matches.len() > 1 {
            return Err(format!("Cell name '{value}' is ambiguous; use a cell UUID").into());
        }
        Ok(matches.into_iter().next().unwrap_or_else(|| value.clone()))
    }
    pub fn route(&self, binding: &Name) -> String {
        self.by_binding(binding)
            .map(|c| c.id.to_string())
            .unwrap_or_else(|| binding.to_string())
    }
    pub fn label(&self, binding: &Name) -> Name {
        self.by_binding(binding)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| binding.clone())
    }
    pub fn check_pin(&self, tree: &Path, binding: &Name) -> Fallible {
        if let Some(record) = self.by_binding(binding) {
            if pin(tree, binding)? != record.key {
                return Err(
                    "cell identity pin changed; explicit identity repair is required".into(),
                );
            }
        }
        Ok(())
    }
    pub fn check_vm(&self, binding: &Name, vm: Option<&Vm>) -> Fallible {
        if let Some(record) = self.by_binding(binding) {
            let expected = record
                .vm
                .as_ref()
                .ok_or("local identity cannot target a remote VM")?;
            let current =
                vm.ok_or("registered cell VM is missing; explicit identity repair is required")?;
            if current.vm_name != expected.name
                || current.created_at.as_deref() != Some(&expected.created_at)
            {
                return Err(
                    "cell VM incarnation changed; explicit identity repair is required".into(),
                );
            }
        }
        Ok(())
    }
    pub fn check_active(&self, root: &Path, tree: &crate::Tree) -> Fallible {
        crate::host_binding::validate_tree(root, tree)?;
        let mut routes = BTreeSet::new();
        let mut labels = BTreeSet::new();
        for cell in tree.iter() {
            self.check_pin(root, &cell.name)?;
            if !routes.insert(self.route(&cell.name))
                || !labels.insert(self.label(&cell.name))
                || self.cells.iter().any(|known| {
                    known.binding != cell.name && known.id.as_str() == cell.name.as_str()
                })
            {
                return Err("cell declaration conflicts with a reserved identity or label".into());
            }
        }
        Ok(())
    }
    pub fn save(&self, tree: &Path) -> Fallible {
        self.validate()?;
        atomic(&tree.join(FILE), &crate::canon::bytes(self))
    }
    pub fn rename(&mut self, tree: &Path, id: &CellId, name: Name) -> Fallible {
        let (active, _) = crate::tree::load(tree)?;
        let record = self
            .cells
            .iter()
            .find(|c| &c.id == id)
            .ok_or("unknown cell UUID")?;
        let binding = record.binding.clone();
        if active.get(&binding).is_none() {
            return Err("cell no longer exists".into());
        }
        self.check_pin(tree, &binding)?;
        if active
            .iter()
            .any(|c| c.name != binding && self.label(&c.name) == name)
        {
            return Err("cell name is already in use or reserved".into());
        }
        let mut next = self.clone();
        next.cells.iter_mut().find(|c| &c.id == id).unwrap().name = name;
        next.validate()?;
        next.save(tree)?;
        *self = next;
        Ok(())
    }
}
pub fn pin(tree: &Path, name: &Name) -> Fallible<Key> {
    let bytes = read(&tree.join(".pins").join(format!("{name}.pub")))?;
    Ok(std::str::from_utf8(&bytes)?.trim().parse()?)
}
pub fn observe(tree: &Path, vms: Option<&[Vm]>) -> Fallible<Vec<Observation>> {
    let (cells, _) = crate::tree::load(tree)?;
    let bindings = vms.map(crate::exe::cell_vms).transpose()?;
    let mut out = Vec::new();
    for cell in cells.iter() {
        let vm = if crate::host_binding::Binding::load(tree, &cell.name)?.is_some() {
            if bindings.as_ref().is_some_and(|b| b.contains_key(&cell.name)) {
                return Err("external cell also has an exe.dev VM binding".into());
            }
            None
        } else { match &bindings {
            Some(bindings) => {
                let vm = bindings
                    .get(&cell.name)
                    .ok_or("every migrated cell must have a uniquely tagged VM")?;
                Some(VmBinding {
                    name: vm.vm_name.clone(),
                    created_at: vm
                        .created_at
                        .clone()
                        .filter(|s| !s.is_empty())
                        .ok_or("VM creation identity is missing")?,
                })
            }
            None => None,
        }};
        out.push(Observation {
            cell: cell.clone(),
            parent: cells.parent(&cell.name).map(|p| p.name.clone()),
            key: pin(tree, &cell.name)?,
            vm,
        });
    }
    Ok(out)
}
impl Migration {
    fn hash(&self) -> Hash {
        Hash::of(&crate::canon::bytes(&(
            self.version,
            self.id,
            self.before,
            self.snapshot,
            &self.registry,
        )))
    }
    pub fn plan(tree: &Path, observed: &[Observation]) -> Fallible<Self> {
        let previous = Registry::load(tree)?;
        let mut registry = previous.clone();
        for observation in observed {
            let binding = &observation.cell.name;
            if let Some(existing) = registry.by_binding(binding) {
                if existing.key != observation.key || existing.vm != observation.vm {
                    return Err(
                        "existing identity binding changed; migration cannot adopt a replacement"
                            .into(),
                    );
                }
            } else {
                let id = loop {
                    let candidate = CellId::generate();
                    if !registry
                        .cells
                        .iter()
                        .any(|c| c.id == candidate || c.binding.as_str() == candidate.as_str())
                        && !observed
                            .iter()
                            .any(|o| o.cell.name.as_str() == candidate.as_str())
                    {
                        break candidate;
                    }
                };
                registry.cells.push(Identity {
                    id,
                    binding: binding.clone(),
                    name: binding.clone(),
                    key: observation.key,
                    vm: observation.vm.clone(),
                });
            }
        }
        registry.validate()?;
        let mut plan = Self {
            id: RequestId::generate(),
            version: 1,
            before: previous.revision(),
            snapshot: Hash::of(&crate::canon::bytes(&observed)),
            registry,
            approval: Hash::zero(),
        };
        plan.approval = plan.hash();
        Ok(plan)
    }
    pub fn apply(&self, tree: &Path, observed: &[Observation], approval: Hash) -> Fallible {
        if self.version != 1 || approval != self.approval || self.approval != self.hash() {
            return Err("identity migration approval mismatch".into());
        }
        self.registry.validate()?;
        if self.snapshot != Hash::of(&crate::canon::bytes(&observed)) {
            return Err("cell tree, pin or VM changed; review migration again".into());
        }
        let current = Registry::load(tree)?;
        if current == self.registry && tree.join(FILE).exists() {
            return Ok(());
        } // lost response / crash after atomic commit
        if current.revision() != self.before {
            return Err("identity registry changed; review migration again".into());
        }
        let dir = tree.join(".identity-migrations");
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let backup = dir.join(format!("{}.before.json", self.id));
        if backup.exists() {
            let saved: Registry = serde_json::from_slice(&read(&backup)?)?;
            if saved != current {
                return Err("identity migration backup mismatch".into());
            }
        } else {
            atomic(&backup, &crate::canon::bytes(&current))?;
        }
        atomic(
            &dir.join(format!("{}.plan.json", self.id)),
            &crate::canon::bytes(self),
        )?;
        self.registry.save(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Said, Secret, Statement};
    struct Fixture {
        root: std::path::PathBuf,
        secret: Secret,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("identity-{}", RequestId::generate()));
            fs::create_dir_all(root.join(".pins")).unwrap();
            fs::create_dir(root.join("parent")).unwrap();
            let secret = Secret::generate();
            for (path, name) in [("parent.json", "parent"), ("parent/child.json", "child")] {
                fs::write(
                    root.join(path),
                    format!(r#"{{"name":"{name}","grants":[]}}"#),
                )
                .unwrap();
                fs::write(
                    root.join(".pins").join(format!("{name}.pub")),
                    secret.public().to_string(),
                )
                .unwrap();
            }
            Self { root, secret }
        }
        fn observe(&self) -> Vec<Observation> {
            observe(&self.root, None).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn additive_migration_preserves_signed_bytes_sessions_tree_and_pins() {
        let f = Fixture::new();
        let statement = Statement::sign(
            &f.secret,
            "child".parse().unwrap(),
            1,
            Said::Fetch {
                url: "https://example.com".parse().unwrap(),
            },
        );
        let signed = crate::canon::bytes(&statement);
        let hash = statement.hash();
        fs::write(f.root.join("history.jsonl"), &signed).unwrap();
        // Non-declaration files belong outside the tree scanner.
        fs::rename(f.root.join("history.jsonl"), f.root.join(".history.jsonl")).unwrap();
        let before = f.observe();
        let files = [
            "parent.json",
            "parent/child.json",
            ".pins/parent.pub",
            ".pins/child.pub",
            ".history.jsonl",
        ];
        let bytes: Vec<_> = files
            .iter()
            .map(|p| fs::read(f.root.join(p)).unwrap())
            .collect();
        let plan = Migration::plan(&f.root, &before).unwrap();
        assert_ne!(plan.registry.cells[0].id, plan.registry.cells[1].id);
        assert!(!f.root.join(FILE).exists());
        plan.apply(&f.root, &before, plan.approval).unwrap();
        plan.apply(&f.root, &before, plan.approval).unwrap();
        let mut registry = Registry::load(&f.root).unwrap();
        let child = registry
            .by_binding(&"child".parse().unwrap())
            .unwrap()
            .id
            .clone();
        registry
            .rename(&f.root, &child, "renamed".parse().unwrap())
            .unwrap();
        assert_eq!(
            registry.by_route(&"child".parse().unwrap()).unwrap().id,
            child
        );
        assert_eq!(
            registry
                .by_route(&child.to_string().parse().unwrap())
                .unwrap()
                .name
                .as_str(),
            "renamed"
        );
        assert!(
            registry.by_route(&"renamed".parse().unwrap()).is_none(),
            "labels cannot become transport authority"
        );
        for (path, bytes) in files.iter().zip(bytes) {
            assert_eq!(fs::read(f.root.join(path)).unwrap(), bytes);
        }
        let parsed: Statement = serde_json::from_slice(&signed).unwrap();
        assert_eq!(parsed.hash(), hash);
        assert!(parsed.open(&f.secret.public()).is_ok());
        assert_eq!(
            crate::canon::bytes(&f.observe()),
            crate::canon::bytes(&before)
        );
        assert_eq!(
            fs::metadata(f.root.join(FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn uuid_parser_and_manual_declarations_cannot_shadow_reserved_routes() {
        for bad in [
            "parent",
            "../parent",
            "",
            "00000000-0000-4000-8000-00000000000Z",
        ] {
            assert!(bad.parse::<CellId>().is_err());
        }
        let f = Fixture::new();
        let observed = f.observe();
        let plan = Migration::plan(&f.root, &observed).unwrap();
        plan.apply(&f.root, &observed, plan.approval).unwrap();
        let registry = Registry::load(&f.root).unwrap();
        let reserved = registry.cells[0].id.to_string();
        fs::write(
            f.root.join(format!("{reserved}.json")),
            format!(r#"{{"name":"{reserved}","grants":[]}}"#),
        )
        .unwrap();
        assert!(registry
            .check_active(&f.root, &crate::tree::load(&f.root).unwrap().0)
            .is_err());
    }

    #[test]
    fn stale_snapshots_wrong_approvals_and_replacement_keys_are_refused() {
        let f = Fixture::new();
        let observed = f.observe();
        let plan = Migration::plan(&f.root, &observed).unwrap();
        assert!(plan.apply(&f.root, &observed, Hash::zero()).is_err());
        let mut changed = f.observe();
        changed[0]
            .cell
            .grants
            .insert(crate::Grant::Host("example.com".parse().unwrap()));
        assert!(plan.apply(&f.root, &changed, plan.approval).is_err());
        changed = f.observe();
        changed[0].vm = Some(VmBinding {
            name: "replacement".into(),
            created_at: "later".into(),
        });
        assert!(plan.apply(&f.root, &changed, plan.approval).is_err());
        changed = f.observe();
        changed[0].parent = None;
        assert!(plan.apply(&f.root, &changed, plan.approval).is_err());
        plan.apply(&f.root, &observed, plan.approval).unwrap();
        fs::write(
            f.root.join(".pins/child.pub"),
            Secret::generate().public().to_string(),
        )
        .unwrap();
        assert!(Registry::load(&f.root)
            .unwrap()
            .check_pin(&f.root, &"child".parse().unwrap())
            .is_err());
        assert!(Migration::plan(&f.root, &f.observe()).is_err());
    }
    #[test]
    fn display_names_are_reusable_but_routes_and_deleted_identities_stay_fixed() {
        let f = Fixture::new();
        let observed = f.observe();
        let plan = Migration::plan(&f.root, &observed).unwrap();
        plan.apply(&f.root, &observed, plan.approval).unwrap();
        let mut registry = Registry::load(&f.root).unwrap();
        let child = registry
            .by_binding(&"child".parse().unwrap())
            .unwrap()
            .id
            .clone();
        assert!(registry
            .rename(&f.root, &child, "parent".parse().unwrap())
            .is_err());
        let parent_id = registry
            .by_binding(&"parent".parse().unwrap())
            .unwrap()
            .id
            .clone();
        registry
            .rename(&f.root, &parent_id, "new-parent".parse().unwrap())
            .unwrap();
        registry
            .rename(&f.root, &child, "parent".parse().unwrap())
            .unwrap();
        let tree = crate::tree::load(&f.root).unwrap().0;
        registry.check_active(&f.root, &tree).unwrap();
        assert_eq!(
            registry.by_route(&"parent".parse().unwrap()).unwrap().id,
            parent_id
        );
        assert_eq!(
            registry
                .by_route(&child.to_string().parse().unwrap())
                .unwrap()
                .binding
                .as_str(),
            "child"
        );
        assert!(registry
            .resolve_cli(&tree, &"parent".parse().unwrap())
            .unwrap_err()
            .to_string()
            .contains("ambiguous; use a cell UUID"));
        assert_eq!(
            registry
                .resolve_cli(&tree, &"new-parent".parse().unwrap())
                .unwrap()
                .as_str(),
            "parent"
        );
        assert_eq!(
            registry
                .resolve_cli(&tree, &child.to_string().parse().unwrap())
                .unwrap()
                .as_str(),
            "child"
        );
        assert_eq!(
            registry
                .resolve_cli(&tree, &parent_id.to_string().parse().unwrap())
                .unwrap()
                .as_str(),
            "parent"
        );
        assert!(registry
            .rename(&f.root, &child, parent_id.to_string().parse().unwrap())
            .is_err());
        assert!(registry
            .rename(&f.root, &child, "new-parent".parse().unwrap())
            .is_err());
        fs::remove_file(f.root.join("parent/child.json")).unwrap();
        assert!(registry
            .rename(&f.root, &child, "other".parse().unwrap())
            .is_err());
        assert!(registry.by_binding(&"child".parse().unwrap()).is_some());
        let mut invalid = registry.clone();
        invalid.version = 99;
        assert!(invalid.save(&f.root).is_err());
        fs::set_permissions(f.root.join(FILE), fs::Permissions::from_mode(0o666)).unwrap();
        assert!(Registry::load(&f.root).is_err());
    }
    #[test]
    fn retry_after_registry_commit_keeps_the_original_mapping_and_backup() {
        let f = Fixture::new();
        let observed = f.observe();
        let plan = Migration::plan(&f.root, &observed).unwrap();
        plan.apply(&f.root, &observed, plan.approval).unwrap();
        let backup = f
            .root
            .join(".identity-migrations")
            .join(format!("{}.before.json", plan.id));
        let saved = fs::read(&backup).unwrap();
        plan.apply(&f.root, &observed, plan.approval).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), saved);
        let next = Migration::plan(&f.root, &observed).unwrap();
        assert_eq!(next.registry, plan.registry);
        let mut corrupt = plan.clone();
        corrupt.registry.cells[0].id = CellId::generate();
        assert!(corrupt.apply(&f.root, &observed, plan.approval).is_err());
    }
}
