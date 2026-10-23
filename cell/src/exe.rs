//! exe.dev as the edge: read its VM and integration listings back into
//! cells and doors. Only what the edge enforces is visible here. Grants the
//! box enforces are reported by the box itself, in `Checked`.

use crate::apply::{Edge, Fallible};
use crate::cell::{Cell, TAG_PREFIX};
use crate::grant::{Grant, EDGE_SUFFIX};
use crate::prim::{Hash, Host, Name};
use crate::said::{Key, Said, Statement};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

/// exe.dev names are a separate namespace from Kuggis cell names. Keep them
/// validated before they become routing or destructive-operation targets.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VmName(String);

impl VmName {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let bytes = value.as_bytes();
        let label_char =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        if bytes.is_empty()
            || bytes.len() > 63
            || !bytes[0].is_ascii_alphanumeric()
            || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
            || !bytes.iter().all(|byte| label_char(*byte))
        {
            return Err("Invalid exe.dev VM name".into());
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for VmName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl TryFrom<String> for VmName {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}
impl From<VmName> for String {
    fn from(value: VmName) -> Self {
        value.0
    }
}

/// New VMs use a stricter namespace than logical cell names (and some older
/// VMs). Preserve already-valid names; otherwise produce a stable provider name.
pub fn provision_name(name: &Name) -> String {
    let raw = name.as_str();
    if (5..=52).contains(&raw.len())
        && raw.as_bytes()[0].is_ascii_lowercase()
        && !raw.contains("--")
    {
        return raw.into();
    }
    let slug = raw
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let candidate = format!("cell-{slug}");
    if candidate.len() <= 52 && !raw.contains("--") {
        return candidate;
    }
    // Truncation and normalization can lose information. Retain a digest to
    // distinguish names that would otherwise collapse to the same slug.
    let prefix = candidate[..candidate.len().min(39)].trim_end_matches('-');
    format!("{prefix}-{}", &Hash::of(raw.as_bytes()).to_string()[..12])
}

/// Parsed exe.dev attachment syntax. Unrecognized external forms remain
/// observable but never gain ownership authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentTarget {
    Vm(VmName),
    Tag(String),
    Auto(String),
    Unknown(String),
}
impl AttachmentTarget {
    pub fn parse(raw: &str) -> Self {
        if let Some(name) = raw.strip_prefix("vm:") {
            return VmName::parse(name.to_owned())
                .map(Self::Vm)
                .unwrap_or_else(|_| Self::Unknown(raw.into()));
        }
        if let Some(tag) = raw.strip_prefix("tag:") {
            return if tag.is_empty() || tag.chars().any(char::is_whitespace) {
                Self::Unknown(raw.into())
            } else {
                Self::Tag(tag.into())
            };
        }
        if let Some(target) = raw.strip_prefix("auto:") {
            return if target.is_empty() || target.chars().any(char::is_whitespace) {
                Self::Unknown(raw.into())
            } else {
                Self::Auto(target.into())
            };
        }
        Self::Unknown(raw.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind<'a> {
    GitHub,
    HttpProxy,
    Llm,
    Brave,
    Exa,
    Other(&'a str),
}
impl<'a> IntegrationKind<'a> {
    pub fn parse(raw: &'a str) -> Self {
        match raw {
            "github" => Self::GitHub,
            "http-proxy" => Self::HttpProxy,
            "llm" => Self::Llm,
            "brave" => Self::Brave,
            "exa" => Self::Exa,
            value => Self::Other(value),
        }
    }
}

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

/// exe.dev prints an empty list as `null`.
fn null_is_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Vm {
    pub vm_name: String,
    /// Binds destructive operations to this VM incarnation, not a reusable name.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default, deserialize_with = "null_is_empty")]
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
    #[serde(default, deserialize_with = "null_is_empty")]
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

/// Resolve a cell's root VM only when its ownership tag is unique and the VM
/// carries no competing cell tag. Callers that may act on a VM use this rather
/// than treating the first matching tag as authority.
pub fn cell_vm<'a>(vms: &'a [Vm], cell: &Name) -> Result<Option<&'a Vm>, String> {
    let tag = format!("{TAG_PREFIX}{cell}");
    let matches: Vec<_> = vms.iter().filter(|vm| vm.tags.contains(&tag)).collect();
    if matches.len() > 1 {
        return Err(format!("Multiple VMs carry {tag}"));
    }
    let Some(vm) = matches.first().copied() else {
        return Ok(None);
    };
    if vm
        .tags
        .iter()
        .filter(|candidate| candidate.starts_with(TAG_PREFIX))
        .count()
        != 1
    {
        return Err(format!("VM {} carries tags for multiple cells", vm.vm_name));
    }
    VmName::parse(vm.vm_name.clone())
        .map_err(|_| format!("Cell VM has an invalid exe.dev name: {}", vm.vm_name))?;
    Ok(Some(vm))
}

/// Validate all declared cell ownership before a reconciliation or route map
/// is built. An ambiguous listing cannot be partially interpreted.
pub fn cell_vms(vms: &[Vm]) -> Result<BTreeMap<Name, &Vm>, String> {
    let mut result = BTreeMap::new();
    for vm in vms {
        let tags: Vec<_> = vm
            .tags
            .iter()
            .filter(|tag| tag.starts_with(TAG_PREFIX))
            .collect();
        if tags.is_empty() {
            continue;
        }
        VmName::parse(vm.vm_name.clone())
            .map_err(|_| format!("Cell VM has an invalid exe.dev name: {}", vm.vm_name))?;
        if tags.len() != 1 {
            return Err(format!("VM {} carries tags for multiple cells", vm.vm_name));
        }
        let raw = tags[0].strip_prefix(TAG_PREFIX).unwrap();
        let cell: Name = raw
            .parse()
            .map_err(|_| format!("VM {} carries an invalid cell tag", vm.vm_name))?;
        if result.insert(cell.clone(), vm).is_some() {
            return Err(format!("Multiple VMs carry the cell-{cell} tag"));
        }
    }
    Ok(result)
}

impl Integration {
    pub fn kind(&self) -> IntegrationKind<'_> {
        IntegrationKind::parse(&self.kind)
    }
    /// Attached to this VM, by name or by one of its tags. `auto:all` is
    /// not an attachment anyone declared, so it is not a grant.
    pub fn attached_to(&self, vm: &Vm) -> bool {
        self.attachments
            .iter()
            .any(|raw| match AttachmentTarget::parse(raw) {
                AttachmentTarget::Vm(name) => name.as_str() == vm.vm_name,
                AttachmentTarget::Tag(tag) => vm.tags.iter().any(|x| x == &tag),
                AttachmentTarget::Auto(_) | AttachmentTarget::Unknown(_) => false,
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
                let readonly = self.config.get("readonly").and_then(|v| v.as_bool()) == Some(true);
                self.config
                    .get("repositories")
                    .and_then(|r| r.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str()?.parse().ok())
                    .map(|repo| Grant::Code {
                        repo,
                        write: !readonly,
                        via: None,
                    })
                    .collect()
            }
            // Provider credentials now belong to Pi in each cell. A named
            // integration remains an explicit edge grant if still attached.
            "llm" if self.name == "llm" => vec![],
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
        control_output(args, out)
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

    fn context(&self, vm: &str, context: &crate::context::Context) -> Fallible {
        // Fixed host-owned directory, never a path supplied by the guest. Atomic
        // replacement beneath a directory bind is visible to existing sessions.
        // Publication is retried by the reconciler, not the provisioning SSH
        // retry loop: an unreachable cell must not lock operator mutations for
        // twenty connection attempts.
        let key: crate::Key = context.key.parse()?;
        let mut child = Command::new("ssh")
            .args(VM_OPTS)
            .args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"])
            .arg(format!("{vm}.exe.xyz"))
            .arg(format!("test \"$(cat /etc/cell/key.pub)\" = '{key}' && {}", r#"sudo sh -c 'set -eu; mkdir -p /usr/local/lib/cell; t=$(mktemp /usr/local/lib/cell/.context.XXXXXX); trap "rm -f \"$t\"" EXIT; cat > "$t"; chmod 644 "$t"; mv -f "$t" /usr/local/lib/cell/context.json'"#))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(&serde_json::to_vec(context)?)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(format!(
                "publish context on {vm}: {}",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        Ok(())
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

fn control_output(args: &[&str], out: Output) -> Fallible<String> {
    if out.status.success() {
        return Ok(String::from_utf8(out.stdout)?);
    }
    // exe.dev reports some validation errors on stdout, including JSON errors.
    // SSH diagnostics still take priority when both streams contain output.
    let detail = [&out.stderr, &out.stdout]
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let detail = if detail.is_empty() {
        out.status.to_string()
    } else {
        detail
    };
    Err(format!(
        "ssh exe.dev {}: {}",
        args.join(" "),
        detail.chars().take(4096).collect::<String>()
    )
    .into())
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
    fn provisioning_accepts_every_cell_name_shape_without_changing_identity() {
        assert_eq!(provision_name(&"project".parse().unwrap()), "project");
        assert_eq!(provision_name(&"app".parse().unwrap()), "cell-app");
        for raw in [
            "a".to_string(),
            "12345".into(),
            "a--b".into(),
            "a".repeat(63),
            format!("{}b", "a".repeat(62)),
        ] {
            let name = provision_name(&raw.parse().unwrap());
            assert!((5..=52).contains(&name.len()), "{name}");
            assert!(name.as_bytes()[0].is_ascii_lowercase());
            assert!(!name.contains("--") && !name.ends_with('-'));
            assert!(VmName::parse(name).is_ok());
        }
        assert_ne!(
            provision_name(&"a--b".parse().unwrap()),
            provision_name(&"a-b".parse().unwrap())
        );
        assert_ne!(
            provision_name(&"a".repeat(63).parse().unwrap()),
            provision_name(&format!("{}b", "a".repeat(62)).parse().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_validation_errors_on_stdout_are_visible() {
        use std::os::unix::process::ExitStatusExt;
        let out = Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: br#"{"error":"invalid VM name"}"#.to_vec(),
            stderr: Vec::new(),
        };
        let error = control_output(&["new", "--name=bad"], out)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid VM name"), "{error}");
    }

    #[test]
    fn ownership_lookup_refuses_ambiguous_vm_tags() {
        let name: Name = "dev".parse().unwrap();
        let vm = |vm_name: &str, tags: &[&str]| Vm {
            vm_name: vm_name.into(),
            created_at: Some("fixture".into()),
            tags: tags.iter().map(|tag| (*tag).into()).collect(),
            status: "running".into(),
        };
        assert_eq!(
            cell_vm(&[vm("dev", &["cell-dev"])], &name)
                .unwrap()
                .unwrap()
                .vm_name,
            "dev"
        );
        assert!(cell_vm(&[vm("a", &["cell-dev"]), vm("b", &["cell-dev"])], &name).is_err());
        assert!(cell_vm(&[vm("a", &["cell-dev", "cell-other"])], &name).is_err());
        assert!(cell_vms(&[vm("a", &["cell-dev"]), vm("b", &["cell-dev"])]).is_err());
    }

    #[test]
    fn attachment_parser_keeps_targets_typed_and_unknown_targets_inert() {
        assert_eq!(
            AttachmentTarget::parse("vm:dev"),
            AttachmentTarget::Vm(VmName::parse("dev").unwrap())
        );
        assert_eq!(
            AttachmentTarget::parse("tag:cell-dev"),
            AttachmentTarget::Tag("cell-dev".into())
        );
        assert_eq!(
            AttachmentTarget::parse("auto:all"),
            AttachmentTarget::Auto("all".into())
        );
        assert_eq!(
            AttachmentTarget::parse("vm:bad name"),
            AttachmentTarget::Unknown("vm:bad name".into())
        );
        assert_eq!(
            AttachmentTarget::parse("future:selector"),
            AttachmentTarget::Unknown("future:selector".into())
        );
        assert!(serde_json::from_str::<VmName>("\"-unsafe\"").is_err());
        assert!(serde_json::from_str::<VmName>("\"dev;echo-pwned\"").is_err());
    }

    #[test]
    fn a_log_in_name_order_is_put_in_sequence() {
        let secret = Secret::generate();
        let name: Name = "a".parse().unwrap();
        let said = Said::Session {
            session: crate::SessionRef {
                cell: name.clone(),
                id: crate::SessionId::generate(),
            },
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
      {"vm_name":"widget-dev","tags":["box","cell-widget"],"status":"running"},
      {"vm_name":"scratch","tags":["cell-scratch"],"status":"running"},
      {"vm_name":"plain","tags":["box"],"status":"running"},
      {"vm_name":"odd","tags":["cell-Bad Name"],"status":"running"}
    ]}"#;
    const INTS: &str = r#"[
      {"name":"gh-widget-dev","type":"github","attachments":["vm:widget-dev"],"config":{"repositories":["example/widget","upstream/WIDGET"],"installation_id":1}},
      {"name":"gh-ro","type":"github","attachments":["tag:cell-widget"],"config":{"repositories":["o/r"],"readonly":true}},
      {"name":"gh-loose","type":"github","attachments":null,"config":{"repositories":["o/loose"]}},
      {"name":"codex","type":"llm","attachments":["tag:cell-widget"],"config":{"providers":{"openai":{"enabled":true,"source":"chatgpt"}}}},
      {"name":"llm","type":"llm","attachments":["auto:all"],"config":{"providers":{"openai":{"enabled":true,"source":"managed"}}}},
      {"name":"brave","type":"brave","attachments":["vm:scratch"],"config":{}},
      {"name":"Odd Name","type":"exa","attachments":["vm:scratch"],"config":{}},
      {"name":"door-scratch","type":"http-proxy","attachments":["tag:cell-widget"],"config":{"target":"https://scratch.exe.xyz/","peer":true}},
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
            vm_of(&vms.vms, &"widget".parse().unwrap()).unwrap().vm_name,
            "widget-dev",
            "the root may be a VM of another name, adopted by tag"
        );
        assert!(vm_of(&vms.vms, &"plain".parse().unwrap()).is_none());
        let widget: Name = "widget".parse().unwrap();
        let want: Cell = serde_json::from_str(
            r#"{"name":"widget","grants":[{"edge":"codex"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":true}},{"code":{"repo":"o/r","write":false}}]}"#,
        )
        .unwrap();
        let (cell, doors) = seen.of(&widget).unwrap();
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
        assert!(json.contains(r#""doors":{"widget":["scratch"]}"#), "{json}");
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
