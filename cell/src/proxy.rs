//! The wall's one road out: an HTTPS CONNECT proxy that tunnels to the
//! hosts a cell's grants allow and refuses everything else. Standard
//! library only. It sees hostnames, never content: TLS runs end to end
//! through the tunnel.
//!
//! The proxy runs as its own user; the firewall lets only that user out.
//! The agent user reaches it on loopback and has no DNS and no direct TCP.

use crate::cell::Cell;
use crate::exe::door_of_host;
use crate::grant::{Grant, EDGE_SUFFIX};
use crate::prim::Host;
use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Where exe.dev answers for its integrations from inside a VM. An edge
/// host must resolve there and nowhere else; every other host must
/// resolve somewhere public.
///
/// Doors are edge hosts the cell file never names: the tree decides which
/// children a cell may knock on, and the gateway serves a door only to the
/// VM it is attached to. So any door hostname passes here, and the gateway
/// answers "not attached to this VM" for the rest.
pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub hosts: BTreeSet<Host>,
    pub public_network: bool,
    /// The subset of `hosts` that are exe.dev's edge, expected at [`GATEWAY`].
    pub edge: BTreeSet<Host>,
    pub ports: BTreeSet<u16>,
    /// Skip the address rules. Tests need it to reach a target on the same
    /// machine. A cell never sets it.
    pub private_ok: bool,
}

impl Policy {
    pub fn for_cell(cell: &Cell) -> Policy {
        let edge = cell
            .grants
            .iter()
            .filter(|g| g.at_edge())
            .flat_map(Grant::hosts)
            .collect();
        Policy {
            hosts: cell.allowlist(),
            public_network: cell.public_network(),
            edge,
            ports: [443, 8443].into(),
            private_ok: false,
        }
    }

    /// An explicit host, a public-network destination, or a door.
    pub fn allows(&self, host: &Host) -> bool {
        self.hosts.contains(host)
            || door_of_host(host).is_some()
            || (self.public_network && !host.as_str().ends_with(EDGE_SUFFIX))
    }

    /// A host expected at the gateway.
    pub fn at_edge(&self, host: &Host) -> bool {
        self.edge.contains(host) || door_of_host(host).is_some()
    }

    /// May this allowed host be reached at these addresses?
    fn placed_well(&self, host: &Host, addrs: &[SocketAddr]) -> bool {
        if self.private_ok {
            return true;
        }
        if self.at_edge(host) {
            addrs.iter().all(|a| a.ip() == IpAddr::V4(GATEWAY))
        } else {
            addrs.iter().all(|a| is_public(a.ip()))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    /// Most bytes a request head may take before it is refused.
    pub header_bytes: usize,
    /// Longest wait for the next byte of a request head.
    pub read_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            header_bytes: 8192,
            read_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// Turns an allowed host and port into addresses. Injected so tests can
/// point a name at a local target, and so a name resolving somewhere
/// private can be tested without DNS.
pub type Resolver = Arc<dyn Fn(&Host, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

pub fn system_resolver() -> Resolver {
    Arc::new(|host: &Host, port: u16| {
        (host.as_str(), port)
            .to_socket_addrs()
            .map(Iterator::collect)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Timeout,
    TooLarge,
    Malformed,
    Method,
    Port,
    Host,
    /// The allowed name resolved somewhere it should not be: an ordinary
    /// host to a non-public address, or an edge host away from the gateway.
    Private,
    Unresolved,
    Unreachable,
}

impl Refusal {
    fn status(self) -> &'static str {
        match self {
            Refusal::Timeout => "408 Request Timeout",
            Refusal::TooLarge => "431 Request Header Fields Too Large",
            Refusal::Malformed => "400 Bad Request",
            Refusal::Method => "405 Method Not Allowed",
            Refusal::Port | Refusal::Host | Refusal::Private => "403 Forbidden",
            Refusal::Unresolved | Refusal::Unreachable => "502 Bad Gateway",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Tunnelled,
    Refused(Refusal),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Tunnelled => f.write_str("tunnelled"),
            Outcome::Refused(r) => write!(f, "refused: {r:?}"),
        }
    }
}

pub struct Proxy {
    policy: Policy,
    resolve: Resolver,
    limits: Limits,
}

impl Proxy {
    pub fn new(policy: Policy, resolve: Resolver) -> Proxy {
        Proxy {
            policy,
            resolve,
            limits: Limits::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Proxy {
        self.limits = limits;
        self
    }

    /// Serve forever, one thread per connection, one log line per decision.
    pub fn serve(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            let me = Arc::clone(&self);
            thread::spawn(move || {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "?".into());
                let (target, outcome) = me.handle(stream);
                eprintln!("{peer} {target} {outcome}");
            });
        }
        Ok(())
    }

    /// Handle one client connection to completion. Returns the target as
    /// parsed, for the log, and what was decided.
    pub fn handle(&self, mut client: TcpStream) -> (String, Outcome) {
        let _ = client.set_read_timeout(Some(self.limits.read_timeout));
        let _ = client.set_nodelay(true);

        let mut buf: Vec<u8> = Vec::with_capacity(1024);
        let head_end = loop {
            if let Some(i) = find_head_end(&buf) {
                break i;
            }
            if buf.len() >= self.limits.header_bytes {
                return refuse(client, Refusal::TooLarge, "-");
            }
            let mut chunk = [0u8; 1024];
            let want = chunk.len().min(self.limits.header_bytes - buf.len());
            match client.read(&mut chunk[..want]) {
                Ok(0) => return refuse(client, Refusal::Malformed, "-"),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return refuse(client, Refusal::Timeout, "-")
                }
                Err(_) => return refuse(client, Refusal::Malformed, "-"),
            }
        };

        let line_end = buf
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap_or(head_end);
        let Ok(line) = std::str::from_utf8(&buf[..line_end]) else {
            return refuse(client, Refusal::Malformed, "-");
        };
        let shown = printable(line);
        let mut parts = line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return refuse(client, Refusal::Malformed, &shown);
        };
        if !version.starts_with("HTTP/1.") {
            return refuse(client, Refusal::Malformed, &shown);
        }
        if method != "CONNECT" {
            return refuse(client, Refusal::Method, &shown);
        }
        if target.contains('/') || target.starts_with('[') {
            return refuse(client, Refusal::Malformed, &shown);
        }
        let Some((h, p)) = target.rsplit_once(':') else {
            return refuse(client, Refusal::Malformed, &shown);
        };
        let Ok(host) = h.to_ascii_lowercase().parse::<Host>() else {
            return refuse(client, Refusal::Malformed, &shown);
        };
        let canonical_digits =
            p.bytes().all(|c| c.is_ascii_digit()) && !(p.len() > 1 && p.starts_with('0'));
        let port = match p.parse::<u16>() {
            Ok(n) if n != 0 && canonical_digits => n,
            _ => return refuse(client, Refusal::Malformed, &shown),
        };
        let shown = format!("{host}:{port}");
        if !self.policy.ports.contains(&port) {
            return refuse(client, Refusal::Port, &shown);
        }
        if !self.policy.allows(&host) {
            return refuse(client, Refusal::Host, &shown);
        }
        let addrs = match (self.resolve)(&host, port) {
            Ok(a) if !a.is_empty() => a,
            _ => return refuse(client, Refusal::Unresolved, &shown),
        };
        if !self.policy.placed_well(&host, &addrs) {
            return refuse(client, Refusal::Private, &shown);
        }
        let reach =
            |a: &SocketAddr| TcpStream::connect_timeout(a, self.limits.connect_timeout).ok();
        let Some(mut target) = addrs.iter().find_map(reach) else {
            return refuse(client, Refusal::Unreachable, &shown);
        };

        let _ = target.set_nodelay(true);
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .is_err()
        {
            return (shown, Outcome::Tunnelled);
        }
        let leftover = &buf[head_end + 4..];
        if !leftover.is_empty() && target.write_all(leftover).is_err() {
            return (shown, Outcome::Tunnelled);
        }
        let _ = client.set_read_timeout(None);
        pipe(client, target);
        (shown, Outcome::Tunnelled)
    }
}

fn refuse(mut client: TcpStream, why: Refusal, shown: &str) -> (String, Outcome) {
    let _ = client.write_all(
        format!(
            "HTTP/1.1 {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            why.status()
        )
        .as_bytes(),
    );
    let _ = client.shutdown(Shutdown::Both);
    (shown.to_string(), Outcome::Refused(why))
}

fn pipe(client: TcpStream, target: TcpStream) {
    let (Ok(mut c_in), Ok(mut t_in)) = (client.try_clone(), target.try_clone()) else {
        return;
    };
    let mut c_out = client;
    let mut t_out = target;
    let up = thread::spawn(move || {
        let _ = io::copy(&mut c_in, &mut t_out);
        let _ = t_out.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut t_in, &mut c_out);
    let _ = c_out.shutdown(Shutdown::Write);
    let _ = up.join();
}

/// Index of the CRLFCRLF that ends the request head.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// A bounded, printable rendering of untrusted request text for one log line.
fn printable(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(96)
        .collect()
}

/// An address a cell may be tunnelled to: nothing loopback, private,
/// link-local, multicast, reserved or metadata-shaped, in either family.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return is_public_v4(Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xff00) == 0xff00
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || s[..6] == [0, 0, 0, 0, 0, 0])
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || o[0] >= 240
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn public_network_keeps_address_and_edge_boundaries() {
        let cell: Cell = serde_json::from_str(
            r#"{"name":"research","grants":[{"network":"public"},{"edge":"brave"}]}"#,
        )
        .unwrap();
        let policy = Policy::for_cell(&cell);
        let host: Host = "en.wikipedia.org".parse().unwrap();
        let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
        let gateway: SocketAddr = (GATEWAY, 443).into();
        assert!(policy.allows(&host));
        assert!(policy.placed_well(&host, &[public]));
        assert!(!policy.private_ok);
        assert!(!policy.allows(&"github.int.exe.xyz".parse().unwrap()));
        assert!(!policy.allows(&"other.int.exe.xyz".parse().unwrap()));
        for address in [
            "127.0.0.1:443",
            "10.0.0.1:443",
            "192.168.1.1:443",
            "169.254.169.254:443",
            "[::1]:443",
            "[fd00::1]:443",
            "[fe80::1]:443",
            "[::ffff:127.0.0.1]:443",
        ] {
            let private: SocketAddr = address.parse().unwrap();
            assert!(!policy.placed_well(&host, &[private]), "{address}");
            assert!(!policy.placed_well(&host, &[public, private]), "{address}");
        }
        for name in ["brave.int.exe.xyz", "door-child.int.exe.xyz"] {
            let edge = name.parse().unwrap();
            assert!(policy.allows(&edge));
            assert!(policy.placed_well(&edge, &[gateway]));
            assert!(!policy.placed_well(&edge, &[public]));
        }
    }

    #[test]
    fn public_addresses() {
        let v4 = |s: &str| is_public(s.parse::<Ipv4Addr>().unwrap().into());
        let v6 = |s: &str| is_public(s.parse::<Ipv6Addr>().unwrap().into());
        for ok in ["8.8.8.8", "160.79.104.10", "140.82.112.3", "1.1.1.1"] {
            assert!(v4(ok), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "192.0.2.1",
        ] {
            assert!(!v4(bad), "{bad}");
        }
        assert!(v6("2606:4700:4700::1111"));
        for bad in [
            "::1",
            "::",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "fe80::a9fe:a9fe",
            "ff02::1",
            "2001:db8::1",
            "::ffff:169.254.169.254",
            "::ffff:127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "::a9fe:a9fe",
        ] {
            assert!(!v6(bad), "{bad}");
        }
        assert!(v6("::ffff:8.8.8.8"));
        assert!(v6("64:ff9b::808:808"));
    }

    #[test]
    fn head_end_and_printable() {
        assert_eq!(
            find_head_end(b"CONNECT a:443 HTTP/1.1\r\n\r\nxyz"),
            Some(22)
        );
        assert_eq!(find_head_end(b"CONNECT a:443 HTTP/1.1\r\n"), None);
        assert_eq!(printable("a\x1b[31mb\r\nc"), "a[31mbc");
        assert_eq!(printable(&"x".repeat(200)).len(), 96);
    }
}
