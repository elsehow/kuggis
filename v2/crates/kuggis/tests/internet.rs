use kuggis::{
    config::{Egress, Host},
    deploy::{firewall, firewall_for},
    network::Policy,
    proxy, Workspace,
};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

fn workspace(mode: &str) -> Workspace {
    Workspace::parse(
        &json!({"version":2,"name":"test","ssh":"test-host","launch":["bash"],"egress":mode,
        "resources":[{"name":"private","endpoints":[{"host":"private.example","port":8443,
            "destination":{"kind":"pinned","addresses":["10.0.2.2"]}}]}]})
        .to_string(),
    )
    .unwrap()
}

#[test]
fn internet_allows_unlisted_public_targets_but_preserves_private_pins() {
    let w = workspace("internet");
    let p = Policy::new(&w);
    let public = Host::try_from("new.example".to_owned()).unwrap();
    assert!(p.allows_resolved(&public, 12345, &["1.1.1.1".parse().unwrap()]));
    for addr in [
        "127.0.0.1",
        "10.0.2.2",
        "169.254.169.254",
        "192.168.1.1",
        "::ffff:127.0.0.1",
        "::1",
    ] {
        assert!(!p.allows_resolved(&public, 443, &[addr.parse().unwrap()]));
        assert!(!p.allows_resolved(
            &public,
            443,
            &["1.1.1.1".parse().unwrap(), addr.parse().unwrap()]
        ));
    }
    assert!(!p.allows_resolved(&public, 443, &[]));
    assert!(!p.allows_resolved(&public, 0, &["1.1.1.1".parse().unwrap()]));
    let pinned = Host::try_from("private.example".to_owned()).unwrap();
    assert!(p.allows_resolved(&pinned, 8443, &["10.0.2.2".parse().unwrap()]));
    assert!(!p.allows_resolved(&pinned, 443, &["10.0.2.2".parse().unwrap()]));
    assert!(!p.allows_resolved(&pinned, 8443, &["1.1.1.1".parse().unwrap()]));
    assert!(!Policy::new(&workspace("restricted")).allows_target(&public, 443));
}

#[test]
fn old_configs_remain_restricted_and_internet_firewall_retains_management_guards() {
    let w =
        Workspace::parse(r#"{"version":2,"name":"test","ssh":"host","launch":["bash"]}"#).unwrap();
    assert_eq!(w.egress(), Egress::Restricted);
    assert!(serde_json::to_value(w).unwrap().get("egress").is_none());
    assert_eq!(firewall(), firewall_for(Egress::Restricted));
    let internet = firewall_for(Egress::Internet);
    assert!(internet.contains("masquerade"));
    assert!(internet.contains("10.205.0.2 ct state established,related accept"));
    assert!(internet.contains("169.254.0.0/16"));
    // Conntrack runs at -200; reply rules must run strictly afterward, including
    // when NAT causes those hooks to be registered/reordered on a live update.
    assert!(internet.contains("type filter hook output priority -150;"));
    let input = |text: String| text.split(" chain forward {").next().unwrap().to_owned();
    assert_eq!(input(internet), input(firewall()));
    assert!(Workspace::parse(
        r#"{"version":2,"name":"test","ssh":"host","launch":["bash"],"egress":"typo"}"#
    )
    .is_err());
}

#[test]
fn absolute_http_requests_route_by_url_and_drop_proxy_auth() {
    let (target, bytes)=proxy::http_request(b"GET http://example.com:8080/path?q=yes HTTP/1.1\r\nHost: wrong.example\r\nProxy-Authorization: secret\r\nUser-Agent: example\r\n\r\n").unwrap();
    assert_eq!(target.host.as_str(), "example.com");
    assert_eq!(target.port, 8080);
    let output = String::from_utf8(bytes).unwrap();
    assert!(output.starts_with(
        "GET /path?q=yes HTTP/1.1\r\nHost: example.com:8080\r\nConnection: close\r\n"
    ));
    assert!(!output.contains("secret"));
    assert!(!output.contains("wrong.example"));
    for invalid in [
        "GET /relative HTTP/1.1\r\n\r\n",
        "GET https://example.com/ HTTP/1.1\r\n\r\n",
        "GET http://user:password@example.com/ HTTP/1.1\r\n\r\n",
        "GET http://example.com/ HTTP/1.1\r\n folded: value\r\n\r\n",
    ] {
        assert!(proxy::http_request(invalid.as_bytes()).is_err());
    }
}

#[test]
fn http_proxy_passes_the_actual_response_and_finishes_with_client_stdin_open() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = upstream.local_addr().unwrap();
    let remote = thread::spawn(move || {
        let (mut socket, _) = upstream.accept().unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            socket.read_exact(&mut b).unwrap();
            header.push(b[0]);
        }
        assert!(header.starts_with(b"GET / HTTP/1.1\r\n"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    });
    let proxy = thread::spawn(move || {
        proxy::handle(
            server,
            &workspace("internet"),
            &|_, port| {
                assert_eq!(port, 8080);
                Ok(vec!["1.1.1.1:8080".parse().unwrap()])
            },
            &|_| TcpStream::connect(address),
        )
        .unwrap()
    });
    client
        .write_all(b"GET http://new.example:8080/ HTTP/1.1\r\nHost: new.example:8080\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
    );
    proxy.join().unwrap();
    remote.join().unwrap();
}
