//! The wall, poked at over real sockets. A target server sits on loopback;
//! a resolver maps names to it, or to somewhere private, without DNS.

use cell::proxy::{Limits, Policy, Proxy, Resolver};
use cell::{Cell, Host};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// An echo target: replies "echo:" + whatever it reads until the client
/// half-closes, then closes.
fn target() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                let _ = s.write_all(b"echo:");
                let _ = s.write_all(&buf);
                let _ = s.shutdown(Shutdown::Both);
            });
        }
    });
    addr
}

fn resolver(map: Vec<(&str, SocketAddr)>) -> Resolver {
    let map: Vec<(String, SocketAddr)> = map.into_iter().map(|(h, a)| (h.to_string(), a)).collect();
    Arc::new(move |host: &Host, _port: u16| {
        Ok(map
            .iter()
            .filter(|(h, _)| h == host.as_str())
            .map(|(_, a)| *a)
            .collect())
    })
}

fn cell(json: &str) -> Cell {
    serde_json::from_str(json).unwrap()
}

fn proxy(policy: Policy, resolve: Resolver, limits: Limits) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let p = Arc::new(Proxy::new(policy, resolve).with_limits(limits));
    thread::spawn(move || p.serve(l).unwrap());
    addr
}

fn limits() -> Limits {
    Limits {
        header_bytes: 1024,
        read_timeout: Duration::from_millis(300),
        connect_timeout: Duration::from_secs(2),
    }
}

/// Send raw bytes, half-close, read everything back. Returns (status line, body after the head).
fn exchange(proxy: SocketAddr, raw: &[u8]) -> (String, Vec<u8>) {
    let mut s = TcpStream::connect(proxy).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(raw).unwrap();
    s.shutdown(Shutdown::Write).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    split(&out)
}

fn split(out: &[u8]) -> (String, Vec<u8>) {
    let i = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&out[..i]).to_string();
    let status = head.lines().next().unwrap_or("").to_string();
    (status, out[i + 4..].to_vec())
}

fn allowed_policy() -> Policy {
    let c = cell(r#"{"name":"t","grants":[{"provider":"anthropic"},{"host":"pypi.org"}]}"#);
    let mut p = Policy::for_cell(&c);
    p.private_ok = true; // the target lives on loopback
    p
}

#[test]
fn tunnels_an_allowed_host_and_forwards_bytes_sent_before_the_200() {
    let t = target();
    let px = proxy(
        allowed_policy(),
        resolver(vec![("api.anthropic.com", t), ("pypi.org", t)]),
        limits(),
    );
    let (status, body) = exchange(
        px,
        b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\r\nhello",
    );
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    assert_eq!(body, b"echo:hello");
    // case folds, port 8443 allowed too, data after the 200
    let mut s = TcpStream::connect(px).unwrap();
    s.write_all(b"CONNECT PyPI.org:8443 HTTP/1.0\r\n\r\n")
        .unwrap();
    let mut head = [0u8; 39];
    s.read_exact(&mut head).unwrap();
    assert_eq!(&head[..], b"HTTP/1.1 200 Connection Established\r\n\r\n");
    s.write_all(b"later").unwrap();
    s.shutdown(Shutdown::Write).unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"echo:later");
}

#[test]
fn refuses_hosts_off_the_list_and_ports_off_the_list() {
    let t = target();
    let px = proxy(
        allowed_policy(),
        resolver(vec![
            ("api.anthropic.com", t),
            ("evil.example", t),
            ("pypi.org", t),
        ]),
        limits(),
    );
    let (status, body) = exchange(px, b"CONNECT evil.example:443 HTTP/1.1\r\n\r\nsecret");
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert!(body.is_empty(), "nothing is forwarded on a refusal");
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:80 HTTP/1.1\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:65535 HTTP/1.1\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    // a subdomain or superdomain of an allowed host is another host
    let (status, _) = exchange(px, b"CONNECT x.api.anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let (status, _) = exchange(px, b"CONNECT anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
}

#[test]
fn refuses_an_allowed_name_that_resolves_somewhere_private() {
    let t = target();
    let mut policy = allowed_policy();
    policy.private_ok = false;
    let metadata: SocketAddr = "169.254.169.254:80".parse().unwrap();
    let loopback: SocketAddr = t;
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let px = proxy(
        policy,
        resolver(vec![
            ("api.anthropic.com", metadata),
            ("pypi.org", public),
            ("pypi.org", loopback),
        ]),
        limits(),
    );
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert_eq!(
        status, "HTTP/1.1 403 Forbidden",
        "rebinding an allowed name to the metadata service"
    );
    let (status, _) = exchange(px, b"CONNECT pypi.org:443 HTTP/1.1\r\n\r\n");
    assert_eq!(
        status, "HTTP/1.1 403 Forbidden",
        "one private answer among public ones poisons the lot"
    );
}

#[test]
fn edge_hosts_must_sit_at_the_gateway_and_ordinary_hosts_must_not() {
    let c = cell(
        r#"{"name":"t","grants":[{"provider":"anthropic"},{"provider":"openai"},{"edge":"codex"},{"code":{"repo":"o/r","write":true}},{"host":"pypi.org"}]}"#,
    );
    let policy = Policy::for_cell(&c);
    assert_eq!(
        policy
            .edge
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["codex.int.exe.xyz", "github.int.exe.xyz"]
    );
    let gateway: SocketAddr = (cell::proxy::GATEWAY, 443).into();
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let fast = Limits {
        connect_timeout: Duration::from_millis(100),
        ..limits()
    };
    let px = proxy(
        policy,
        resolver(vec![
            ("github.int.exe.xyz", public), // the edge, rebound elsewhere
            ("codex.int.exe.xyz", gateway), // the edge, where it belongs
            ("pypi.org", gateway),          // an ordinary host, pointed at the gateway
            ("api.anthropic.com", public),  // an ordinary host, public
        ]),
        fast,
    );
    let (status, _) = exchange(px, b"CONNECT github.int.exe.xyz:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 403"),
        "edge host away from the gateway: {status}"
    );
    let (status, _) = exchange(px, b"CONNECT pypi.org:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 403"),
        "ordinary host at the gateway: {status}"
    );
    // the two well-placed ones pass the address rule; from this machine the connect then fails, which is the next check
    let (status, _) = exchange(px, b"CONNECT codex.int.exe.xyz:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 502"),
        "edge host at the gateway passes placement: {status}"
    );
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 502") || status.starts_with("HTTP/1.1 200"),
        "public host passes placement: {status}"
    );
}

#[test]
fn refuses_everything_that_is_not_a_clean_connect() {
    let t = target();
    let px = proxy(
        allowed_policy(),
        resolver(vec![("api.anthropic.com", t)]),
        limits(),
    );
    let cases: &[(&[u8], &str)] = &[
        (b"GET https://api.anthropic.com/ HTTP/1.1\r\n\r\n", "405"),
        (b"POST api.anthropic.com:443 HTTP/1.1\r\n\r\n", "405"),
        (
            b"CONNECT https://api.anthropic.com:443 HTTP/1.1\r\n\r\n",
            "400",
        ),
        (b"CONNECT api.anthropic.com HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:443/x HTTP/1.1\r\n\r\n", "400"),
        (
            b"CONNECT api.anthropic.com:443 HTTP/1.1 extra\r\n\r\n",
            "400",
        ),
        (b"CONNECT  api.anthropic.com:443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:443 FTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:443 HTTP/2\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:0443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:+443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:0 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com:443:443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT 160.79.104.10:443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT [::1]:443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com.:443 HTTP/1.1\r\n\r\n", "400"),
        (b"CONNECT api.anthropic.com\t:443 HTTP/1.1\r\n\r\n", "400"),
        (
            b"CONNECT api.anthropic\xc2\xa0.com:443 HTTP/1.1\r\n\r\n",
            "400",
        ),
        (b"CONNECT api.anthropic.com:443\r\n\r\n", "400"),
        (b"\r\n\r\n", "400"),
        (
            b"\xff\xfe CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n",
            "400",
        ),
        (
            b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: evil.example\r\n\r\n",
            "200",
        ),
    ];
    for (raw, want) in cases {
        let (status, _) = exchange(px, raw);
        assert!(
            status.starts_with(&format!("HTTP/1.1 {want}")),
            "{:?} -> {status}",
            String::from_utf8_lossy(raw)
        );
    }
    // a request line smuggled after a refused one is never read: the connection closes
    let (status, body) = exchange(
        px,
        b"GET / HTTP/1.1\r\n\r\nCONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n",
    );
    assert!(status.starts_with("HTTP/1.1 405"));
    assert!(body.is_empty());
}

#[test]
fn bounds_the_head_and_the_wait() {
    let t = target();
    let px = proxy(
        allowed_policy(),
        resolver(vec![("api.anthropic.com", t)]),
        limits(),
    );
    let big = format!(
        "CONNECT api.anthropic.com:443 HTTP/1.1\r\nX: {}\r\n\r\n",
        "a".repeat(2000)
    );
    let (status, _) = exchange(px, big.as_bytes());
    assert!(status.starts_with("HTTP/1.1 431"), "{status}");
    // an endless head with no blank line
    let mut s = TcpStream::connect(px).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    for _ in 0..40 {
        if s.write_all(
            b"X: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
        )
        .is_err()
        {
            break;
        }
    }
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let (status, _) = split(&out);
    assert!(status.starts_with("HTTP/1.1 431"), "{status}");
    // a slow client that never finishes its request
    let mut s = TcpStream::connect(px).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"CONNECT api.anthropic.com:443 HTTP/1.1\r\n")
        .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let (status, _) = split(&out);
    assert!(status.starts_with("HTTP/1.1 408"), "{status}");
    // an empty connection
    let s = TcpStream::connect(px).unwrap();
    s.shutdown(Shutdown::Write).unwrap();
    let mut out = Vec::new();
    let _ = (&s).read_to_end(&mut out);
    assert!(split(&out).0.starts_with("HTTP/1.1 400"));
}

#[test]
fn an_unresolvable_or_unreachable_host_is_a_gateway_error_not_a_hang() {
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap(); // listener dropped: refused
    let px = proxy(
        allowed_policy(),
        resolver(vec![("api.anthropic.com", dead)]), // pypi.org resolves to nothing
        limits(),
    );
    let (status, _) = exchange(px, b"CONNECT pypi.org:443 HTTP/1.1\r\n\r\n");
    assert!(status.starts_with("HTTP/1.1 502"), "{status}");
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert!(status.starts_with("HTTP/1.1 502"), "{status}");
}

#[test]
fn an_empty_cell_tunnels_nothing() {
    let t = target();
    let mut policy = Policy::for_cell(&cell(r#"{"name":"t"}"#));
    policy.private_ok = true;
    assert!(policy.hosts.is_empty());
    let px = proxy(policy, resolver(vec![("api.anthropic.com", t)]), limits());
    let (status, _) = exchange(px, b"CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n");
    assert!(status.starts_with("HTTP/1.1 403"));
}

#[test]
fn many_clients_at_once() {
    let t = target();
    let px = proxy(
        allowed_policy(),
        resolver(vec![("api.anthropic.com", t), ("evil.example", t)]),
        limits(),
    );
    let handles: Vec<_> = (0..32)
        .map(|i| {
            thread::spawn(move || {
                if i % 2 == 0 {
                    let (status, body) = exchange(
                        px,
                        format!("CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n{i}").as_bytes(),
                    );
                    assert!(status.starts_with("HTTP/1.1 200"));
                    assert_eq!(body, format!("echo:{i}").as_bytes());
                } else {
                    let (status, _) = exchange(px, b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n");
                    assert!(status.starts_with("HTTP/1.1 403"));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn doors_pass_without_a_grant_but_only_at_the_gateway() {
    // the cell file names no door; the tree and the gateway decide
    let policy = Policy::for_cell(&cell(r#"{"name":"t","grants":[{"provider":"anthropic"}]}"#));
    assert!(policy
        .hosts
        .iter()
        .all(|h| !h.as_str().starts_with("door-")));
    let gateway: SocketAddr = (cell::proxy::GATEWAY, 443).into();
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let fast = Limits {
        connect_timeout: Duration::from_millis(100),
        ..limits()
    };
    let px = proxy(
        policy,
        resolver(vec![
            ("door-child.int.exe.xyz", gateway), // a door, where doors live
            ("door-other.int.exe.xyz", public),  // a door name, rebound off the gateway
            ("doorchild.int.exe.xyz", gateway),  // not a door: no dash
            ("door-plan.int.exe.xyz", gateway),  // not a door: a command word is not a Name
            ("door-child.exe.xyz", gateway),     // not the edge
            ("door-child.int.exe.xyz.evil.example", gateway),
        ]),
        fast,
    );
    let (status, _) = exchange(px, b"CONNECT door-child.int.exe.xyz:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 502"),
        "a door at the gateway passes the host and address rules; off-VM the connect then fails: {status}"
    );
    let (status, _) = exchange(px, b"CONNECT door-other.int.exe.xyz:443 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 403"),
        "a door name away from the gateway: {status}"
    );
    for not in [
        "doorchild.int.exe.xyz",
        "door-plan.int.exe.xyz",
        "door-child.exe.xyz",
        "door-child.int.exe.xyz.evil.example",
        "DOOR-CHILD.INT.EXE.XYZ.evil.example",
    ] {
        let line = format!("CONNECT {not}:443 HTTP/1.1\r\n\r\n");
        let (status, _) = exchange(px, line.as_bytes());
        assert!(status.starts_with("HTTP/1.1 403"), "{not}: {status}");
    }
    let (status, _) = exchange(px, b"CONNECT door-child.int.exe.xyz:80 HTTP/1.1\r\n\r\n");
    assert!(
        status.starts_with("HTTP/1.1 403"),
        "a door on a port off the list: {status}"
    );
}

#[test]
fn policy_for_a_cell_is_443_and_8443_and_never_private() {
    let p = Policy::for_cell(&cell(r#"{"name":"t","grants":[{"provider":"openai"}]}"#));
    assert_eq!(p.ports, BTreeSet::from([443, 8443]));
    assert!(!p.private_ok);
    assert_eq!(
        p.hosts.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["api.openai.com", "auth.openai.com", "chatgpt.com"]
    );
}
