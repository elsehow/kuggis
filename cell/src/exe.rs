//! exe.dev as the edge: read its VM and integration listings back into
//! cells and doors. Only what the edge enforces is visible here. Grants the
//! box enforces are reported by the box itself, in `Checked`.

use crate::apply::{Edge, Fallible};
use crate::cell::{Cell, TAG_PREFIX};
use crate::grant::{Grant, Harness, EDGE_SUFFIX};
use crate::prim::{Hash, Host, Name};
use crate::said::{Key, Said, Statement};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

/// A door is an http-proxy integration named `door-<child>`, targeting the
/// child's VM with `--peer`, attached to the parent's VM. The edge stamps
/// every knock with the caller's VM name, and serves the hostname only to
/// VMs it is attached to.
pub const DOOR_PREFIX: &str = "door-";

/// The integration's name.
pub fn door_name(child: &Name) -> String {
    format!("{DOOR_PREFIX}{child}")
}

/// The hostname the parent knocks on.
pub fn door_host(child: &Name) -> Host {
    format!("{}{EDGE_SUFFIX}", door_name(child))
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
    /// The cell this VM is the root of, by its `cell-` tag. A VM without
    /// the tag is not a cell, whatever its name: `cell adopt` adds the tag.
    pub fn cell_name(&self) -> Option<Name> {
        self.tags
            .iter()
            .find_map(|t| t.strip_prefix(TAG_PREFIX))
            .and_then(|n| n.parse().ok())
    }
}

/// The VM that is this cell's root.
pub fn vm_of<'a>(vms: &'a [Vm], cell: &Name) -> Option<&'a Vm> {
    vms.iter().find(|v| v.cell_name().as_ref() == Some(cell))
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

/// exe.dev over ssh: the CLI is the API, and the HTTPS API is the same
/// words in a POST body. `bin` is the Linux build shipped to a box for
/// setup; without it, setup is refused.
pub struct Ssh {
    pub bin: Option<PathBuf>,
}

const VM_OPTS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
    "-o",
    "ConnectTimeout=10",
];

/// A VM just made takes a while to answer; ssh says 255 until it does.
fn retrying(what: &str, mut attempt: impl FnMut() -> std::io::Result<Output>) -> Fallible<String> {
    for tries in 1.. {
        let out = attempt()?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        if out.status.code() != Some(255) || tries >= 20 {
            return Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
        }
        thread::sleep(Duration::from_secs(3));
    }
    unreachable!()
}

impl Ssh {
    fn vm(vm: &str, cmd: &str) -> Fallible<String> {
        Ssh::vm_in(vm, cmd, &[])
    }

    /// A command on a VM with something on its stdin.
    fn vm_in(vm: &str, cmd: &str, input: &[u8]) -> Fallible<String> {
        let host = format!("{vm}.exe.xyz");
        retrying(&format!("ssh {host} {cmd}"), || {
            let mut child = Command::new("ssh")
                .args(VM_OPTS)
                .arg(&host)
                .arg(cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            child.stdin.take().expect("piped").write_all(input)?;
            child.wait_with_output()
        })
    }

    fn put(vm: &str, local: &Path, remote: &str) -> Fallible<()> {
        let to = format!("{vm}.exe.xyz:{remote}");
        retrying(&format!("scp {} {to}", local.display()), || {
            Command::new("scp")
                .args(VM_OPTS)
                .arg(local)
                .arg(&to)
                .output()
        })
        .map(drop)
    }
}

impl Edge for Ssh {
    fn listing(&self) -> Fallible<(Vec<Vm>, Vec<Integration>)> {
        let listing: Listing = serde_json::from_str(&self.run(&["ls", "--json"])?)?;
        let ints = serde_json::from_str(&self.run(&["integrations", "list", "--json"])?)?;
        Ok((listing.vms, ints))
    }

    fn run(&self, args: &[&str]) -> Fallible<String> {
        let out = Command::new("ssh")
            .args(["-o", "BatchMode=yes", "exe.dev"])
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(format!(
                "ssh exe.dev {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    fn log(&self, vm: &str) -> Fallible<Option<(Key, Vec<Statement>)>> {
        let raw = Ssh::vm(
            vm,
            "test -f /etc/cell/key.pub && cat /etc/cell/key.pub && cat /var/lib/cell/said/*.json 2>/dev/null; true",
        )?;
        parse_log(&raw)
    }

    fn binary(&self) -> Fallible<Option<Hash>> {
        self.bin
            .as_deref()
            .map(|p| -> Fallible<Hash> { Ok(Hash::of(&std::fs::read(p)?)) })
            .transpose()
    }

    fn setup(&self, vm: &str, cell: &Cell) -> Fallible<String> {
        let bin = self
            .bin
            .as_deref()
            .ok_or("setup needs the Linux build: cell apply ... --bin PATH")?;
        let file =
            std::env::temp_dir().join(format!("cell-{}-{}.json", cell.name, std::process::id()));
        std::fs::write(&file, cell.canonical())?;
        Ssh::put(vm, bin, "/tmp/cell")?;
        let sent = Ssh::put(vm, &file, "/tmp/cell.json");
        let _ = std::fs::remove_file(&file);
        sent?;
        Ssh::vm(vm, "sudo /tmp/cell setup /tmp/cell.json")
    }

    fn knock(&self, vm: &str, said: &Said) -> Fallible<u64> {
        let out = Ssh::vm_in(
            vm,
            &format!(
                "sudo {} knock --inbox {}",
                crate::wall::BIN,
                crate::wall::INBOX
            ),
            &serde_json::to_vec(said)?,
        )?;
        Ok(out.trim().parse()?)
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

/// The key on its first line, then the statements as the shell glob gave
/// them: in name order, where 10 comes before 2. Put in sequence here, so
/// that the verifier's order check speaks of gaps and nothing else.
fn parse_log(raw: &str) -> Fallible<Option<(Key, Vec<Statement>)>> {
    let Some((key, rest)) = raw.split_once('\n') else {
        return Ok(None);
    };
    let key: Key = key.parse()?;
    let mut log = serde_json::Deserializer::from_str(rest)
        .into_iter::<Statement>()
        .collect::<Result<Vec<_>, _>>()?;
    log.sort_by_key(|s| s.seq);
    Ok(Some((key, log)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::said::Secret;

    #[test]
    fn a_log_in_name_order_is_put_in_sequence() {
        let secret = Secret::generate();
        let name: Name = "a".parse().unwrap();
        let said = Said::Session {
            name: "s".parse().unwrap(),
            harness: Harness::Claude,
            state: crate::said::State::Idle,
            text: None,
        };
        let mut raw = format!("{}\n", secret.public());
        for seq in [1, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9] {
            let s = Statement::sign(&secret, name.clone(), seq, said.clone());
            raw.push_str(&serde_json::to_string(&s).unwrap());
            raw.push('\n');
        }
        let (key, log) = parse_log(&raw).unwrap().unwrap();
        assert!(key == secret.public());
        let seqs: Vec<u64> = log.iter().map(|s| s.seq).collect();
        assert_eq!(seqs, (1..=11).collect::<Vec<_>>());
        assert!(parse_log("").unwrap().is_none());
    }

    const VMS: &str = r#"{"vms":[
      {"vm_name":"airo-dev","tags":["box","cell-airo"],"status":"running"},
      {"vm_name":"scratch","tags":["cell-scratch"],"status":"running"},
      {"vm_name":"plain","tags":["box"],"status":"running"},
      {"vm_name":"odd","tags":["cell-Bad Name"],"status":"running"}
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
            "a VM without a cell tag is not a cell, nor one whose tag is not a name"
        );
        assert_eq!(
            vm_of(&vms.vms, &"airo".parse().unwrap()).unwrap().vm_name,
            "airo-dev",
            "the root may be a VM of another name, adopted by tag"
        );
        assert!(vm_of(&vms.vms, &"plain".parse().unwrap()).is_none());
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
