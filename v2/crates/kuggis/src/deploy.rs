//! Pure compilation of a validated workspace into plaintext host/guest files.
//! Only the host executor performs effects. Rendered artifacts are not evidence
//! that the live boundary matches them.
use crate::{config::Workspace, Error, Result};
use serde::Serialize;
use std::{collections::BTreeMap, net::IpAddr};

pub const PROXY_PORT: u16 = 3128;
pub const HOST: &str = "10.205.0.1";
pub const GUEST: &str = "10.205.0.2";
pub const RUNTIME: &str = "/usr/local/lib/kuggis-v2";
pub const CONFIG: &str = "/etc/kuggis/workspace.json";
pub const PROXY: &str = "http://10.205.0.1:3128";
pub const HOST_EXECUTOR: &str = include_str!("../runtime/host.py");
pub const GUEST_EXECUTOR: &str = include_str!("../runtime/guest.py");

/// One dedicated host and one fixed user/network namespace allocation.
#[derive(Debug, Serialize)]
pub struct Boundary {
    machine: &'static str,
    interface: &'static str,
    uid_base: u32,
    uid_count: u32,
    host: &'static str,
    guest: &'static str,
}

impl Default for Boundary {
    fn default() -> Self {
        Self {
            machine: "kuggis-v2",
            interface: "kv2-host",
            uid_base: 524288,
            uid_count: 65536,
            host: HOST,
            guest: GUEST,
        }
    }
}

impl Boundary {
    pub fn is_internal(ip: IpAddr) -> bool {
        let v4 = match ip {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(ip) => ip.to_ipv4_mapped(),
        };
        v4.is_some_and(|ip| ip.octets()[..3] == [10, 205, 0] && ip.octets()[3] < 4)
    }
}

#[derive(Debug, Serialize)]
pub struct File {
    pub mode: u32,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Bundle<'a> {
    pub version: u8,
    pub workspace: &'a Workspace,
    pub boundary: Boundary,
    pub files: BTreeMap<String, File>,
}

impl<'a> Bundle<'a> {
    pub fn compile(workspace: &'a Workspace) -> Self {
        let mut files = BTreeMap::new();
        let mut add = |path: &str, mode, content: String| {
            files.insert(path.into(), File { mode, content });
        };
        add(
            "/usr/local/libexec/kuggis-host",
            0o755,
            HOST_EXECUTOR.into(),
        );
        add(&format!("{RUNTIME}/guest.py"), 0o644, GUEST_EXECUTOR.into());
        add("/etc/kuggis/wall.nft", 0o600, firewall());
        add(
            "/etc/systemd/network/00-kuggis-v2.network",
            0o644,
            "[Match]\nName=kv2-host\n[Link]\nUnmanaged=yes\n".into(),
        );
        add(
            "/etc/systemd/system/kuggis-v2-wall.service",
            0o644,
            include_str!("../templates/wall.service").into(),
        );
        add(
            "/etc/systemd/system/kuggis-v2-proxy.service",
            0o644,
            include_str!("../templates/proxy.service").into(),
        );
        let sockets = workspace
            .private_web()
            .iter()
            .map(|web| format!("kuggis-v2-web-{}.socket", web.name.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        add(
            "/etc/systemd/system/kuggis-v2.service",
            0o644,
            include_str!("../templates/workspace.service")
                .replace("@WEB_SOCKETS@", &sockets)
                .replace("@MEMORY_BYTES@", &workspace.memory_bytes().to_string()),
        );
        for web in workspace.private_web() {
            let unit = format!("kuggis-v2-web-{}", web.name.as_str());
            add(
                &format!("/etc/systemd/system/{unit}.socket"),
                0o644,
                include_str!("../templates/web.socket")
                    .replace("@NAME@", web.name.as_str())
                    .replace("@HOST_PORT@", &web.host_port.to_string()),
            );
            add(
                &format!("/etc/systemd/system/{unit}.service"),
                0o644,
                include_str!("../templates/web.service")
                    .replace("@NAME@", web.name.as_str())
                    .replace("@GUEST_PORT@", &web.guest_port.to_string()),
            );
        }
        Self {
            version: 2,
            workspace,
            boundary: Boundary::default(),
            files,
        }
    }
}

pub fn firewall() -> String {
    include_str!("../templates/wall.nft").into()
}

/// Normalized nftables state for host evidence comparisons. Counters and object
/// handles vary; rule order within each chain is semantically significant.
pub fn normalize_nft(value: &serde_json::Value) -> Result<serde_json::Value> {
    let entries = value
        .get("nftables")
        .and_then(|value| value.as_array())
        .ok_or_else(|| Error("invalid nftables JSON".into()))?;
    fn clean(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Array(values) => values.iter().map(clean).collect(),
            serde_json::Value::Object(values) => values
                .iter()
                .filter(|(key, _)| !["handle", "index"].contains(&key.as_str()))
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if key == "counter" {
                            serde_json::json!({})
                        } else {
                            clean(value)
                        },
                    )
                })
                .collect(),
            value => value.clone(),
        }
    }
    let mut entries: Vec<_> = entries
        .iter()
        .filter(|entry| entry.get("metainfo").is_none())
        .map(clean)
        .collect();
    entries.sort_by_key(|entry| {
        (
            entry
                .as_object()
                .and_then(|value| value.keys().next())
                .cloned(),
            entry
                .get("rule")
                .and_then(|rule| rule.get("chain"))
                .map(|chain| chain.to_string())
                .unwrap_or_else(|| entry.to_string()),
        )
    });
    Ok(serde_json::json!({"nftables": entries}))
}
