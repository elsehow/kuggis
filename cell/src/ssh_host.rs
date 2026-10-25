//! Bootstrap an existing, dedicated Ubuntu host without a cloud-provider API.
//! This intentionally does not implement exe.dev's Edge: external inventory
//! must never be mistaken for provider-managed VMs or integrations.
use crate::{
    apply::Fallible, canon, host_runtime::Runtime, Cell, Hash, Host, Key, Said, Statement,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    net::IpAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const HELPER: &str = include_str!("ssh_host.py");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: PathBuf,
    pub known_hosts: PathBuf,
    pub runtime: Runtime,
}

impl Config {
    pub fn load(path: &Path) -> Fallible<Self> {
        let config: Self = serde_json::from_slice(&fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Fallible {
        if self.hostname.parse::<IpAddr>().is_err() {
            self.hostname.parse::<Host>()?;
        }
        if self.hostname.starts_with('-') || self.hostname.chars().any(char::is_whitespace) {
            return Err("invalid SSH hostname".into());
        }
        if self.user.is_empty()
            || self.user.len() > 64
            || !self
                .user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            || self.user.starts_with('-')
        {
            return Err("invalid SSH user".into());
        }
        if self.port == 0 {
            return Err("SSH port must be nonzero".into());
        }
        for path in [&self.identity_file, &self.known_hosts] {
            if !path.is_absolute()
                || path
                    .to_str()
                    .is_none_or(|p| p.chars().any(char::is_control) || p.contains('%'))
            {
                return Err("SSH key and known-hosts paths must be absolute UTF-8 paths without control characters or percent expansions".into());
            }
        }
        self.runtime.validate()?;
        if self.runtime.is_exe() {
            return Err("host commands require the ssh runtime".into());
        }
        Ok(())
    }

    /// Ignore ambient SSH configuration, proxy commands, forwarding and TOFU.
    /// The known-hosts contents, not just their path, are part of the plan.
    fn command(&self, remote: &str) -> Fallible<Command> {
        self.validate()?;
        let mut command = Command::new("ssh");
        command.args([
            "-F",
            "/dev/null",
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
        ]);
        let known = self
            .known_hosts
            .to_str()
            .expect("validated")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile=\"{known}\""))
            .arg("-i")
            .arg(&self.identity_file)
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-l")
            .arg(&self.user)
            .arg(&self.hostname)
            .arg(remote);
        Ok(command)
    }

    fn trust(&self) -> Fallible<Hash> {
        let bytes = fs::read(&self.known_hosts)?;
        if bytes.is_empty() {
            return Err("known_hosts is empty; pin the server's SSH host key first".into());
        }
        Ok(Hash::of(&bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub machine_id: String,
    pub os: String,
    pub version: String,
    pub init: String,
    pub arch: String,
    pub dns: BTreeSet<IpAddr>,
    pub memory_total: u64,
    pub cell: Option<Cell>,
    pub key: Option<Key>,
    pub runtime: Option<Runtime>,
    pub binary: Option<Hash>,
    pub checked: Option<Statement>,
}

/// Local receipt binds a host incarnation to its cell signing identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub machine_id: String,
    pub cell: crate::Name,
    pub key: Key,
}

impl Pin {
    pub fn load(state: &Path) -> Fallible<Option<Self>> {
        match fs::read(state.join("host-pin.json")) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn check(&self, before: &Observation, cell: &Cell) -> Fallible {
        if self.machine_id != before.machine_id
            || self.cell != cell.name
            || Some(self.key) != before.key
        {
            return Err("host or cell identity differs from the local pin; automatic adoption and migration are refused".into());
        }
        Ok(())
    }

    fn save(&self, state: &Path) -> Fallible {
        fs::create_dir_all(state)?;
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        let path = state.join("host-pin.json");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&canon::bytes(self))?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if Self::load(state)?.as_ref() != Some(self) {
                    return Err("host identity pin changed".into());
                }
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Setup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub host: Config,
    pub ssh_trust: Hash,
    pub cell: Cell,
    pub binary: Hash,
    pub before: Observation,
    pub pin: Option<Pin>,
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn new(
        host: Config,
        ssh_trust: Hash,
        cell: Cell,
        binary: Hash,
        before: Observation,
        pin: Option<Pin>,
    ) -> Fallible<Self> {
        host.validate()?;
        host.runtime.validate_cell(&cell)?;
        if before.os != "ubuntu"
            || before.version != "24.04"
            || before.arch != "x86_64"
            || before.init != "systemd"
        {
            return Err("SSH bootstrap currently requires an x86_64 Ubuntu 24.04 host".into());
        }
        if before.machine_id.len() != 32
            || !before.machine_id.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("host has no valid machine identity".into());
        }
        if before.dns != host.runtime.dns() {
            return Err("configured DNS addresses differ from the host's /etc/resolv.conf".into());
        }
        if host.runtime.memory_max() >= before.memory_total {
            return Err("guest memory limit must leave memory available to the host".into());
        }
        if let Some(pin) = &pin {
            pin.check(&before, &cell)?;
        } else if before.key.is_some()
            || before.cell.is_some()
            || before.runtime.is_some()
            || before.binary.is_some()
        {
            return Err("host is already initialized; a matching host identity receipt is required (migration is not implemented)".into());
        }
        if before.cell.as_ref().is_some_and(|c| c.name != cell.name) {
            return Err("host belongs to another cell".into());
        }
        if before.runtime.as_ref().is_some_and(Runtime::is_exe) {
            return Err("exe.dev cells require an explicit migration".into());
        }
        let proved = verified(&before, &cell, &host.runtime, binary)?;
        Ok(Self {
            version: 1,
            host,
            ssh_trust,
            cell,
            binary,
            before,
            pin,
            actions: if proved { vec![] } else { vec![Action::Setup] },
        })
    }

    pub fn hash(&self) -> Hash {
        Hash::of(&canon::bytes(self))
    }
}

fn verified(
    observed: &Observation,
    cell: &Cell,
    runtime: &Runtime,
    binary: Hash,
) -> Fallible<bool> {
    let (Some(statement), Some(key)) = (&observed.checked, &observed.key) else {
        return Ok(false);
    };
    if statement.cell != cell.name {
        return Err("signed check belongs to another cell".into());
    }
    let Said::Checked {
        cell: checked_cell,
        ok,
        binary: checked_binary,
        report,
        ..
    } = statement.open(key)?
    else {
        return Err("host returned a statement that is not a boundary check".into());
    };
    Ok(*ok
        && (!cell.grants.iter().any(|g| matches!(g, crate::Grant::Code { .. }))
            || report.lines().any(|line| line == crate::environment::SCOPED_GIT_CHECK))
        && *checked_cell == cell.hash()
        && *checked_binary == binary
        && observed.cell.as_ref() == Some(cell)
        && observed.runtime.as_ref() == Some(runtime)
        && observed.binary == Some(binary)
        && report.contains(&format!(
            "host runtime {}",
            Hash::of(&canon::bytes(runtime))
        )))
}

fn python(action: &str) -> String {
    format!(
        "sudo -n python3 -c '{}' {action}",
        HELPER.replace('\'', "'\\''")
    )
}

pub trait Transport {
    fn observe(&self, config: &Config, trust: Hash) -> Fallible<Observation>;
    fn setup(&self, plan: &Plan, binary: &[u8]) -> Fallible;
}

pub struct Ssh;

// Freeze reviewed SSH trust for the lifetime of each connection. Rechecking a
// mutable known_hosts path before spawning ssh alone leaves a TOCTOU window.
struct TrustSnapshot {
    dir: PathBuf,
    config: Config,
}
impl TrustSnapshot {
    fn new(config: &Config, expected: Hash) -> Fallible<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let bytes = fs::read(&config.known_hosts)?;
        if Hash::of(&bytes) != expected {
            return Err("SSH trust changed after review".into());
        }
        let dir = std::env::temp_dir().join(format!("cell-ssh-{}", crate::RequestId::generate()));
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let mut snapshot = Self {
            dir,
            config: config.clone(),
        };
        snapshot.config.known_hosts = snapshot.dir.join("known_hosts");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&snapshot.config.known_hosts)?;
        file.write_all(&bytes)?;
        Ok(snapshot)
    }
}
impl Drop for TrustSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn inspect(config: &Config) -> Fallible<Observation> {
    config.validate()?;
    Ssh.observe(config, config.trust()?)
}

impl Transport for Ssh {
    fn observe(&self, config: &Config, trust: Hash) -> Fallible<Observation> {
        let snapshot = TrustSnapshot::new(config, trust)?;
        let output = snapshot
            .config
            .command(&python("inspect"))?
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "SSH host inspection failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn setup(&self, plan: &Plan, binary: &[u8]) -> Fallible {
        let header = serde_json::json!({"before":plan.before,"cell":plan.cell,"runtime":plan.host.runtime,"binary":plan.binary,"size":binary.len()});
        let snapshot = TrustSnapshot::new(&plan.host, plan.ssh_trust)?;
        let mut child = snapshot
            .config
            .command(&python("apply"))?
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut input = child.stdin.take().expect("piped");
        let sent = (|| -> std::io::Result<()> {
            input.write_all(&canon::bytes(&header))?;
            input.write_all(b"\n")?;
            input.write_all(binary)
        })();
        drop(input);
        let status = child.wait()?;
        sent?;
        if !status.success() {
            return Err(format!(
                "SSH setup exited with {status}; inspect the host before retrying"
            )
            .into());
        }
        Ok(())
    }
}

pub fn linux_binary(path: &Path) -> Fallible<Vec<u8>> {
    let binary = fs::read(path)?;
    if binary.len() < 64
        || binary.len() > 256 * 1024 * 1024
        || &binary[..6] != b"\x7fELF\x02\x01"
        || binary[18..20] != [62, 0]
    {
        return Err("--bin must be a 64-bit little-endian x86 Linux ELF binary (build x86_64-unknown-linux-musl)".into());
    }
    Ok(binary)
}

pub fn plan(
    transport: &dyn Transport,
    host: Config,
    cell: Cell,
    binary: &[u8],
    state: &Path,
) -> Fallible<Plan> {
    // Refuse unsupported authority before any SSH connection.
    host.validate()?;
    host.runtime.validate_cell(&cell)?;
    let trust = host.trust()?;
    let before = transport.observe(&host, trust)?;
    if trust != host.trust()? {
        return Err("SSH known-hosts file changed during inspection".into());
    }
    Plan::new(
        host,
        trust,
        cell,
        Hash::of(binary),
        before,
        Pin::load(state)?,
    )
}

pub fn apply(
    transport: &dyn Transport,
    reviewed: &Plan,
    approval: Hash,
    binary: &[u8],
    state: &Path,
) -> Fallible<Observation> {
    if approval != reviewed.hash()
        || Hash::of(binary) != reviewed.binary
        || reviewed.ssh_trust != reviewed.host.trust()?
    {
        return Err("host plan, binary or SSH trust changed; review a new plan".into());
    }
    fs::create_dir_all(state)?;
    fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(state.join("host.lock"))?;
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another host operation is using this state directory".into());
    }
    let fresh = plan(
        transport,
        reviewed.host.clone(),
        reviewed.cell.clone(),
        binary,
        state,
    )?;
    if fresh != *reviewed {
        return Err("host changed after approval; review a new plan".into());
    }
    if fresh.actions.is_empty() {
        return Ok(fresh.before);
    }
    // Do not retry mutations after an SSH disconnect. Inspect the result and
    // pin a newly created identity even if setup failed partway through.
    let result = transport.setup(&fresh, binary);
    let after = transport.observe(&fresh.host, fresh.ssh_trust)?;
    if after.machine_id != fresh.before.machine_id {
        return Err("host identity changed during setup; inspect before retrying".into());
    }
    if after.cell.is_none() && fresh.before.cell.is_none() {
        result?;
        return Err("setup returned without installing a cell declaration".into());
    }
    if after.cell.as_ref().map(|c| &c.name) != Some(&fresh.cell.name) {
        return Err("cell identity changed during setup; inspect before retrying".into());
    }
    if let Some(key) = after.key {
        let pin = Pin {
            machine_id: after.machine_id.clone(),
            cell: fresh.cell.name.clone(),
            key,
        };
        if let Some(previous) = &fresh.pin {
            previous.check(&after, &fresh.cell)?;
        }
        pin.save(state)?;
    }
    result?;
    if !verified(&after, &fresh.cell, &fresh.host.runtime, fresh.binary)? {
        return Err(
            "setup did not produce a passing signed check for the reviewed deployment".into(),
        );
    }
    fs::write(state.join("last-applied.json"), canon::bytes(&fresh))?;
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;
    use std::cell::{Cell as Counter, RefCell};

    struct Fixture {
        dir: PathBuf,
        host: Config,
        cell: Cell,
        binary: Vec<u8>,
        edge: Fake,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ssh-host-test-{}", crate::RequestId::generate()));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("known_hosts"), "fixture pinned host key\n").unwrap();
            let host = Config {
                hostname: "worker.example".into(),
                user: "operator".into(),
                port: 2222,
                identity_file: dir.join("identity"),
                known_hosts: dir.join("known_hosts"),
                runtime: Runtime::Ssh {
                    dns: ["127.0.0.53".parse().unwrap()].into(),
                    memory_max: 8 * 1024 * 1024 * 1024,
                },
            };
            let cell = Cell::new(
                "worker".parse().unwrap(),
                [crate::Grant::Provider(crate::Provider::OpenAi)],
            );
            let before = Observation {
                machine_id: "0123456789abcdef0123456789abcdef".into(),
                os: "ubuntu".into(),
                version: "24.04".into(),
                init: "systemd".into(),
                arch: "x86_64".into(),
                dns: host.runtime.dns(),
                memory_total: 16 * 1024 * 1024 * 1024,
                cell: None,
                key: None,
                runtime: None,
                binary: None,
                checked: None,
            };
            let edge = Fake {
                observed: RefCell::new(before),
                secret: Secret::generate(),
                setups: Counter::new(0),
                fail: Counter::new(false),
            };
            Self {
                dir,
                host,
                cell,
                binary: b"fixture binary".to_vec(),
                edge,
            }
        }
        fn state(&self) -> PathBuf {
            self.dir.join("state")
        }
        fn plan(&self) -> Plan {
            plan(
                &self.edge,
                self.host.clone(),
                self.cell.clone(),
                &self.binary,
                &self.state(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    struct Fake {
        observed: RefCell<Observation>,
        secret: Secret,
        setups: Counter<usize>,
        fail: Counter<bool>,
    }
    impl Transport for Fake {
        fn observe(&self, _: &Config, _: Hash) -> Fallible<Observation> {
            Ok(self.observed.borrow().clone())
        }
        fn setup(&self, plan: &Plan, _: &[u8]) -> Fallible {
            self.setups.set(self.setups.get() + 1);
            let mut observed = self.observed.borrow_mut();
            observed.cell = Some(plan.cell.clone());
            observed.key = Some(self.secret.public());
            observed.runtime = Some(plan.host.runtime.clone());
            observed.binary = Some(plan.binary);
            observed.checked = Some(Statement::sign(
                &self.secret,
                plan.cell.name.clone(),
                1,
                Said::Checked {
                    cell: plan.cell.hash(),
                    ok: !self.fail.get(),
                    binary: plan.binary,
                    signed: BTreeSet::new(),
                    report: format!(
                        "host runtime {}",
                        Hash::of(&canon::bytes(&plan.host.runtime))
                    ),
                },
            ));
            if self.fail.get() {
                return Err("injected setup failure".into());
            }
            Ok(())
        }
    }

    #[test]
    fn applies_pins_verifies_and_reapply_is_a_noop() {
        let f = Fixture::new();
        let reviewed = f.plan();
        assert_eq!(reviewed.actions, [Action::Setup]);
        apply(&f.edge, &reviewed, reviewed.hash(), &f.binary, &f.state()).unwrap();
        let pin = Pin::load(&f.state()).unwrap().unwrap();
        assert_eq!(pin.key, f.edge.secret.public());
        let reviewed = f.plan();
        assert!(reviewed.actions.is_empty());
        apply(&f.edge, &reviewed, reviewed.hash(), &f.binary, &f.state()).unwrap();
        assert_eq!(f.edge.setups.get(), 1);
    }

    #[test]
    fn target_binary_trust_and_dns_drift_stop_before_mutation() {
        for drift in ["machine", "binary", "trust", "dns", "memory", "approval"] {
            let f = Fixture::new();
            let reviewed = f.plan();
            let mut binary = f.binary.clone();
            let mut approval = reviewed.hash();
            match drift {
                "machine" => f.edge.observed.borrow_mut().machine_id = "a".repeat(32),
                "binary" => binary.push(0),
                "trust" => fs::write(&f.host.known_hosts, "different host key").unwrap(),
                "dns" => f.edge.observed.borrow_mut().dns.clear(),
                "memory" => f.edge.observed.borrow_mut().memory_total = 1024,
                "approval" => approval = Hash::zero(),
                _ => unreachable!(),
            }
            assert!(
                apply(&f.edge, &reviewed, approval, &binary, &f.state()).is_err(),
                "{drift}"
            );
            assert_eq!(f.edge.setups.get(), 0, "{drift}");
        }
    }

    #[test]
    fn partial_failure_pins_identity_and_can_be_replanned() {
        let f = Fixture::new();
        let reviewed = f.plan();
        f.edge.fail.set(true);
        assert!(apply(&f.edge, &reviewed, reviewed.hash(), &f.binary, &f.state()).is_err());
        assert!(Pin::load(&f.state()).unwrap().is_some());
        assert!(!f.state().join("last-applied.json").exists());
        let retry = f.plan();
        assert_eq!(retry.actions, [Action::Setup]);
        f.edge.fail.set(false);
        apply(&f.edge, &retry, retry.hash(), &f.binary, &f.state()).unwrap();
        f.edge.observed.borrow_mut().key = Some(Secret::generate().public());
        assert!(plan(
            &f.edge,
            f.host.clone(),
            f.cell.clone(),
            &f.binary,
            &f.state()
        )
        .is_err());
    }

    #[test]
    fn no_implicit_adoption_or_forged_checks() {
        let f = Fixture::new();
        let reviewed = f.plan();
        f.edge.setup(&reviewed, &f.binary).unwrap();
        assert!(plan(
            &f.edge,
            f.host.clone(),
            f.cell.clone(),
            &f.binary,
            &f.state()
        )
        .is_err());
        Pin {
            machine_id: reviewed.before.machine_id,
            cell: f.cell.name.clone(),
            key: f.edge.secret.public(),
        }
        .save(&f.state())
        .unwrap();
        let observed = f.edge.observed.borrow().clone();
        let forged = Statement::sign(
            &Secret::generate(),
            f.cell.name.clone(),
            2,
            Said::Checked {
                cell: f.cell.hash(),
                ok: true,
                report: "forged".into(),
                binary: observed.binary.unwrap(),
                signed: BTreeSet::new(),
            },
        );
        f.edge.observed.borrow_mut().checked = Some(forged);
        assert!(plan(
            &f.edge,
            f.host.clone(),
            f.cell.clone(),
            &f.binary,
            &f.state()
        )
        .is_err());
    }

    #[test]
    fn ssh_arguments_pin_trust_and_do_not_interpret_config_as_shell() {
        let f = Fixture::new();
        let command = f.host.command("sudo -n /usr/local/bin/cell relay").unwrap();
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|p| p == ["-F", "/dev/null"]));
        assert!(args.contains(&"StrictHostKeyChecking=yes"));
        assert!(args.contains(&"ForwardAgent=no"));
        assert_eq!(args.last(), Some(&"sudo -n /usr/local/bin/cell relay"));
        for hostname in [
            "-oProxyCommand=evil",
            "worker;touch /tmp/pwn",
            "user@host",
            "worker\nother",
        ] {
            let mut host = f.host.clone();
            host.hostname = hostname.into();
            assert!(host.command("true").is_err());
        }
        let mut host = f.host.clone();
        host.identity_file = "/tmp/%h/key".into();
        assert!(host.validate().is_err());
    }

    #[test]
    fn ssh_trust_snapshot_survives_source_file_changes() {
        let f = Fixture::new();
        let hash = f.host.trust().unwrap();
        let snapshot = TrustSnapshot::new(&f.host, hash).unwrap();
        fs::write(&f.host.known_hosts, "replacement").unwrap();
        assert_eq!(snapshot.config.trust().unwrap(), hash);
        assert!(TrustSnapshot::new(&f.host, hash).is_err());
    }

    #[test]
    fn bootstrap_protocol_is_checked_before_privileged_mutations() {
        let status = Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ssh_host.py"))
            .status()
            .unwrap();
        assert!(status.success());
    }
}
