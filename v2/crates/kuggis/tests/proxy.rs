use kuggis::{
    proxy::{self, Target},
    Workspace,
};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

const CONFIG: &str = r#"{"version":2,"name":"test","ssh":"test-host","launch":["bash"],"resources":[{"name":"target","endpoints":[{"host":"allowed.example","port":443}]}]}"#;

#[test]
fn strict_connect_target_parser() {
    for bad in [
        "GET allowed.example:443 HTTP/1.1",
        "CONNECT allowed.example:0443 HTTP/1.1",
        "CONNECT allowed.example:0 HTTP/1.1",
        "CONNECT allowed.example:443 HTTP/1.bad",
        "CONNECT allowed.example:443/path HTTP/1.1",
        "CONNECT allowed.example:443 HTTP/1.1 extra",
        "CONNECT [allowed.example]:443 HTTP/1.1",
        "CONNECT 2606:4700:4700::1111:443 HTTP/1.1",
    ] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(
        Target::parse("CONNECT ALLOWED.example:443 HTTP/1.1")
            .unwrap()
            .host
            .as_str(),
        "allowed.example"
    );
    assert_eq!(
        Target::parse("CONNECT [2606:4700:4700:0:0:0:0:1111]:443 HTTP/1.1")
            .unwrap()
            .host
            .as_str(),
        "2606:4700:4700::1111"
    );
}

#[test]
fn rejected_requests_never_dial_or_resolve_unconfigured_names() {
    for (request, addresses, expected) in [
        (
            "CONNECT missing.example:443 HTTP/1.1\r\n\r\n",
            vec!["1.1.1.1:443"],
            "403",
        ),
        (
            "CONNECT allowed.example:22 HTTP/1.1\r\n\r\n",
            vec!["1.1.1.1:22"],
            "403",
        ),
        (
            "CONNECT allowed.example:443 HTTP/1.1\r\n\r\n",
            vec!["1.1.1.1:443", "127.0.0.1:443"],
            "403",
        ),
        (
            "CONNECT allowed.example:443 HTTP/1.1\r\n\r\n",
            vec!["1.1.1.1:22"],
            "403",
        ),
        ("GET / HTTP/1.1\r\n\r\n", vec![], "400"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        let resolves = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&resolves);
        let worker = thread::spawn(move || {
            proxy::handle(
                server,
                &Workspace::parse(CONFIG).unwrap(),
                &|_, _| {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(addresses
                        .iter()
                        .map(|address| address.parse().unwrap())
                        .collect())
                },
                &|_| panic!("denied request was dialed"),
            )
            .unwrap()
        });
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {expected}")),
            "{response}"
        );
        worker.join().unwrap();
        if request.contains("missing.example") || request.contains("example:22") {
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn checked_address_is_dialed_once_and_early_binary_bytes_survive() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_address = target.local_addr().unwrap();
    let echo = thread::spawn(move || {
        let (mut connection, _) = target.accept().unwrap();
        let mut bytes = Vec::new();
        connection.read_to_end(&mut bytes).unwrap();
        connection.write_all(&bytes).unwrap();
    });
    let worker = thread::spawn(move || {
        proxy::handle(
            server,
            &Workspace::parse(CONFIG).unwrap(),
            &|_, _| Ok(vec!["1.1.1.1:443".parse().unwrap()]),
            &|address| {
                assert_eq!(address.to_string(), "1.1.1.1:443");
                TcpStream::connect(target_address)
            },
        )
        .unwrap()
    });
    client
        .write_all(b"CONNECT allowed.example:443 HTTP/1.1\r\n\r\n\0\xffbinary")
        .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"\0\xffbinary"));
    worker.join().unwrap();
    echo.join().unwrap();
}
