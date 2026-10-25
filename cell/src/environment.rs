//! Mutable guest state is separate from the host-owned authority boundary.
//! The host helper never chroots into an environment after its first boot:
//! upgrades execute inside its namespaces, where guest symlinks are harmless.
use serde::{Deserialize, Serialize};
use std::{error::Error, fs, path::Path, process::Command};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
pub const SPEC: &str = "/etc/cell/environment.json";
pub const HELPER: &str = "/usr/local/lib/cell/environment.py";
pub const PROXY: &str = "10.203.0.1:3128";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundarySpec {
    pub version: u32,
    pub machine: String,
    pub uid_base: u32,
    pub uid_count: u32,
    pub interface: String,
    pub host_ip: String,
    pub guest_ip: String,
}
impl Default for BoundarySpec {
    fn default() -> Self {
        Self {
            version: 1,
            machine: "cell-env".into(),
            uid_base: 524288,
            uid_count: 65536,
            interface: "ve-cell-env".into(),
            host_ip: "10.203.0.1".into(),
            guest_ip: "10.203.0.2".into(),
        }
    }
}

impl BoundarySpec {
    /// The guest's `agent` (UID 1000) as host processes see it, e.g. through
    /// SO_PEERCRED. The host's own legacy `agent` user is a different UID.
    pub fn host_agent_uid(&self) -> u32 {
        self.uid_base + 1000
    }
}

pub fn installed() -> bool {
    Path::new(SPEC).exists()
}

pub fn helper(action: &str) -> Result<String> {
    let out = Command::new("python3").args([HELPER, action]).output()?;
    if !out.status.success() {
        return Err(format!(
            "environment {action}: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

pub fn install() -> Result {
    let spec = BoundarySpec::default();
    if installed() && serde_json::from_slice::<BoundarySpec>(&fs::read(SPEC)?)? != spec {
        return Err("environment boundary changed; an explicit migration is required".into());
    }
    fs::create_dir_all("/usr/local/lib/cell")?;
    fs::write(HELPER, include_str!("environment/host.py"))?;
    fs::write(SPEC, serde_json::to_vec_pretty(&spec)?)?;
    // Guest-side upgrade script is only invoked through systemd-run --machine.
    fs::write(
        "/usr/local/lib/cell/environment-guest.py",
        include_str!("environment/guest.py"),
    )?;
    helper("prepare")?;
    Ok(())
}

pub fn firewall() -> String {
    firewall_for(&crate::host_runtime::Runtime::default())
}

pub fn firewall_for(runtime: &crate::host_runtime::Runtime) -> String {
    let b = BoundarySpec::default();
    let dns = runtime.dns().iter().map(|ip| {
        let family = if ip.is_ipv4() { "ip" } else { "ip6" };
        format!("  meta skuid \"cell-proxy\" {family} daddr {ip} udp dport 53 accept\n  meta skuid \"cell-proxy\" {family} daddr {ip} tcp dport 53 accept")
    }).collect::<Vec<_>>().join("\n");
    format!(
        r#"table inet cell {{}}
flush table inet cell
table inet cell {{
 chain input {{
  type filter hook input priority -200; policy accept;
  iifname "{iface}" ip daddr {host} tcp dport 3128 accept
  iifname "{iface}" counter reject
  iifname != "lo" tcp dport 3128 counter reject
 }}
 chain forward {{
  type filter hook forward priority -200; policy accept;
  iifname "{iface}" counter reject
 }}
 chain out {{
  type filter hook output priority filter; policy accept;
  meta skuid "agent" counter reject
  meta skuid "cell-proxy" oifname "lo" accept
  meta skuid "cell-proxy" oifname "{iface}" tcp sport 3128 accept
{dns}
  meta skuid "cell-proxy" tcp dport {{ 443, 8443 }} accept
  meta skuid "cell-proxy" counter reject
 }}
}}
"#,
        iface = b.interface,
        host = b.host_ip
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authority_follows_the_interface_not_the_guest_identity() {
        let wall = firewall();
        assert!(wall.contains("iifname \"ve-cell-env\" counter reject"));
        assert!(wall.contains("hook input priority -200"));
        assert!(wall.contains("hook forward priority -200"));
        assert!(wall.contains("ip daddr 10.203.0.1 tcp dport 3128 accept"));
        assert!(!wall.contains("meta skuid \"root\""));
        assert_ne!(BoundarySpec::default().uid_base, 0);
    }

    #[test]
    fn portable_dns_does_not_open_guest_egress_or_keep_the_platform_gateway() {
        let runtime: crate::host_runtime::Runtime = serde_json::from_str(r#"{"backend":"ssh","dns":["127.0.0.53","2001:4860:4860::8888"],"memory_max":8589934592}"#).unwrap();
        let wall = firewall_for(&runtime);
        assert!(!wall.contains("169.254.169.254"));
        assert!(wall.contains("ip daddr 127.0.0.53 udp dport 53 accept"));
        assert!(wall.contains("ip6 daddr 2001:4860:4860::8888 tcp dport 53 accept"));
        assert!(wall.contains("iifname \"ve-cell-env\" counter reject"));
        assert!(wall.contains("meta skuid \"cell-proxy\" counter reject"));
    }
}
