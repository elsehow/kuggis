//! Typed SSH requests and process arguments. The host helper owns guest entry.
//!
//! The helper reads launch commands and resources from host-owned config,
//! enters the guest namespaces, then drops to the agent user before touching any
//! guest file or program. Never evaluate guest paths or launch argv as host root.
use crate::{
    config::{GuestPath, Name, Workspace},
    environment::HOST_HELPER,
};
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone)]
pub enum Request {
    Chats,
    Chat(Name),
    ArchiveChat(Name),
    SelectChat {
        chat: Name,
        agent: Name,
    },
    Shell,
    Check,
    Exec(Vec<String>),
    Put {
        path: GuestPath,
        replace: bool,
    },
    PutInChat {
        path: GuestPath,
        replace: bool,
        chat: Name,
    },
    Get {
        path: GuestPath,
    },
}

#[derive(Debug, Serialize)]
pub struct Invocation {
    pub program: &'static str,
    /// Exact process arguments, not a shell command to concatenate locally.
    pub args: Vec<String>,
    pub stdin: Stream,
    pub stdout: Stream,
    pub requires_installed_host_helper: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Terminal,
    Closed,
    KeepAlivePipe,
    FileFrame,
    InstallFrame,
    JsonReport,
    ControlReply,
    Empty,
}

impl Invocation {
    pub fn for_management(workspace: &Workspace) -> Self {
        Self::remote(
            workspace,
            vec![
                "sudo".into(),
                "-n".into(),
                "--".into(),
                "/usr/bin/python3".into(),
                "-c".into(),
                crate::management::HOST.into(),
            ],
            false,
            Stream::InstallFrame,
            Stream::JsonReport,
            true,
        )
    }
    pub fn for_github(workspace: &Workspace, action: &str) -> Self {
        Self::remote(
            workspace,
            vec![
                "sudo".into(),
                "-n".into(),
                "--".into(),
                HOST_HELPER.into(),
                "github".into(),
                workspace.name().as_str().into(),
                action.into(),
            ],
            false,
            Stream::InstallFrame,
            Stream::JsonReport,
            true,
        )
    }
    /// Forwarding is established before SSH starts this fixed readiness command.
    /// Keeping stdin open holds the session; closing the client releases it.
    pub fn for_web(workspace: &Workspace, host_port: u16, local_port: u16) -> Self {
        let mut invocation = Self::remote(
            workspace,
            vec![
                "sh".into(),
                "-c".into(),
                "printf 'kuggis-web-ready\\n'; exec cat >/dev/null".into(),
            ],
            false,
            Stream::KeepAlivePipe,
            Stream::ControlReply,
            false,
        );
        for arg in &mut invocation.args {
            if arg == "ClearAllForwardings=yes" {
                *arg = "ClearAllForwardings=no".into();
            }
        }
        invocation.args.splice(
            0..0,
            [
                "-o".into(),
                "ExitOnForwardFailure=yes".into(),
                "-o".into(),
                "SessionType=default".into(),
                "-o".into(),
                "StdinNull=no".into(),
                "-o".into(),
                "ForkAfterAuthentication=no".into(),
                "-L".into(),
                format!("127.0.0.1:{local_port}:127.0.0.1:{host_port}"),
            ],
        );
        invocation
    }

    pub fn exe_control(workspace: &Workspace, words: &[String]) -> Self {
        let mut invocation = Self::remote(
            workspace,
            words.to_vec(),
            false,
            Stream::Closed,
            Stream::ControlReply,
            false,
        );
        let target = invocation.args.len() - 2;
        invocation.args[target] = "exe.dev".into();
        invocation
    }
    pub fn for_request(workspace: &Workspace, request: &Request) -> Self {
        let mut remote = vec![
            "sudo".to_owned(),
            "-n".into(),
            "--".into(),
            HOST_HELPER.into(),
        ];
        let (verb, terminal, stdin, stdout) = match request {
            Request::Chat(_) | Request::SelectChat { .. } => {
                ("connect", true, Stream::Terminal, Stream::Terminal)
            }
            Request::Chats => ("chats", false, Stream::Closed, Stream::JsonReport),
            Request::ArchiveChat(_) => ("archive", false, Stream::Closed, Stream::Empty),
            Request::Shell => ("shell", true, Stream::Terminal, Stream::Terminal),
            Request::Check => ("check", false, Stream::Closed, Stream::JsonReport),
            Request::Exec(_) => ("exec", false, Stream::Terminal, Stream::Terminal),
            Request::Put { .. } | Request::PutInChat { .. } => {
                ("put", false, Stream::FileFrame, Stream::Empty)
            }
            Request::Get { .. } => ("get", false, Stream::Closed, Stream::FileFrame),
        };
        remote.extend([verb.to_owned(), workspace.name().as_str().to_owned()]);
        match request {
            Request::SelectChat { chat, agent } => {
                remote.extend([
                    "--chat".into(),
                    chat.as_str().into(),
                    "--agent".into(),
                    agent.as_str().into(),
                ]);
            }
            Request::Chat(name) | Request::ArchiveChat(name) => {
                remote.extend(["--chat".into(), name.as_str().into()]);
            }
            Request::Put { path, replace } => {
                if *replace {
                    remote.push("--replace".into());
                }
                remote.extend(["--".into(), path.as_str().to_owned()]);
            }
            Request::PutInChat {
                path,
                replace,
                chat,
            } => {
                remote.extend(["--chat".into(), chat.as_str().into()]);
                if *replace {
                    remote.push("--replace".into());
                }
                remote.extend(["--".into(), path.as_str().to_owned()]);
            }
            Request::Get { path } => remote.extend(["--".into(), path.as_str().to_owned()]),
            Request::Exec(argv) => {
                remote.push("--".into());
                remote.extend(argv.iter().cloned());
            }
            _ => {}
        }
        Self::remote(workspace, remote, terminal, stdin, stdout, true)
    }

    pub fn for_install(workspace: &Workspace) -> Self {
        Self::remote(
            workspace,
            vec![
                "sudo".into(),
                "-n".into(),
                "--".into(),
                "/usr/bin/python3".into(),
                "-c".into(),
                crate::deploy::HOST_EXECUTOR.into(),
                "install".into(),
            ],
            false,
            Stream::InstallFrame,
            Stream::JsonReport,
            false,
        )
    }

    fn remote(
        workspace: &Workspace,
        remote: Vec<String>,
        terminal: bool,
        stdin: Stream,
        stdout: Stream,
        requires_installed_host_helper: bool,
    ) -> Self {
        let mut args: Vec<String> = [
            if terminal { "-tt" } else { "-T" },
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if let Some(config) = workspace.ssh_config() {
            args.extend(["-F".into(), config.as_str().into()]);
        }
        args.push(workspace.ssh().as_str().to_owned());
        // OpenSSH sends a command string to the remote shell, even when invoked
        // with a local argv. Quote every word, including file paths containing '.
        args.push(
            remote
                .iter()
                .map(|word| quote(word))
                .collect::<Vec<_>>()
                .join(" "),
        );
        Self {
            program: "ssh",
            args,
            stdin,
            stdout,
            requires_installed_host_helper,
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(self.program);
        command.args(&self.args);
        command
    }
}

pub(crate) fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}
