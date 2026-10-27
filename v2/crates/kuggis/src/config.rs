//! Deployment configuration is authoritative; guest state never grants access.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::IpAddr,
    num::{NonZeroU16, NonZeroU64},
};

macro_rules! checked_string {
    ($name:ident, $check:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(value: String) -> Result<Self> {
                if $check(&value) {
                    Ok(Self(value))
                } else {
                    // Do not echo potentially sensitive input into diagnostics.
                    Err(Error(concat!("invalid ", stringify!($name)).into()))
                }
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

fn name(value: &str) -> bool {
    (1..=48).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.ends_with('-')
}

fn ssh_target(value: &str) -> bool {
    // Use an SSH config alias for custom ports, usernames or IPv6 addresses.
    (1..=253).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn host(value: &str) -> bool {
    if let Ok(ip) = value.parse::<IpAddr>() {
        return value == ip.to_string();
    }
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn absolute_path(value: &str) -> bool {
    value.strip_prefix('/').is_some_and(relative_path)
}

fn package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+.-".contains(&b))
}

checked_string!(Name, name);
checked_string!(SshTarget, ssh_target);
checked_string!(Host, host);
checked_string!(GuestPath, relative_path);
checked_string!(HostPath, absolute_path);
checked_string!(Package, package);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Destination {
    #[default]
    Public,
    /// Deliberate private/link-local access, or stable address pins. DNS must resolve only
    /// to this set. An installer must also exclude its own management endpoints.
    Pinned { addresses: BTreeSet<IpAddr> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub host: Host,
    pub port: NonZeroU16,
    #[serde(default)]
    pub destination: Destination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    /// Already provisioned on the trusted host. Never opened by the CLI.
    pub source: HostPath,
    /// Relative to the guest agent's home, not to the host rootfs.
    pub target: GuestPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub name: Name,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub credentials: Vec<Credential>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateWeb {
    pub name: Name,
    pub guest_port: NonZeroU16,
    /// Always loopback on the host. An authenticated ingress must reach it.
    pub host_port: NonZeroU16,
}

/// Hosting convenience, separate from guest authority. The control connection
/// remains on the operator machine and is never passed into the guest.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Ingress {
    #[default]
    Tunnel,
    Exe {
        vm: Name,
        web: Name,
    },
}

/// Can only be constructed by strict deserialization followed by validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "Declaration", into = "Declaration")]
pub struct Workspace {
    name: Name,
    memory_bytes: NonZeroU64,
    ssh: SshTarget,
    ssh_config: Option<HostPath>,
    launch: Vec<String>,
    packages: Vec<Package>,
    resources: Vec<Resource>,
    private_web: Vec<PrivateWeb>,
    ingress: Ingress,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    version: u8,
    name: Name,
    #[serde(default = "default_memory")]
    memory_bytes: NonZeroU64,
    ssh: SshTarget,
    #[serde(default)]
    ssh_config: Option<HostPath>,
    launch: Vec<String>,
    #[serde(default)]
    packages: Vec<Package>,
    #[serde(default)]
    resources: Vec<Resource>,
    #[serde(default)]
    private_web: Vec<PrivateWeb>,
    #[serde(default)]
    ingress: Ingress,
}

fn default_memory() -> NonZeroU64 {
    NonZeroU64::new(4 * 1024 * 1024 * 1024).unwrap()
}

impl TryFrom<Declaration> for Workspace {
    type Error = Error;

    fn try_from(raw: Declaration) -> Result<Self> {
        if raw.version != 2 {
            return Err(Error("expected configuration version 2".into()));
        }
        if raw.memory_bytes.get() > i64::MAX as u64 {
            return Err(Error(
                "memory_bytes exceeds the supported byte count".into(),
            ));
        }
        if raw.launch.is_empty()
            || raw.launch[0].is_empty()
            || raw.launch[0].starts_with('-')
            || raw.launch.iter().any(|arg| arg.contains('\0'))
        {
            return Err(Error(
                "launch must be a nonempty argv without NUL bytes".into(),
            ));
        }
        let mut names = BTreeSet::new();
        let mut endpoints = BTreeSet::new();
        let mut targets: BTreeSet<&str> = BTreeSet::new();
        for resource in &raw.resources {
            if !names.insert(&resource.name) {
                return Err(Error("duplicate resource name".into()));
            }
            for endpoint in &resource.endpoints {
                if !endpoints.insert((&endpoint.host, endpoint.port)) {
                    return Err(Error(
                        "duplicate endpoint; declare each host/port once".into(),
                    ));
                }
                if let Destination::Pinned { addresses } = &endpoint.destination {
                    if addresses.is_empty()
                        || !addresses.iter().copied().all(crate::network::valid_pin)
                    {
                        return Err(Error("address pins must be nonempty unicast destinations, excluding loopback and scoped IPv6 addresses".into()));
                    }
                    if addresses
                        .iter()
                        .any(|ip| crate::deploy::Boundary::is_internal(*ip))
                    {
                        return Err(Error(
                            "the workspace boundary cannot be an outbound resource".into(),
                        ));
                    }
                }
            }
            for credential in &resource.credentials {
                let target = credential.target.as_str();
                if targets
                    .iter()
                    .any(|old| path_contains(old, target) || path_contains(target, old))
                {
                    return Err(Error("credential destinations overlap".into()));
                }
                targets.insert(target);
            }
        }
        let mut names = BTreeSet::new();
        let mut host_ports = BTreeSet::new();
        for web in &raw.private_web {
            if !names.insert(&web.name) || !host_ports.insert(web.host_port) {
                return Err(Error(
                    "private web names and host ports must be unique".into(),
                ));
            }
            // Host management ports cannot be shadowed by a workspace relay.
            if web.host_port.get() < 1024 || web.host_port.get() == crate::deploy::PROXY_PORT {
                return Err(Error(
                    "private web ports must be at least 1024 and must not shadow the proxy".into(),
                ));
            }
        }
        if let Ingress::Exe { vm, web } = &raw.ingress {
            if raw.ssh.as_str() != format!("{}.exe.xyz", vm.as_str()) {
                return Err(Error(
                    "exe ingress must name the same VM as the SSH hostname".into(),
                ));
            }
            if !raw
                .private_web
                .iter()
                .any(|publication| &publication.name == web)
            {
                return Err(Error(
                    "exe ingress must reference a declared private web port".into(),
                ));
            }
        }
        Ok(Self {
            name: raw.name,
            memory_bytes: raw.memory_bytes,
            ssh: raw.ssh,
            ssh_config: raw.ssh_config,
            launch: raw.launch,
            packages: raw.packages,
            resources: raw.resources,
            private_web: raw.private_web,
            ingress: raw.ingress,
        })
    }
}

impl From<Workspace> for Declaration {
    fn from(workspace: Workspace) -> Self {
        Self {
            version: 2,
            name: workspace.name,
            memory_bytes: workspace.memory_bytes,
            ssh: workspace.ssh,
            ssh_config: workspace.ssh_config,
            launch: workspace.launch,
            packages: workspace.packages,
            resources: workspace.resources,
            private_web: workspace.private_web,
            ingress: workspace.ingress,
        }
    }
}

fn path_contains(parent: &str, child: &str) -> bool {
    parent == child
        || child
            .strip_prefix(parent)
            .is_some_and(|tail| tail.starts_with('/'))
}

impl Workspace {
    pub fn parse(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn memory_bytes(&self) -> NonZeroU64 {
        self.memory_bytes
    }
    pub fn ssh(&self) -> &SshTarget {
        &self.ssh
    }
    pub fn launch(&self) -> &[String] {
        &self.launch
    }
    pub fn ssh_config(&self) -> Option<&HostPath> {
        self.ssh_config.as_ref()
    }
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }
    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }
    pub fn private_web(&self) -> &[PrivateWeb] {
        &self.private_web
    }
    pub fn ingress(&self) -> &Ingress {
        &self.ingress
    }
}
