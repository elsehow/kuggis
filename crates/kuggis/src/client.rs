//! Local effects: stream an installation or file over an already authorized SSH
//! connection. Never read resource credentials or interpret guest output as code.
use crate::{
    config::{GuestPath, Ingress, Name, Workspace},
    deploy::Bundle,
    ssh::{Invocation, Request},
    transfer::{self, PendingFile},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Child, Stdio},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn success(status: std::process::ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!("remote operation failed ({status})")))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    pub name: Name,
    pub agent: Option<Name>,
    pub running: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatIndex {
    pub chats: Vec<Chat>,
    pub agents: Vec<Name>,
    pub default_agent: Option<Name>,
}

pub fn chats(workspace: &Workspace, target: &str, json: bool) -> Result<()> {
    let mut process = Process(
        Invocation::for_request(workspace, &Request::Chats)
            .command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let mut bytes = Vec::new();
    process
        .0
        .stdout
        .take()
        .ok_or_else(|| Error("missing chat list".into()))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error("chat list exceeds 1 MiB".into()));
    }
    success(process.0.wait()?)?;
    let index: ChatIndex = serde_json::from_slice(&bytes)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&index)?);
    } else {
        let target = if Name::try_from(target.to_owned()).is_ok() {
            target.to_owned()
        } else {
            crate::ssh::quote(target)
        };
        println!("Chats in {}:", target);
        if index.chats.is_empty() {
            println!("  No chats yet.");
        }
        for chat in &index.chats {
            println!(
                "  {:<24} {:<10} {}",
                chat.name.as_str(),
                chat.agent.as_ref().map(Name::as_str).unwrap_or("legacy"),
                if chat.running { "running" } else { "stopped" }
            );
        }
        println!("\nOpen a chat: kuggis chat open {} NAME", target);
        println!(
            "Create one:  kuggis chat open {} new-chat{}",
            target,
            index
                .default_agent
                .as_ref()
                .map(|agent| format!(" --agent {}", agent.as_str()))
                .unwrap_or_default()
        );
        if !index.agents.is_empty() {
            println!(
                "Available agents: {}",
                index
                    .agents
                    .iter()
                    .map(Name::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

pub fn enter(workspace: &Workspace, request: Request) -> Result<()> {
    if matches!(&request, Request::Chat(name) if name.as_str() != "default")
        && workspace.chat_launch().is_none()
        && workspace.agents().is_empty()
    {
        return Err(Error(
            "named chats require a chat_launch recipe; configure it and run setup first".into(),
        ));
    }
    if let Request::SelectChat { agent, .. } = &request {
        if !workspace.agents().contains_key(agent) {
            return Err(Error(format!(
                "agent {} is not configured in this workspace",
                agent.as_str()
            )));
        }
    }
    let mut command = Invocation::for_request(workspace, &request).command();
    if matches!(request, Request::Check | Request::ArchiveChat(_)) {
        command.stdin(Stdio::null());
    } else if std::env::var("TERM").as_deref() == Ok("xterm-ghostty") {
        // Ubuntu 24.04's base terminfo predates Ghostty. Use its documented
        // xterm compatibility mode for the SSH PTY, including direct shells.
        command.env("TERM", "xterm-256color");
    }
    success(command.status()?)
}

pub fn put(workspace: &Workspace, source: &Path, path: GuestPath, replace: bool) -> Result<()> {
    put_with_chat(workspace, source, path, replace, None)
}

pub fn put_with_chat(
    workspace: &Workspace,
    source: &Path,
    path: GuestPath,
    replace: bool,
    chat: Option<Name>,
) -> Result<()> {
    let mut source = File::open(source)?;
    let request = match chat {
        Some(chat) => Request::PutInChat {
            path,
            replace,
            chat,
        },
        None => Request::Put { path, replace },
    };
    let mut process = Process(
        Invocation::for_request(workspace, &request)
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?,
    );
    {
        let mut input = process
            .0
            .stdin
            .take()
            .ok_or_else(|| Error("missing upload stream".into()))?;
        transfer::send(&mut source, &mut input)?;
    }
    success(process.0.wait()?)
}

pub fn get(
    workspace: &Workspace,
    path: GuestPath,
    destination: &Path,
    replace: bool,
) -> Result<()> {
    let mut pending = PendingFile::new(destination)?;
    let mut process = Process(
        Invocation::for_request(workspace, &Request::Get { path })
            .command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let output = process
        .0
        .stdout
        .take()
        .ok_or_else(|| Error("missing download stream".into()))?;
    transfer::receive(&mut BufReader::new(output), &mut pending.file)?;
    success(process.0.wait()?)?;
    pending.commit(replace)
}

#[derive(Serialize)]
struct Installation<'a> {
    bundle: Bundle<'a>,
    binary_size: u64,
    sha256: String,
    allow_restart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_revision: Option<&'a str>,
}

pub fn setup(workspace: &Workspace, binary: &Path) -> Result<()> {
    setup_checked(workspace, binary, false, None)
}

pub fn setup_checked(
    workspace: &Workspace,
    binary: &Path,
    allow_restart: bool,
    expected_revision: Option<&str>,
) -> Result<()> {
    let mut binary = File::open(binary)?;
    let length = binary.metadata()?.len();
    if !binary.metadata()?.is_file() || length == 0 || length > 64 * 1024 * 1024 {
        return Err(Error("expected a Linux binary of at most 64 MiB".into()));
    }
    let mut magic = [0; 6];
    binary.read_exact(&mut magic)?;
    if magic != *b"\x7fELF\x02\x01" {
        return Err(Error(
            "setup requires a 64-bit little-endian Linux binary (--bin)".into(),
        ));
    }
    binary.seek(SeekFrom::Start(0))?;
    let request = Installation {
        bundle: Bundle::compile(workspace),
        binary_size: length,
        sha256: transfer::digest(&mut binary)?,
        allow_restart,
        expected_revision,
    };
    let header = serde_json::to_vec(&request)?;
    if header.len() >= 1024 * 1024 {
        return Err(Error("installation manifest exceeds 1 MiB".into()));
    }
    configure_ingress(workspace)?;
    binary.seek(SeekFrom::Start(0))?;
    let mut process = Process(
        Invocation::for_install(workspace)
            .command()
            .stdin(Stdio::piped())
            .spawn()?,
    );
    {
        let mut input = process
            .0
            .stdin
            .take()
            .ok_or_else(|| Error("missing installation stream".into()))?;
        input.write_all(&header)?;
        input.write_all(b"\n")?;
        if std::io::copy(&mut binary.take(length), &mut input)? != length {
            return Err(Error("installation binary changed during upload".into()));
        }
    }
    success(process.0.wait()?)
}

/// Explicitly private before any relay is installed or started. This is a
/// deployment operation, never an authority exposed to the guest agent.
fn configure_ingress(workspace: &Workspace) -> Result<()> {
    let Ingress::Exe { vm, web } = workspace.ingress() else {
        return Ok(());
    };
    let publication = workspace
        .private_web()
        .iter()
        .find(|entry| &entry.name == web)
        .ok_or_else(|| Error("missing validated ingress port".into()))?;
    for words in [
        vec!["share".into(), "set-private".into(), vm.as_str().into()],
        vec![
            "share".into(),
            "port".into(),
            vm.as_str().into(),
            publication.host_port.to_string(),
        ],
    ] {
        let output = Invocation::exe_control(workspace, &words)
            .command()
            .stdin(Stdio::null())
            .output()?;
        success(output.status)?;
        // exe can report structured validation errors with a successful SSH exit.
        if serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .is_some_and(|value| value.get("error").is_some_and(|error| !error.is_null()))
            || String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("error")
        {
            return Err(Error(
                "exe refused the private ingress configuration".into(),
            ));
        }
    }
    Ok(())
}
