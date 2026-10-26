//! Private supervisor bindings for externally owned hosts. These never implement
//! Edge: registration cannot provision, adopt, or delete an exe.dev VM.
use crate::ssh_host::Transport;
use crate::{apply::Fallible, canon, identity, ssh_host, Cell, Hash, Name, Tree};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub host: ssh_host::Config,
    pub ssh_trust: Hash,
    pub pin: ssh_host::Pin,
}
impl Binding {
    pub fn new(host: ssh_host::Config, pin: ssh_host::Pin) -> Fallible<Self> {
        let ssh_trust = host.trust()?;
        let value = Self {
            version: 1,
            host,
            ssh_trust,
            pin,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Fallible {
        if self.version != 1 {
            return Err("unsupported external host binding".into());
        }
        self.host.validate()?;
        if self.pin.machine_id.len() != 32
            || !self.pin.machine_id.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("invalid external host machine identity".into());
        }
        if self.host.trust()? != self.ssh_trust {
            return Err("external host SSH trust changed".into());
        }
        Ok(())
    }
    pub fn load(tree: &Path, name: &Name) -> Fallible<Option<Self>> {
        let path = tree.join(".hosts").join(format!("{name}.json"));
        let meta = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
            return Err("external host binding must be a private regular file".into());
        }
        let value: Self = serde_json::from_slice(&fs::read(path)?)?;
        value.validate()?;
        let registry = identity::Registry::load(tree)?;
        let record = registry
            .by_binding(name)
            .ok_or("external host has no registered cell identity")?;
        if value.pin.cell != *name
            || value.pin.key != record.key
            || record.vm.is_some()
            || identity::pin(tree, name)? != value.pin.key
        {
            return Err("external host binding conflicts with the registered cell identity".into());
        }
        Ok(Some(value))
    }
    pub fn observe(&self, cell: &Cell) -> Fallible<ssh_host::Observation> {
        self.validate()?;
        let observed = ssh_host::Ssh.observe(&self.host, self.ssh_trust)?;
        self.pin.check(&observed, cell)?;
        if observed.cell.as_ref().map(|c| &c.name) != Some(&cell.name)
            || observed.runtime.as_ref() != Some(&self.host.runtime)
        {
            return Err("external host runtime or cell binding changed".into());
        }
        Ok(observed)
    }
    pub fn plan(&self, cell: Cell, binary: &[u8]) -> Fallible<ssh_host::Plan> {
        self.host.runtime.validate_cell(&cell)?;
        let observed = self.observe(&cell)?;
        ssh_host::Plan::new(
            self.host.clone(),
            self.ssh_trust,
            cell,
            Hash::of(binary),
            observed,
            Some(self.pin.clone()),
        )
    }
    pub fn apply(
        &self,
        plan: &ssh_host::Plan,
        approval: Hash,
        binary: &[u8],
        state: &Path,
    ) -> Fallible<usize> {
        self.validate()?;
        if plan.host != self.host
            || plan.ssh_trust != self.ssh_trust
            || plan.pin.as_ref() != Some(&self.pin)
        {
            return Err("external host binding changed after review".into());
        }
        fs::create_dir_all(state)?;
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        match ssh_host::Pin::load(state)? {
            Some(pin) if pin != self.pin => {
                return Err("external host deployment receipt changed".into())
            }
            None => identity::atomic(&state.join("host-pin.json"), &canon::bytes(&self.pin))?,
            _ => {}
        }
        ssh_host::apply(&ssh_host::Ssh, plan, approval, binary, state)?;
        Ok(plan.actions.len())
    }
    /// The caller retains the snapshot until the SSH child has exited. The
    /// checks and command run on the same authenticated connection, before stdin
    /// reaches the relay/health helper. No cached observation authorizes a call.
    pub(crate) fn command(
        &self,
        command: &str,
    ) -> Fallible<(std::process::Command, ssh_host::TrustSnapshot)> {
        self.validate()?;
        let snapshot = ssh_host::TrustSnapshot::new(&self.host, self.ssh_trust)?;
        let check = format!("test \"$(cat /etc/machine-id)\" = '{}' && test \"$(cat /etc/cell/key.pub)\" = '{}' && test \"$(sudo -n python3 -c 'import json; print(json.load(open(\"/etc/cell/cell.json\"))[\"name\"])')\" = '{}' && {command}", self.pin.machine_id, self.pin.key, self.pin.cell);
        Ok((snapshot.config.command(&check)?, snapshot))
    }
}

pub fn validate_tree(root: &Path, tree: &Tree) -> Fallible {
    for cell in tree.iter() {
        if let Some(binding) = Binding::load(root, &cell.name)? {
            binding.host.runtime.validate_cell(cell)?;
            if tree.parent(&cell.name).is_some() || tree.children(&cell.name).next().is_some() {
                return Err(
                    "external hosts currently require root cells without child doors".into(),
                );
            }
        }
    }
    Ok(())
}

pub fn revision(root: &Path) -> Fallible<Hash> {
    let (tree, _) = crate::tree::load(root)?;
    let bindings = tree
        .iter()
        .map(|c| Ok((c.name.clone(), Binding::load(root, &c.name)?)))
        .collect::<Fallible<Vec<_>>>()?;
    Ok(if bindings.iter().all(|(_, b)| b.is_none()) {
        Hash::ZERO
    } else {
        Hash::of(&canon::bytes(&bindings))
    })
}

#[derive(Debug, Serialize)]
pub struct Registration {
    pub binding: Binding,
    pub cell: Cell,
    pub before: Hash,
    pub observed: ssh_host::Observation,
}
impl Registration {
    pub fn hash(&self) -> Hash {
        Hash::of(&canon::bytes(self))
    }
}

fn before(root: &Path) -> Fallible<Hash> {
    let (tree, paths) = crate::tree::load(root)?;
    let registry = identity::Registry::load(root)?;
    registry.check_active(root, &tree)?;
    validate_tree(root, &tree)?;
    if tree.iter().any(|c| registry.by_binding(&c.name).is_none()) {
        return Err("register identities for existing cells before adding an external host".into());
    }
    Ok(Hash::of(&canon::bytes(&(
        tree.iter()
            .map(|c| (c, tree.parent(&c.name).map(|p| &p.name)))
            .collect::<Vec<_>>(),
        paths,
        registry,
        revision(root)?,
    ))))
}

pub fn registration(root: &Path, cell: Cell, binding: Binding) -> Fallible<Registration> {
    let before = before(root)?;
    let (tree, _) = crate::tree::load(root)?;
    let registry = identity::Registry::load(root)?;
    if tree.get(&cell.name).is_some()
        || registry.cells.iter().any(|c| {
            c.binding == cell.name || c.name == cell.name || c.id.as_str() == cell.name.as_str()
        })
    {
        return Err(
            "registration only adds a new cell; existing cells require an explicit migration"
                .into(),
        );
    }
    for path in [
        root.join(format!("{}.json", cell.name)),
        root.join(".pins").join(format!("{}.pub", cell.name)),
        root.join(".hosts").join(format!("{}.json", cell.name)),
    ] {
        if fs::symlink_metadata(path).is_ok() {
            return Err("registration destination already exists".into());
        }
    }
    binding.host.runtime.validate_cell(&cell)?;
    let observed = binding.observe(&cell)?;
    if !ssh_host::verified(
        &observed,
        &cell,
        &binding.host.runtime,
        observed.binary.ok_or("host binary missing")?,
    )? {
        return Err(
            "registration requires a passing signed check for the declared cell and runtime".into(),
        );
    }
    Ok(Registration {
        binding,
        cell,
        before,
        observed,
    })
}

/// Call with the operator state lock held. The declaration is installed last;
/// a partial registration therefore cannot become a routable cell. Private
/// files are retained for explicit repair if an I/O failure interrupts commit.
pub fn register(root: &Path, plan: Registration, approval: Hash) -> Fallible<crate::CellId> {
    if plan.hash() != approval {
        return Err("host registration approval mismatch".into());
    }
    let fresh = registration(root, plan.cell.clone(), plan.binding.clone())?;
    if fresh.hash() != approval {
        return Err("host or supervisor changed; review registration again".into());
    }
    let mut registry = identity::Registry::load(root)?;
    let id = crate::CellId::generate();
    registry.cells.push(identity::Identity {
        id: id.clone(),
        binding: plan.cell.name.clone(),
        name: plan.cell.name.clone(),
        key: plan.binding.pin.key,
        vm: None,
    });
    registry.validate()?;
    for dir in [root.join(".pins"), root.join(".hosts")] {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    fn create(path: &Path, bytes: &[u8]) -> Fallible {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
    create(
        &root.join(".pins").join(format!("{}.pub", plan.cell.name)),
        plan.binding.pin.key.to_string().as_bytes(),
    )?;
    create(
        &root.join(".hosts").join(format!("{}.json", plan.cell.name)),
        &canon::bytes(&plan.binding),
    )?;
    registry.save(root)?;
    create(
        &root.join(format!("{}.json", plan.cell.name)),
        &plan.cell.canonical(),
    )?;
    fs::File::open(root)?.sync_all()?;
    Ok(id)
}
