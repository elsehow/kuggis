//! Operator-side input and pure compilation. Runtime agents remain ordinary programs.
use crate::{config::Name, Error, Result, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Repository(String);

impl TryFrom<String> for Repository {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        let parts: Vec<_> = value.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || p.len() > 100
                    || p.starts_with('.')
                    || p.starts_with('-')
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            || value.ends_with(".git")
        {
            return Err(Error(
                "repo: expected OWNER/REPO, without a URL or .git suffix".into(),
            ));
        }
        Ok(Self(value))
    }
}
impl From<Repository> for String {
    fn from(value: Repository) -> String {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    #[default]
    Read,
    Write,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    #[default]
    Exe,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    #[default]
    Pi,
    Codex,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Model {
    #[default]
    ExeManaged,
}

fn version() -> u8 {
    1
}
fn cpus() -> u16 {
    2
}
fn memory() -> u16 {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    #[serde(default = "version")]
    pub version: u8,
    pub name: Name,
    pub repo: Repository,
    #[serde(default)]
    pub github_access: Access,
    #[serde(default)]
    pub host: Host,
    #[serde(default, alias = "default_agent")]
    pub agent: Agent,
    #[serde(default)]
    pub model: Model,
    #[serde(default = "cpus")]
    pub cpus: u16,
    #[serde(default = "memory")]
    pub memory_gib: u16,
}

impl CreateRequest {
    pub fn parse(value: Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value)?;
        if request.version != 1 {
            return Err(Error("version: expected setup version 1".into()));
        }
        if request.name.as_str().len() > 40 {
            return Err(Error("name: at most 40 characters for exe creation".into()));
        }
        if !(1..=64).contains(&request.cpus) || !(4..=256).contains(&request.memory_gib) {
            return Err(Error(
                "capacity: cpus must be 1..64 and memory_gib 4..256".into(),
            ));
        }
        Ok(request)
    }

    pub fn vm(&self) -> String {
        format!("kuggis-{}", self.name.as_str())
    }

    /// No provider calls, files or subprocesses: all output is validated by the core schema.
    pub fn workspace(&self, ssh_config: &Path) -> Result<Workspace> {
        let mut resources = json!([
            {"name":"ubuntu-packages", "endpoints":[
                {"host":"archive.ubuntu.com","port":443},
                {"host":"ports.ubuntu.com","port":443}
            ]},
            {"name":"github", "endpoints":[
                {"host":"github.com","port":443},
                {"host":"api.github.com","port":443}
            ]}
        ]);
        let models: Value =
            serde_json::from_str(include_str!("../../../recipes/pi/exe-resource.json"))?;
        resources.as_array_mut().unwrap().push(models);
        let agents = json!({
            "pi": {
                "launch":["/home/agent/.local/bin/pi","--offline","--continue"],
                "chat_launch":["/home/agent/.local/bin/pi","--offline","--session-dir","/home/agent/.kuggis/chats/{chat}","--continue"]
            },
            "codex": {
                "launch":["/home/agent/.local/bin/kuggis-codex","default"],
                "chat_launch":["/home/agent/.local/bin/kuggis-codex","{chat}"]
            }
        });
        let agent = match self.agent {
            Agent::Pi => "pi",
            Agent::Codex => "codex",
        };
        let launch = &agents[agent]["launch"];
        let chat_launch = &agents[agent]["chat_launch"];
        Workspace::parse(
            &json!({
                "version":2, "name":self.name, "ssh":format!("{}.exe.xyz",self.vm()),
                "ssh_config":ssh_config,
                "launch":launch, "chat_launch":chat_launch,
                "default_agent":agent,
                "agents":agents,
                "memory_bytes":(u64::from(self.memory_gib)-2)*1024*1024*1024,
                "packages":["fd-find","ripgrep","gh"], "resources":resources,
                "private_web":[{"name":"preview","guest_port":3000,"host_port":8000}]
            })
            .to_string(),
        )
    }
}

pub fn state_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("KUGGIS_STATE_DIR").filter(|v| !v.is_empty()) {
        return absolute(Path::new(&root));
    }
    default_state_root(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn default_state_root(config_home: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    let config = config_home
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_owned)
        .or_else(|| {
            home.map(Path::new)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .ok_or_else(|| {
            Error("cannot locate workspace state: set KUGGIS_STATE_DIR, XDG_CONFIG_HOME, or HOME to an absolute directory".into())
        })?;
    Ok(config.join("kuggis/workspaces"))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub fn config_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.is_file() || value.contains('/') || value.ends_with(".json") {
        return Ok(path.to_owned());
    }
    let name = Name::try_from(value.to_owned())?;
    let name = name.as_str();
    let config = state_root()?.join(name).join("workspace.json");
    match config.try_exists() {
        Ok(true) => Ok(config),
        Ok(false) => Err(Error(format!(
            "workspace '{name}' not found; looked for {}. Set KUGGIS_STATE_DIR to an existing workspace directory or pass a workspace.json path explicitly",
            config.display()
        ))),
        Err(error) => Err(Error(format!("cannot access workspace '{name}' at {}: {error}", config.display()))),
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error("setup file exceeds 1 MiB".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn ask(label: &str, default: &str) -> Result<String> {
    eprint!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    std::io::stderr().flush()?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Err(Error("setup input ended".into()));
    }
    let answer = answer.trim();
    Ok(if answer.is_empty() {
        default.to_owned()
    } else {
        answer.to_owned()
    })
}

pub fn create(args: &[String]) -> Result<()> {
    let mut fields = serde_json::Map::new();
    let (mut file, mut ssh_config, mut binary) = (None, None, None);
    let (mut json_output, mut plan, mut restart) = (false, false, false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--plan" => plan = true,
            "--restart" => restart = true,
            "--file" | "--ssh-config" | "--bin" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error(format!("missing value for {arg}")))?;
                let target = match arg.as_str() {
                    "--file" => &mut file,
                    "--ssh-config" => &mut ssh_config,
                    _ => &mut binary,
                };
                if target.replace(absolute(Path::new(value))?).is_some() {
                    return Err(Error(format!("duplicate {arg}")));
                }
            }
            "--repo" | "--host" | "--agent" | "--default-agent" | "--model" | "--github-access"
            | "--cpus" | "--memory-gib" => {
                let key = if arg == "--default-agent" {
                    "agent".to_owned()
                } else {
                    arg.trim_start_matches("--").replace('-', "_")
                };
                let value = args
                    .next()
                    .ok_or_else(|| Error(format!("missing value for {arg}")))?;
                let value = if key == "cpus" || key == "memory_gib" {
                    json!(value
                        .parse::<u16>()
                        .map_err(|_| Error(format!("{key}: expected integer")))?)
                } else {
                    json!(value)
                };
                if fields.insert(key, value).is_some() {
                    return Err(Error(format!("duplicate {arg}")));
                }
            }
            name if !name.starts_with('-') && !fields.contains_key("name") => {
                fields.insert("name".into(), json!(name));
            }
            _ => return Err(Error(format!("unknown create argument: {arg}"))),
        }
    }
    if let Some(path) = file {
        if !fields.is_empty() {
            return Err(Error(
                "--file cannot be combined with setup field arguments".into(),
            ));
        }
        fields = read_json(&path)?
            .as_object()
            .cloned()
            .ok_or_else(|| Error("setup must be an object".into()))?;
    } else if let Some(name) = fields.get("name").and_then(Value::as_str) {
        let name = Name::try_from(name.to_owned())?;
        let saved = state_root()?.join(name.as_str()).join("setup.json");
        if saved.is_file() {
            let mut old = read_json(&saved)?
                .as_object()
                .cloned()
                .ok_or_else(|| Error("invalid saved setup".into()))?;
            old.extend(fields);
            fields = old;
        }
    }
    let interactive = std::io::stdin().is_terminal() && !json_output;
    if interactive {
        eprintln!("Create a workspace on exe.dev with managed models.");
        for (key, label, default) in [
            ("name", "Workspace name", ""),
            ("repo", "GitHub repository (OWNER/REPO)", ""),
            ("github_access", "Repository access (read/write)", "read"),
        ] {
            if !fields.contains_key(key) {
                fields.insert(key.into(), json!(ask(label, default)?));
            }
        }
    }
    for field in ["name", "repo"] {
        if !fields.contains_key(field) {
            return Err(Error(format!("missing field: {field}")));
        }
    }
    let request = CreateRequest::parse(Value::Object(fields))?;
    let directory = state_root()?.join(request.name.as_str());
    let workspace = request.workspace(&directory.join("ssh-config"))?;
    if plan {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"status":"configuration_only","request":request,"vm":request.vm(),"workspace":workspace})
            )?
        );
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let binary = binary.unwrap_or_else(|| {
        let beside = exe.parent().unwrap().join("kuggis-linux-x86_64");
        if beside.is_file() {
            beside
        } else {
            exe.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("x86_64-unknown-linux-musl/release/kuggis")
        }
    });
    let recipes = json!({
        "pi": {
            "install.py":include_str!("../../../recipes/pi/install.py"),
            "release.json":include_str!("../../../recipes/pi/release.json"),
            "settings.json":include_str!("../../../recipes/pi/settings.json"),
            "models.json":include_str!("../../../recipes/pi/exe-models.json"),
            "smoke.py":include_str!("../../../recipes/pi/smoke.py")
        },
        "codex": {
            "install.py":include_str!("../../../recipes/codex/install.py"),
            "release.json":include_str!("../../../recipes/codex/release.json"),
            "config.toml":include_str!("../../../recipes/codex/config.toml"),
            "models.json":include_str!("../../../recipes/codex/models.json"),
            "launch.py":include_str!("../../../recipes/codex/launch.py"),
            "smoke.py":include_str!("../../../recipes/codex/smoke.py")
        }
    });
    let payload = json!({"request":request,"workspace":workspace,"directory":directory,
    "binary":binary,"cli":exe,"ssh_config":ssh_config,"json_output":json_output,
    "host_executor":crate::deploy::HOST_EXECUTOR,"recipes":recipes,"allow_restart":restart});
    let mut child = Command::new("python3")
        .args(["-c", include_str!("../runtime/onboard.py")])
        .stdin(Stdio::piped())
        .spawn()?;
    serde_json::to_writer(
        child
            .stdin
            .take()
            .ok_or_else(|| Error("missing setup input".into()))?,
        &payload,
    )?;
    let status = child.wait()?;
    // The coordinator owns effect diagnostics and the final JSON report.
    if !status.success() {
        std::process::exit(status.code().unwrap_or(2));
    }
    Ok(())
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn default_state_is_in_an_absolute_user_configuration_directory() {
        let home = Some(OsStr::new("/users/operator"));
        for xdg in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
            assert_eq!(
                default_state_root(xdg, home).unwrap(),
                Path::new("/users/operator/.config/kuggis/workspaces")
            );
        }
        assert_eq!(
            default_state_root(Some(OsStr::new("/settings")), home).unwrap(),
            Path::new("/settings/kuggis/workspaces")
        );
        assert!(default_state_root(None, None).is_err());
        assert!(default_state_root(None, Some(OsStr::new("relative"))).is_err());
    }
}
