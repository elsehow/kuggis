//! Agent-side CLI. Reads only the selected artifact and public configuration;
//! the host key is never opened. HTTPS uses the cell's configured proxy.
use super::{
    artifact::Artifact,
    bridge::{self, Config},
    transport::{Command, Reply, Upload},
    SiteId,
};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::{Command as Process, Stdio},
};

/// Written by the host's bridge into its shared directory (`bridge::SHARED_DIR`).
pub const DEFAULT_CONFIG: &str = "/run/cell-publish/publisher.json";
pub fn run(args: &[String]) -> crate::apply::Fallible {
    let mut args = args.to_vec();
    let config_path = if let Some(at) = args.iter().position(|a| a == "--config") {
        if at + 1 >= args.len() {
            return Err("--config requires a path".into());
        }
        let path = args.remove(at + 1);
        args.remove(at);
        path
    } else {
        DEFAULT_CONFIG.into()
    };
    let bytes = fs::read(&config_path).map_err(|e| {
        format!("publishing is not configured for this cell ({config_path}: {e}); the operator must configure its publisher bridge")
    })?;
    let config: Config = serde_json::from_slice(&bytes)?;
    let rest = &config.endpoint.as_str()["https://".len()..];
    if rest.trim_end_matches('/').contains(['/', '?', '#']) {
        return Err("publisher endpoint must be an HTTPS origin".into());
    }
    let site = |name: &str| -> crate::apply::Fallible<SiteId> {
        Ok(SiteId {
            namespace: config.namespace.clone(),
            name: name.parse()?,
        })
    };
    let mut artifact = None;
    let command = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["status", name] => Command::Status { site: site(name)? },
        ["unpublish", name] => Command::Unpublish { site: site(name)? },
        ["rollback", name, version] => Command::Rollback { site: site(name)?, version: version.parse()? },
        [directory, "--site", name] => {
            let packed = Artifact::snapshot(Path::new(directory))?;
            let command = Command::Publish { site: site(name)?, artifact: packed.hash()? };
            artifact = Some(packed);
            command
        }
        _ => return Err("usage: cell publish DIR --site NAME | status NAME | rollback NAME VERSION | unpublish NAME [--config FILE]".into()),
    };
    let request = bridge::request(&config.socket, &command).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            format!("publisher bridge is not running ({}): {e}", config.socket.display())
        }
        // The bridge closes connections from any user but the cell's agent.
        io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => {
            format!("publisher bridge refused this caller; run cell publish as the agent user, not root ({e})")
        }
        _ => format!("publisher bridge: {e}"),
    })?;
    let verified = request.verify()?;
    let (route, body) = if let Some(artifact) = artifact {
        (
            "uploads",
            serde_json::to_vec(&Upload { request, artifact })?,
        )
    } else {
        ("requests", serde_json::to_vec(&request)?)
    };
    let url = format!(
        "{}/v1/{route}",
        config.endpoint.as_str().trim_end_matches('/')
    );
    let mut process = Process::new("curl")
        .args([
            "--disable",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "--max-filesize",
            "65536",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = process
        .stdin
        .take()
        .ok_or("curl stdin unavailable")?
        .write_all(&body);
    let output = process.wait_with_output()?;
    written?;
    if !output.status.success() {
        return Err(format!(
            "publication failed ({}): {} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let reply: Reply = serde_json::from_slice(&output.stdout)?;
    let matches = match (&command, &reply) {
        (Command::Publish { site, artifact }, Reply::Published(receipt)) => {
            &receipt.site == site
                && &receipt.artifact == artifact
                && receipt.publisher == verified.caller
                && receipt.version == verified.request
        }
        (Command::Rollback { site, version }, Reply::RolledBack(receipt)) => {
            &receipt.site == site && &receipt.version == version
        }
        (Command::Unpublish { .. }, Reply::Unpublished) => true,
        (
            Command::Status { site },
            Reply::Status {
                site: returned,
                active,
                versions,
            },
        ) => {
            returned == site
                && versions.iter().all(|r| &r.site == site)
                && active.as_ref().is_none_or(|r| versions.contains(r))
        }
        _ => false,
    };
    if !matches {
        return Err("publisher returned a reply that does not match the request".into());
    }
    // Machine-readable output includes the stable public URL and rollback ID.
    writeln!(io::stdout(), "{}", serde_json::to_string_pretty(&reply)?)?;
    Ok(())
}
