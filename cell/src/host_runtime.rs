//! Host networking and resource limits are deployment facts, not cell grants.
//! Missing configuration preserves the existing exe.dev runtime.
use crate::{apply::Fallible, Cell, Grant};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, net::IpAddr, path::Path};

pub const PATH: &str = "/etc/cell/host.json";
pub const GUEST_PATH: &str = "/run/cell-runtime/host.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum Runtime {
    #[default]
    ExeDev,
    Ssh {
        /// Exact upstream resolvers used by the host's system resolver.
        dns: BTreeSet<IpAddr>,
        /// Explicit guest memory cap in bytes. CPU is not capped by Kuggis.
        memory_max: u64,
    },
}

impl Runtime {
    pub fn load(path: &Path) -> Fallible<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let runtime: Self = serde_json::from_slice(&bytes)?;
                runtime.validate()?;
                Ok(runtime)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn validate(&self) -> Fallible {
        if let Self::Ssh { dns, memory_max } = self {
            if dns.is_empty() || dns.len() > 8 {
                return Err(
                    "SSH runtime requires between one and eight DNS resolver addresses".into(),
                );
            }
            if dns
                .iter()
                .any(|ip| ip.is_unspecified() || ip.is_multicast())
            {
                return Err("DNS resolver must be a unicast address".into());
            }
            if *memory_max < 256 * 1024 * 1024 || *memory_max > i64::MAX as u64 {
                return Err(
                    "memory_max must be at least 256 MiB and fit in a signed 64-bit integer".into(),
                );
            }
        }
        Ok(())
    }

    pub fn validate_cell(&self, cell: &Cell) -> Fallible {
        self.validate()?;
        if !self.is_exe() {
            for grant in &cell.grants {
                match grant {
                    Grant::Provider(_) | Grant::Code { .. } => {},
                    Grant::Host(host) if !host.as_str().ends_with(crate::grant::EDGE_SUFFIX) => {},
                    _ => return Err(format!("SSH hosts do not yet support grant {grant}; an explicit replacement for the exe.dev integration is required").into()),
                }
            }
        }
        Ok(())
    }

    pub fn is_exe(&self) -> bool {
        matches!(self, Self::ExeDev)
    }

    pub fn dns(&self) -> BTreeSet<IpAddr> {
        match self {
            Self::ExeDev => [crate::proxy::GATEWAY.into()].into(),
            Self::Ssh { dns, .. } => dns.clone(),
        }
    }

    pub fn memory_max(&self) -> u64 {
        match self {
            Self::ExeDev => 4 * 1024 * 1024 * 1024,
            Self::Ssh { memory_max, .. } => *memory_max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_hosts_do_not_claim_platform_grants() {
        let runtime: Runtime = serde_json::from_str(r#"{"backend":"ssh","dns":["127.0.0.53","2001:4860:4860::8888"],"memory_max":8589934592}"#).unwrap();
        for grant in [
            r#"{"edge":"search"}"#,
            r#"{"host":"door-child.int.exe.xyz"}"#,
            r#"{"publish":"private"}"#,
        ] {
            let cell: Cell =
                serde_json::from_str(&format!(r#"{{"name":"worker","grants":[{grant}]}}"#))
                    .unwrap();
            assert!(runtime.validate_cell(&cell).is_err(), "{grant}");
        }
        let cell: Cell = serde_json::from_str(
            r#"{"name":"worker","grants":[{"provider":"openai"},{"host":"pypi.org"},{"code":{"repo":"example/project","write":false}}]}"#,
        )
        .unwrap();
        runtime.validate_cell(&cell).unwrap();
        assert_eq!(runtime.memory_max(), 8589934592);
    }
    #[test]
    fn invalid_runtime_is_rejected() {
        for dns in ["[]", "[\"0.0.0.0\"]", "[\"ff02::1\"]"] {
            let runtime: Runtime = serde_json::from_str(&format!(
                r#"{{"backend":"ssh","dns":{dns},"memory_max":8589934592}}"#
            ))
            .unwrap();
            assert!(runtime.validate().is_err());
        }
        assert!(serde_json::from_str::<Runtime>(
            r#"{"backend":"ssh","dns":["1.1.1.1"],"memory_max":123,"extra":true}"#
        )
        .is_err());
    }
}
