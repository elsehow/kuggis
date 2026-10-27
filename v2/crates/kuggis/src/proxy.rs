//! Bounded CONNECT transport. The pure Policy grants a concrete resolved target;
//! connections use those checked IPs directly, with no second DNS lookup.
use crate::{
    config::{Host, Workspace},
    deploy,
    network::Policy,
    Error, Result,
};
use std::{
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub host: Host,
    pub port: u16,
}

impl Target {
    pub fn parse(line: &str) -> Result<Self> {
        let words: Vec<_> = line.split(' ').collect();
        if words.len() != 3
            || words[0] != "CONNECT"
            || !["HTTP/1.0", "HTTP/1.1"].contains(&words[2])
        {
            return Err(Error("expected CONNECT host:port HTTP/1.1".into()));
        }
        let (name, port) = words[1]
            .rsplit_once(':')
            .ok_or_else(|| Error("missing port".into()))?;
        let name = if name.starts_with('[') && name.ends_with(']') {
            let inner = &name[1..name.len() - 1];
            inner
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| Error("brackets require an IPv6 literal".into()))?;
            inner
        } else {
            if name.contains(':') {
                return Err(Error("IPv6 literals require brackets".into()));
            }
            name
        };
        let name = name
            .parse::<IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| name.to_ascii_lowercase());
        let host = Host::try_from(name)?;
        let parsed: u16 = port.parse().map_err(|_| Error("invalid port".into()))?;
        if parsed == 0 || port != parsed.to_string() {
            return Err(Error("invalid port".into()));
        }
        Ok(Self { host, port: parsed })
    }
}

pub fn serve(workspace: Workspace) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", deploy::PROXY_PORT))?;
    let workspace = Arc::new(workspace);
    let active = Arc::new(AtomicUsize::new(0));
    for client in listener.incoming() {
        let client = client?;
        if active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 64).then_some(n + 1)
            })
            .is_err()
        {
            refuse(client, 503);
            continue;
        }
        let workspace = Arc::clone(&workspace);
        let active = Arc::clone(&active);
        thread::spawn(move || {
            struct Slot(Arc<AtomicUsize>);
            impl Drop for Slot {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _slot = Slot(active);
            let _ = handle(client, &workspace, &resolve, &|address| {
                TcpStream::connect_timeout(address, Duration::from_secs(10))
            });
        });
    }
    Ok(())
}

fn resolve(host: &Host, port: u16) -> io::Result<Vec<SocketAddr>> {
    // Even a public address can belong to this host. Do not let a configured
    // public hostname or private pin turn the proxy into management access.
    let output = std::process::Command::new("ip")
        .args(["-j", "address", "show"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("cannot observe host addresses"));
    }
    let interfaces: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    let interfaces = interfaces
        .as_array()
        .ok_or_else(|| io::Error::other("invalid interface report"))?;
    let mut local = Vec::new();
    for interface in interfaces {
        let entries = interface
            .get("addr_info")
            .and_then(|v| v.as_array())
            .ok_or_else(|| io::Error::other("missing interface addresses"))?;
        for entry in entries {
            let ip = entry
                .get("local")
                .and_then(|v| v.as_str())
                .ok_or_else(|| io::Error::other("missing local address"))?;
            local.push(ip.parse::<IpAddr>().map_err(io::Error::other)?);
        }
    }
    let addresses: Vec<_> = (host.as_str(), port).to_socket_addrs()?.collect();
    if addresses.iter().any(|address| local.contains(&address.ip()) ||
        matches!(address.ip(), IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some_and(|ip| local.contains(&ip.into())))) {
        return Err(io::Error::other("host management addresses are not resource destinations"));
    }
    Ok(addresses)
}

/// Resolver and dialer are explicit effects so policy can be tested over local
/// sockets without weakening production address checks.
pub fn handle(
    mut client: TcpStream,
    workspace: &Workspace,
    resolve: &impl Fn(&Host, u16) -> io::Result<Vec<SocketAddr>>,
    dial: &impl Fn(&SocketAddr) -> io::Result<TcpStream>,
) -> Result<()> {
    client.set_write_timeout(Some(Duration::from_secs(10)))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut head = Vec::new();
    let end = loop {
        if let Some(end) = head.windows(4).position(|v| v == b"\r\n\r\n") {
            break end + 4;
        }
        if head.len() >= 8192 || Instant::now() >= deadline {
            refuse(client, 400);
            return Ok(());
        }
        client.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        let mut part = [0; 1024];
        let length = part.len().min(8192 - head.len());
        let n = match client.read(&mut part[..length]) {
            Ok(0) | Err(_) => {
                refuse(client, 400);
                return Ok(());
            }
            Ok(n) => n,
        };
        head.extend_from_slice(&part[..n]);
    };
    let target = std::str::from_utf8(&head[..end])
        .ok()
        .and_then(|head| head.split("\r\n").next())
        .and_then(|line| Target::parse(line).ok());
    let Some(target) = target else {
        refuse(client, 400);
        return Ok(());
    };
    let policy = Policy::new(workspace);
    if policy.endpoint(&target.host, target.port).is_none() {
        refuse(client, 403);
        return Ok(());
    }
    let addresses = match resolve(&target.host, target.port) {
        Ok(addresses) => addresses,
        Err(_) => {
            refuse(client, 502);
            return Ok(());
        }
    };
    let ips: Vec<IpAddr> = addresses.iter().map(SocketAddr::ip).collect();
    if addresses
        .iter()
        .any(|address| address.port() != target.port || deploy::Boundary::is_internal(address.ip()))
        || !policy.allows_resolved(&target.host, target.port, &ips)
    {
        refuse(client, 403);
        return Ok(());
    }
    let Some(mut remote) = addresses.iter().find_map(|address| dial(address).ok()) else {
        refuse(client, 502);
        return Ok(());
    };
    remote.set_write_timeout(Some(Duration::from_secs(30)))?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    remote.write_all(&head[end..])?;
    client.set_read_timeout(None)?;
    let mut from_client = client.try_clone()?;
    let mut to_remote = remote.try_clone()?;
    let upstream = thread::spawn(move || {
        let _ = io::copy(&mut from_client, &mut to_remote);
        let _ = to_remote.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut remote, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = upstream.join();
    Ok(())
}

fn refuse(mut client: TcpStream, status: u16) {
    let _ = client.write_all(
        format!("HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    );
    let _ = client.shutdown(Shutdown::Both);
}
