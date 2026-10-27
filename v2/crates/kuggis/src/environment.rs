//! A reviewable environment description, not an installer or runtime report.
use crate::config::{Credential, Endpoint, Name, PrivateWeb, Workspace};
use serde::Serialize;

pub const GUEST_HOME: &str = "/home/agent";
pub const GUEST_WORKSPACE: &str = "/home/agent/workspace";
pub const HOST_HELPER: &str = "/usr/local/libexec/kuggis-host";

#[derive(Debug, Serialize)]
pub struct Environment<'a> {
    pub status: DescriptionStatus,
    pub name: &'a Name,
    pub host_config: String,
    pub host_data: String,
    pub guest_home: &'static str,
    pub guest_workspace: &'static str,
    pub machine: String,
    pub guest_root_host_uid: u32,
    pub memory_bytes: u64,
    pub launch: &'a [String],
    pub endpoints: Vec<&'a Endpoint>,
    pub credentials: Vec<&'a Credential>,
    pub private_web: Vec<PrivateRelay<'a>>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionStatus {
    ConfigurationOnly,
}

#[derive(Debug, Serialize)]
pub struct PrivateRelay<'a> {
    pub publication: &'a PrivateWeb,
    pub host_bind: &'static str,
    pub requires_authenticated_ingress: bool,
}

impl<'a> Environment<'a> {
    pub fn describe(workspace: &'a Workspace) -> Self {
        Self {
            status: DescriptionStatus::ConfigurationOnly,
            name: workspace.name(),
            host_config: crate::deploy::CONFIG.into(),
            host_data: "/var/lib/kuggis/rootfs".into(),
            guest_home: GUEST_HOME,
            guest_workspace: GUEST_WORKSPACE,
            machine: "kuggis-v2".into(),
            // One dedicated host/guest per workspace in v2's initial scope.
            guest_root_host_uid: 524288,
            memory_bytes: workspace.memory_bytes().get(),
            launch: workspace.launch(),
            endpoints: workspace
                .resources()
                .iter()
                .flat_map(|resource| &resource.endpoints)
                .collect(),
            credentials: workspace
                .resources()
                .iter()
                .flat_map(|resource| &resource.credentials)
                .collect(),
            private_web: workspace
                .private_web()
                .iter()
                .map(|publication| PrivateRelay {
                    publication,
                    host_bind: "127.0.0.1",
                    requires_authenticated_ingress: true,
                })
                .collect(),
        }
    }
}
