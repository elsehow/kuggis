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
    if args == ["capabilities"] {
        let discovery = match crate::context::load() {
            Ok(context) => context.publishing,
            Err(_) => None,
        };
        let discovery = discovery.unwrap_or_else(|| {
            match fs::read(&config_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
            {
                Some(config) => super::provision::Discovery {
                    config: Some(config),
                    sites: vec![],
                    error: None,
                },
                None => super::provision::Discovery::unavailable(
                    "Website publishing is not configured; ask the operator to connect a publisher",
                ),
            }
        });
        println!("{}", serde_json::to_string_pretty(&discovery)?);
        return Ok(());
    }
    let mut visibility = super::Visibility::Private;
    let mut explicit_visibility = false;
    if let Some(at) = args.iter().position(|a| a == "--visibility") {
        if at + 1 >= args.len() {
            return Err("--visibility requires private or public".into());
        }
        visibility = match args.remove(at + 1).as_str() {
            "private" => super::Visibility::Private,
            "public" => super::Visibility::Public,
            _ => return Err("--visibility requires private or public".into()),
        };
        args.remove(at);
        explicit_visibility = true;
    }
    let bytes = fs::read(&config_path).map_err(|e| {
        format!("publishing is not configured for this cell ({config_path}: {e}); the operator must configure its publisher bridge")
    })?;
    let mut config: Config = serde_json::from_slice(&bytes)?;
    if let Some(at) = args.iter().position(|a| a == "--namespace") {
        if at + 1 >= args.len() {
            return Err("--namespace requires a namespace ID".into());
        }
        config.namespace = super::NamespaceId(args.remove(at + 1).parse()?);
        args.remove(at);
    }
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
        ["verify", name] => Command::Verify { site: site(name)? },
        ["status", name] => Command::Status { site: site(name)? },
        ["unpublish", name] => Command::Unpublish { site: site(name)? },
        ["rollback", name, version] => Command::Rollback { site: site(name)?, version: version.parse()? },
        [directory, "--site", name] => {
            let packed = Artifact::snapshot(Path::new(directory))?;
            let command = match visibility {
                super::Visibility::Private => Command::PublishPrivate { site: site(name)?, artifact: packed.hash()? },
                super::Visibility::Public => Command::Publish { site: site(name)?, artifact: packed.hash()? },
            };
            artifact = Some(packed);
            command
        }
        _ => return Err("usage: cell publish DIR --site NAME [--visibility private|public] | capabilities | verify NAME | status NAME | rollback NAME VERSION | unpublish NAME [--config FILE]".into()),
    };
    if explicit_visibility && artifact.is_none() {
        return Err("--visibility only applies when publishing a directory".into());
    }
    let reply = send(&config, &command, artifact)?;
    let mut output = serde_json::to_value(&reply)?;
    let error = match &reply {
        Reply::Published(receipt) | Reply::RolledBack(receipt) => {
            match send(
                &config,
                &Command::Verify {
                    site: receipt.site.clone(),
                },
                None,
            ) {
                Ok(Reply::Verified(verification)) if verification.publication == *receipt => {
                    output["verification"] = serde_json::to_value(&verification)?;
                    verification.error
                }
                Ok(_) => {
                    Some("Site changed before verification; run cell publish verify NAME".into())
                }
                Err(error) => Some(format!(
                    "Published, but external verification failed: {error}"
                )),
            }
        }
        Reply::Verified(verification) => verification.error.clone(),
        _ => None,
    };
    if let Some(error) = &error {
        output["verification_error"] = serde_json::json!(error);
    }
    writeln!(io::stdout(), "{}", serde_json::to_string_pretty(&output)?)?;
    if let Some(error) = error {
        return Err(format!("{error}. Do not share this as a verified website; repair hosting and run cell publish verify NAME").into());
    }
    Ok(())
}
fn send(
    config: &Config,
    command: &Command,
    artifact: Option<Artifact>,
) -> crate::apply::Fallible<Reply> {
    let request = bridge::request(&config.socket, command).map_err(|e| match e.kind() {
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
    let matches = match (command, &reply) {
        (Command::Verify { site }, Reply::Verified(verification)) => {
            &verification.publication.site == site
        }
        (
            Command::Publish { site, artifact } | Command::PublishPrivate { site, artifact },
            Reply::Published(receipt),
        ) => {
            let visibility = if matches!(command, Command::PublishPrivate { .. }) {
                super::Visibility::Private
            } else {
                super::Visibility::Public
            };
            receipt.visibility == visibility
                && &receipt.site == site
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
    Ok(reply)
}
