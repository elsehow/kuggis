//! Private preview selection is pure; the client owns the SSH tunnel lifetime.
use crate::{config::Name, ssh::Invocation, Error, Result, Workspace};
use serde::Serialize;
use std::{
    io::{BufRead, BufReader, Read, Write},
    num::NonZeroU16,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[derive(Debug, Default)]
pub struct Options {
    pub name: Option<Name>,
    pub port: Option<NonZeroU16>,
    pub no_open: bool,
    pub plan: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--no-open" if !options.no_open => options.no_open = true,
                "--plan" if !options.plan => options.plan = true,
                "--port" if options.port.is_none() => {
                    options.port =
                        Some(args.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                            Error("--port requires a port between 1 and 65535".into())
                        })?);
                }
                value if !value.starts_with('-') && options.name.is_none() => {
                    options.name = Some(Name::try_from(value.to_owned())?);
                }
                _ => return Err(Error(format!("unexpected web argument: {arg}"))),
            }
        }
        Ok(options)
    }
}

#[derive(Debug, Serialize)]
pub struct Preview {
    pub name: Name,
    pub guest_port: NonZeroU16,
    pub host_port: NonZeroU16,
    pub local_port: NonZeroU16,
    pub url: String,
    pub ssh: Invocation,
}

impl Preview {
    pub fn select(workspace: &Workspace, options: &Options) -> Result<Self> {
        let mappings = workspace.private_web();
        let web =
            if let Some(name) = &options.name {
                mappings
                    .iter()
                    .find(|web| &web.name == name)
                    .ok_or_else(|| Error(format!("no private preview named {}", name.as_str())))?
            } else {
                match mappings {
                    [web] => web,
                    [] => return Err(Error(
                        "no private preview configured; add a private_web mapping and run setup"
                            .into(),
                    )),
                    _ => {
                        return Err(Error(format!(
                            "choose a preview: {}",
                            mappings
                                .iter()
                                .map(|web| web.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            };
        let local_port = options.port.unwrap_or(web.host_port);
        Ok(Self {
            name: web.name.clone(),
            guest_port: web.guest_port,
            host_port: web.host_port,
            local_port,
            url: format!("http://127.0.0.1:{local_port}/"),
            ssh: Invocation::for_web(workspace, web.host_port.get(), local_port.get()),
        })
    }
}

struct Tunnel(Child);
impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(workspace: &Workspace, options: &Options) -> Result<()> {
    let preview = Preview::select(workspace, options)?;
    if options.plan {
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(());
    }
    let mut tunnel = Tunnel(
        preview
            .ssh
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    // Child::wait closes stdin automatically. Own the pipe separately so the
    // remote keepalive lasts until this command exits, even with caller stdin EOF.
    let _keepalive = tunnel.0.stdin.take();
    let stdout = tunnel
        .0
        .stdout
        .take()
        .ok_or_else(|| Error("missing SSH readiness stream".into()))?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).take(1024).read_line(&mut line);
        let _ = send.send(result.is_ok() && line == "kuggis-web-ready\n");
    });
    if receive.recv_timeout(Duration::from_secs(30)) != Ok(true) {
        return Err(Error(format!(
            "preview tunnel did not start; check SSH errors above. If local port {} is occupied, use --port OTHER_PORT", preview.local_port)));
    }
    println!("{}", preview.url);
    std::io::stdout().flush()?;
    eprintln!(
        "Private preview '{}' → guest port {}. Keep this command running; Ctrl-C closes the tunnel.",
        preview.name.as_str(),
        preview.guest_port
    );
    if !options.no_open {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        match Command::new(opener)
            .arg(&preview.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            _ => eprintln!(
                "Could not open a browser; open {} manually (or use --no-open).",
                preview.url
            ),
        }
    }
    let status = tunnel.0.wait()?;
    if !status.success() {
        return Err(Error(format!("preview tunnel closed ({status})")));
    }
    Ok(())
}
