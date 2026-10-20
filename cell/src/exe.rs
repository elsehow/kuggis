//! exe.dev as the edge: read its VM and integration listings back into
//! cells and doors. Only what the edge enforces is visible here. Grants the
//! box enforces are reported by the box itself, in `Checked`.

use crate::cell::{Cell, TAG_PREFIX};
use crate::grant::{Grant, Harness, EDGE_SUFFIX};
use crate::prim::{Host, Name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A door is an http-proxy integration named `door-<child>`, targeting the
/// child's VM with `--peer`, attached to the parent's VM. The edge stamps
/// every knock with the caller's VM name, and serves the hostname only to
/// VMs it is attached to.
pub const DOOR_PREFIX: &str = "door-";

/// The hostname the parent knocks on.
pub fn door_host(child: &Name) -> Host {
    format!("{DOOR_PREFIX}{child}{EDGE_SUFFIX}")
        .parse()
        .expect("a name between fixed labels is a host")
}

/// The child behind a door hostname, if it is one.
pub fn door_of_host(host: &Host) -> Option<Name> {
    host.as_str()
        .strip_suffix(EDGE_SUFFIX)?
        .strip_prefix(DOOR_PREFIX)?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Vm {
    pub vm_name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Listing {
    pub vms: Vec<Vm>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Integration {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub config: serde_json::Value,
}

impl Vm {
    /// The cell this VM is, by its `cell-` tag, else by its name.
    pub fn cell_name(&self) -> Option<Name> {
        let tagged = self
            .tags
            .iter()
            .find_map(|t| t.strip_prefix(TAG_PREFIX))
            .and_then(|n| n.parse().ok());
        tagged.or_else(|| self.vm_name.parse().ok())
    }
}

impl Integration {
    /// Attached to this VM, by name or by one of its tags. `auto:all` is
    /// not an attachment anyone declared, so it is not a grant.
    pub fn attached_to(&self, vm: &Vm) -> bool {
        self.attachments.iter().any(|a| {
            a.strip_prefix("vm:").is_some_and(|n| n == vm.vm_name)
                || a.strip_prefix("tag:")
                    .is_some_and(|t| vm.tags.iter().any(|x| x == t))
        })
    }

    /// The child this integration is a door to, by its name.
    pub fn door(&self) -> Option<Name> {
        self.name.strip_prefix(DOOR_PREFIX)?.parse().ok()
    }

    /// The grants this integration is. A door is not a grant.
    pub fn grants(&self) -> Vec<Grant> {
        if self.door().is_some() {
            return vec![];
        }
        match self.kind.as_str() {
            "github" => {
                let write = !self
                    .config
                    .get("readonly")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.config
                    .get("repositories")
                    .and_then(|r| r.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str()?.parse().ok())
                    .map(|repo| Grant::Code { repo, write })
                    .collect()
            }
            "llm" => {
                let chatgpt = self
                    .config
                    .pointer("/providers/openai/source")
                    .and_then(|v| v.as_str())
                    == Some("chatgpt");
                if chatgpt {
                    vec![Grant::Agent(Harness::Codex)]
                } else {
                    vec![]
                }
            }
            _ => self.name.parse().map(Grant::Edge).into_iter().collect(),
        }
    }
}

/// The edge as it stands: every VM as the cell it shows, and every door,
/// parent to children. Unvalidated, since drift is what it is for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observed {
    pub cells: BTreeMap<Name, Cell>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub doors: BTreeMap<Name, BTreeSet<Name>>,
}

static NO_DOORS: BTreeSet<Name> = BTreeSet::new();

impl Observed {
    /// A cell and the doors it holds, if the edge has it.
    pub fn of(&self, name: &Name) -> Option<(&Cell, &BTreeSet<Name>)> {
        let cell = self.cells.get(name)?;
        Some((cell, self.doors.get(name).unwrap_or(&NO_DOORS)))
    }
}

pub fn observe(vms: &[Vm], ints: &[Integration]) -> Observed {
    let mut out = Observed::default();
    for vm in vms {
        let Some(name) = vm.cell_name() else { continue };
        let attached = || ints.iter().filter(|i| i.attached_to(vm));
        let grants = attached().flat_map(Integration::grants);
        let doors: BTreeSet<Name> = attached().filter_map(Integration::door).collect();
        if !doors.is_empty() {
            out.doors.insert(name.clone(), doors);
        }
        out.cells.insert(name.clone(), Cell::new(name, grants));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VMS: &str = r#"{"vms":[
      {"vm_name":"airo-dev","tags":["box","cell-airo"],"status":"running"},
      {"vm_name":"scratch","status":"running"},
      {"vm_name":"Bad Name","tags":[],"status":"running"}
    ]}"#;
    const INTS: &str = r#"[
      {"name":"gh-airo-dev","type":"github","attachments":["vm:airo-dev"],"config":{"repositories":["elsehow/airo","forecastingresearch/AIRO"],"installation_id":1}},
      {"name":"gh-ro","type":"github","attachments":["tag:cell-airo"],"config":{"repositories":["o/r"],"readonly":true}},
      {"name":"codex","type":"llm","attachments":["tag:cell-airo"],"config":{"providers":{"openai":{"enabled":true,"source":"chatgpt"}}}},
      {"name":"llm","type":"llm","attachments":["auto:all"],"config":{"providers":{"openai":{"enabled":true,"source":"managed"}}}},
      {"name":"brave","type":"brave","attachments":["vm:scratch"],"config":{}},
      {"name":"Odd Name","type":"exa","attachments":["vm:scratch"],"config":{}},
      {"name":"door-scratch","type":"http-proxy","attachments":["tag:cell-airo"],"config":{"target":"https://scratch.exe.xyz/","peer":true}},
      {"name":"door-Nope","type":"http-proxy","attachments":["vm:scratch"],"config":{}}
    ]"#;

    #[test]
    fn reads_the_edge_back_as_cells_and_doors() {
        let vms: Listing = serde_json::from_str(VMS).unwrap();
        let ints: Vec<Integration> = serde_json::from_str(INTS).unwrap();
        let seen = observe(&vms.vms, &ints);
        assert_eq!(
            seen.cells.len(),
            2,
            "a VM whose name is not a Name is not a cell"
        );
        let airo: Name = "airo".parse().unwrap();
        let want: Cell = serde_json::from_str(
            r#"{"name":"airo","grants":[{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}},{"code":{"repo":"forecastingresearch/airo","write":true}},{"code":{"repo":"o/r","write":false}}]}"#,
        )
        .unwrap();
        let (cell, doors) = seen.of(&airo).unwrap();
        assert_eq!(cell, &want, "a door is not a grant");
        assert_eq!(
            doors.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["scratch"]
        );
        let scratch: Name = "scratch".parse().unwrap();
        let (cell, doors) = seen.of(&scratch).unwrap();
        assert_eq!(
            cell.grants.len(),
            1,
            "the managed llm on auto:all and an unnameable integration are not grants"
        );
        assert!(cell.grants.contains(&Grant::Edge("brave".parse().unwrap())));
        assert!(doors.is_empty(), "door-Nope is not a door to anyone");
        let json = serde_json::to_string(&seen).unwrap();
        assert!(json.contains(r#""doors":{"airo":["scratch"]}"#), "{json}");
        assert_eq!(serde_json::from_str::<Observed>(&json).unwrap(), seen);
    }

    #[test]
    fn door_hosts() {
        let scratch: Name = "scratch".parse().unwrap();
        let h = door_host(&scratch);
        assert_eq!(h.as_str(), "door-scratch.int.exe.xyz");
        assert_eq!(door_of_host(&h), Some(scratch));
        for not in [
            "door-scratch.exe.xyz",
            "doorscratch.int.exe.xyz",
            "door-scratch.int.exe.xyz.evil.example",
            "github.int.exe.xyz",
            "door-plan.int.exe.xyz",
        ] {
            let h: Host = not.parse().unwrap();
            assert_eq!(door_of_host(&h), None, "{not}");
        }
    }
}
