//! Restricted host signer over a Unix socket. The agent supplies ONLY a typed
//! command. Identity, audience, expiry, and request ID are fixed/generated here.
//! The service must run as the key-holding host user, never as the agent.
use super::{
    transport::{self, Command, SignedRequest},
    NamespaceId, PublishScope,
};
use crate::prim::Url;
use crate::{Cell, Grant, RequestId, Secret};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Operator-written configuration on the cell VM host.
pub const HOST_CONFIG: &str = "/etc/cell/publisher.json";
/// Bound read-only into the cell environment at the same path. It holds the
/// signer's socket and the public configuration the agent's client reads.
pub const SHARED_DIR: &str = "/run/cell-publish";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub namespace: NamespaceId,
    pub endpoint: Url,
    pub audience: RequestId,
}
// For key and declaration inputs owned by the trusted service/root. No agent
// paths are opened by the bridge. O_NOFOLLOW/O_NONBLOCK closes link/FIFO races.
fn trusted(path: &Path, secret: bool) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file()
        || (meta.uid() != 0 && meta.uid() != uid)
        || meta.mode() & 0o022 != 0
        || (secret && meta.mode() & 0o077 != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bridge input must be trusted, regular, and protected",
        ));
    }
    let mut bytes = vec![];
    file.take(transport::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > transport::MAX_REQUEST_BYTES {
        return Err(io::Error::other("bridge input too large"));
    }
    Ok(bytes)
}
pub struct Bridge {
    declaration: PathBuf,
    secret: Secret,
    audience: RequestId,
    cell: crate::CellId,
}
impl Bridge {
    pub fn load(declaration: &Path, secret: &Path, audience: RequestId) -> io::Result<Self> {
        let cell: Cell = serde_json::from_slice(&trusted(declaration, false)?)?;
        let key = String::from_utf8(trusted(secret, true)?)
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        Ok(Self {
            declaration: declaration.into(),
            secret: key,
            audience,
            cell: cell.id,
        })
    }
    pub fn load_config(path: &Path) -> io::Result<Config> {
        Ok(serde_json::from_slice(&trusted(path, false)?)?)
    }
    fn sign(&self, bytes: &[u8]) -> io::Result<SignedRequest> {
        let command: Command = serde_json::from_slice(bytes)?;
        // Reload the root-owned declaration for every request; local revocation
        // takes effect without a restart. Remote policy is checked at commit too.
        let cell: Cell = serde_json::from_slice(&trusted(&self.declaration, false)?)?;
        if cell.id != self.cell {
            return Err(io::Error::other("bridge cell identity changed"));
        }
        let needed = match &command {
            Command::ReserveSite { site } => PublishScope::Namespace(site.namespace.clone()),
            _ => PublishScope::Site(command.site().clone()),
        };
        if !cell
            .grants
            .iter()
            .any(|g| matches!(g, Grant::Publish(scope) if scope.covers(&needed)))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "no local publishing grant for this command",
            ));
        }
        Ok(SignedRequest::sign(
            &self.secret,
            self.audience,
            RequestId::generate(),
            transport::unix_time()? + transport::MAX_LIFETIME_SECS,
            command,
        ))
    }
}
fn frame_read(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > transport::MAX_REQUEST_BYTES {
        return Err(io::Error::other("bridge request too large"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn frame_write(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)
}
fn exchange(mut stream: UnixStream, bridge: &Bridge) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let result = frame_read(&mut stream)
        .and_then(|bytes| bridge.sign(&bytes))
        .map_err(|e| e.to_string());
    frame_write(&mut stream, &serde_json::to_vec(&result)?)
}
/// Client opens a Unix socket, not the secret file. Artifact contents never
/// enter this privileged process; only their signed digest does.
pub fn request(socket: &Path, command: &Command) -> io::Result<SignedRequest> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    frame_write(&mut stream, &serde_json::to_vec(command)?)?;
    let result: Result<SignedRequest, String> = serde_json::from_slice(&frame_read(&mut stream)?)?;
    result.map_err(io::Error::other)
}

pub async fn serve(
    config: Config,
    declaration: &Path,
    secret: &Path,
    agent_uid: u32,
) -> crate::apply::Fallible {
    if agent_uid == unsafe { libc::geteuid() } {
        return Err("bridge signer and agent must be different OS users".into());
    }
    let bridge = Arc::new(Bridge::load(declaration, secret, config.audience)?);
    let parent = config
        .socket
        .parent()
        .ok_or("bridge socket needs a parent directory")?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o022 != 0
        || (metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() })
    {
        return Err("bridge socket directory must be trusted and not agent-writable".into());
    }
    remove_stale_socket(&config.socket)?;
    let listener = tokio::net::UnixListener::bind(&config.socket)?;
    // Connections are authenticated by SO_PEERCRED, not filesystem group guesses.
    fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o666))?;
    advertise(parent, &config)?;
    run(listener, bridge, agent_uid).await
}
/// The socket directory outlives restarts because the cell environment binds
/// it. Remove a previous run's socket only when nothing answers on it: a live
/// socket may belong to another signer.
fn remove_stale_socket(socket: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(socket) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(
            "bridge socket path is not this signer's socket",
        ));
    }
    match UnixStream::connect(socket) {
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(socket),
        Ok(_) => Err(io::Error::other("another publishing bridge is listening")),
        Err(e) => Err(e),
    }
}
/// Public configuration beside the socket, so a client in the cell environment
/// can address this signer without reading host files.
fn advertise(directory: &Path, config: &Config) -> io::Result<()> {
    let temporary = directory.join(".publisher.json.new");
    fs::write(&temporary, serde_json::to_vec(config)?)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
    fs::rename(temporary, directory.join("publisher.json"))
}
async fn run(
    listener: tokio::net::UnixListener,
    bridge: Arc<Bridge>,
    agent_uid: u32,
) -> crate::apply::Fallible {
    let capacity = Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        let (stream, _) = listener.accept().await?;
        if stream.peer_cred()?.uid() != agent_uid {
            continue;
        }
        let Ok(permit) = capacity.clone().try_acquire_owned() else {
            continue;
        };
        let bridge = bridge.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let stream = stream.into_std()?;
            stream.set_nonblocking(false)?;
            exchange(stream, &bridge)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{registry::Registry, SiteId};
    use crate::{CellId, Hash};

    #[tokio::test]
    async fn unix_bridge_checks_peer_grants_and_never_accepts_signing_metadata() {
        let directory = std::env::temp_dir().join(format!("cell-bridge-{}", RequestId::generate()));
        fs::create_dir(&directory).unwrap();
        let registry = Registry::create(&directory.join("registry")).unwrap();
        let secret = Secret::generate();
        let namespace = registry.allocate_namespace(secret.public()).unwrap();
        let name: CellId = "489d239e-1b05-546d-a0ca-abd828de38b9".parse().unwrap();
        let cell = crate::fixtures::cell(
            name.clone(),
            [Grant::Publish(PublishScope::Namespace(namespace.clone()))],
        );
        let declaration = directory.join("cell.json");
        fs::write(&declaration, crate::canon::bytes(&cell)).unwrap();
        let key = directory.join("key");
        fs::write(&key, secret.to_string()).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        let bridge =
            Arc::new(Bridge::load(&declaration, &key, registry.audience().unwrap()).unwrap());
        let socket = directory.join("bridge.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server_bridge = bridge.clone();
        let task = tokio::spawn(async move {
            run(listener, server_bridge, unsafe { libc::geteuid() })
                .await
                .unwrap()
        });
        let site = SiteId {
            namespace,
            name: "demo".parse().unwrap(),
        };
        let command = Command::Publish {
            site: site.clone(),
            artifact: Hash::of(b"artifact"),
        };
        let socket2 = socket.clone();
        let request = tokio::task::spawn_blocking(move || request(&socket2, &command))
            .await
            .unwrap()
            .unwrap();
        let verified = request.verify().unwrap();
        assert_eq!(verified.caller, secret.public());
        assert_eq!(verified.audience, registry.audience().unwrap());
        let wire = serde_json::json!({"publish":{"site":site,"artifact":Hash::of(b"artifact")},"caller":secret.public()});
        assert!(bridge.sign(&serde_json::to_vec(&wire).unwrap()).is_err());
        let foreign = Command::Status {
            site: SiteId {
                namespace: NamespaceId(Hash::of(b"foreign")),
                name: "demo".parse().unwrap(),
            },
        };
        assert!(bridge.sign(&crate::canon::bytes(&foreign)).is_err());
        // Rereads local policy rather than retaining startup privileges.
        fs::write(
            &declaration,
            crate::canon::bytes(&crate::fixtures::cell(name, [])),
        )
        .unwrap();
        assert!(bridge
            .sign(&crate::canon::bytes(&Command::Status { site }))
            .is_err());
        task.abort();
        let denied_socket = directory.join("wrong-peer.sock");
        let listener = tokio::net::UnixListener::bind(&denied_socket).unwrap();
        let task = tokio::spawn(async move {
            run(listener, bridge, unsafe { libc::geteuid() } + 1)
                .await
                .unwrap()
        });
        let failed = tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(denied_socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let _ = frame_write(&mut stream, b"{}");
            frame_read(&mut stream)
        })
        .await
        .unwrap();
        assert!(failed.is_err());
        task.abort();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restart_replaces_only_a_dead_socket_and_advertises_public_config() {
        let directory = std::env::temp_dir().join(format!("cell-stale-{}", RequestId::generate()));
        fs::create_dir(&directory).unwrap();
        let socket = directory.join("bridge.sock");
        remove_stale_socket(&socket).unwrap();
        let live = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(remove_stale_socket(&socket).is_err());
        drop(live);
        assert!(socket.exists());
        remove_stale_socket(&socket).unwrap();
        assert!(!socket.exists());
        fs::write(&socket, "not 904254dc-1b8f-5e80-9755-e5afc5b195c5 socket").unwrap();
        assert!(remove_stale_socket(&socket).is_err());
        let config = Config {
            socket,
            namespace: NamespaceId(crate::Hash::of(b"namespace")),
            endpoint: "https://publisher.example".parse().unwrap(),
            audience: RequestId::generate(),
        };
        advertise(&directory, &config).unwrap();
        let advertised = directory.join("publisher.json");
        assert_eq!(
            fs::metadata(&advertised).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let read: Config = serde_json::from_slice(&fs::read(advertised).unwrap()).unwrap();
        assert_eq!(read.namespace, config.namespace);
        assert_eq!(read.audience, config.audience);
        fs::remove_dir_all(directory).unwrap();
    }
}
