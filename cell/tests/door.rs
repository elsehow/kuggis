//! The door over real sockets: the log served, knocks filed, and every
//! wrong knock refused.

use cell::door::{Door, Knock, Limits};
use cell::{Cell, Said, Secret, Statement};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cell-door-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("said")).unwrap();
    fs::create_dir_all(d.join("inbox")).unwrap();
    d
}

fn start(dir: &Path) -> SocketAddr {
    let cell: Cell =
        serde_json::from_str(r#"{"name":"child","grants":[{"agent":"claude"}]}"#).unwrap();
    let door = Arc::new(
        Door::new(&cell, dir.join("said"), dir.join("inbox")).with_limits(Limits {
            header_bytes: 2048,
            body_bytes: 4096,
            read_timeout: Duration::from_millis(500),
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || door.serve(listener));
    addr
}

fn exchange(addr: SocketAddr, raw: &[u8]) -> (String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(raw).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head.lines().next().unwrap_or("").to_string(),
        body.to_string(),
    )
}

fn post(addr: SocketAddr, from: Option<&str>, body: &str) -> (String, String) {
    let header = from.map_or(String::new(), |f| format!("X-Exedev-Source-Vm: {f}\r\n"));
    exchange(
        addr,
        format!(
            "POST /say HTTP/1.1\r\nHost: door\r\n{header}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

#[test]
fn the_log_is_served_from_where_you_left_off() {
    let d = tmp("log");
    let addr = start(&d);
    let (status, body) = exchange(addr, b"GET /said HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "", "an empty log");

    let k = Secret::generate();
    for n in 1..=3u64 {
        let s = Statement::sign(
            &k,
            "child".parse().unwrap(),
            n,
            Said::Fetch {
                url: format!("https://example.com/{n}").parse().unwrap(),
            },
        );
        fs::write(
            d.join("said").join(format!("{n}.json")),
            serde_json::to_string(&s).unwrap(),
        )
        .unwrap();
    }
    let (_, body) = exchange(addr, b"GET /said HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(body.lines().count(), 3);
    let (_, body) = exchange(addr, b"GET /said?since=2 HTTP/1.1\r\nHost: door\r\n\r\n");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1);
    let s: Statement = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(s.seq, 3);
    assert!(s.open(&k.public()).is_ok(), "served as signed");
    let (_, body) = exchange(addr, b"GET /said?since=3 HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(body, "");
    let (status, _) = exchange(addr, b"GET /said?since=x HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 400 Bad Request");
    // a gap in the log ends what is served, rather than skipping it
    fs::write(d.join("said/5.json"), "{}").unwrap();
    let (_, body) = exchange(addr, b"GET /said?since=2 HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(body.lines().count(), 1);
}

#[test]
fn a_vouched_brief_or_reply_is_filed_and_nothing_else_is() {
    let d = tmp("knock");
    let addr = start(&d);
    let brief = r#"{"brief":{"to":"child","session":"dev","text":"fix the tests"}}"#;

    let (status, _) = post(addr, None, brief);
    assert_eq!(
        status, "HTTP/1.1 401 Unauthorized",
        "no platform header, no knock"
    );
    let (status, _) = post(addr, Some("Not A Name"), brief);
    assert_eq!(
        status, "HTTP/1.1 401 Unauthorized",
        "a header that is not a name"
    );
    assert!(fs::read_dir(d.join("inbox")).unwrap().next().is_none());

    let (status, _) = post(addr, Some("parent"), brief);
    assert_eq!(status, "HTTP/1.1 202 Accepted");
    let filed: Knock =
        serde_json::from_str(&fs::read_to_string(d.join("inbox/1.json")).unwrap()).unwrap();
    assert_eq!(filed.from.as_str(), "parent");
    assert_eq!(filed.said, serde_json::from_str::<Said>(brief).unwrap());

    let reply = format!(
        r#"{{"reply":{{"of":"{}","body":{{"Ok":"yes"}}}}}}"#,
        "ab".repeat(32)
    );
    let (status, _) = post(addr, Some("parent"), &reply);
    assert_eq!(status, "HTTP/1.1 202 Accepted");
    assert!(d.join("inbox/2.json").exists());

    // a brief for someone else, and things that only go upward
    for wrong in [
        r#"{"brief":{"to":"other","session":"dev","text":"hi"}}"#,
        r#"{"verdict":{"repo":"o/r","head":"0123456789abcdef0123456789abcdef01234567","pass":true}}"#,
        r#"{"session":{"name":"dev","harness":"claude","state":"idle"}}"#,
        r#"{"fetch":{"url":"https://example.com/"}}"#,
        &format!(
            r#"{{"checked":{{"cell":"{}","ok":true,"report":""}}}}"#,
            "00".repeat(32)
        ),
    ] {
        let (status, _) = post(addr, Some("parent"), wrong);
        assert_eq!(status, "HTTP/1.1 422 Unprocessable Content", "{wrong}");
    }
    for bad in [
        "not json",
        r#"{"brief":{"to":"child","session":"dev","text":"x","extra":1}}"#,
        r#"{"brief":{"to":"child","session":"plan","text":"a command word"}}"#,
    ] {
        let (status, _) = post(addr, Some("parent"), bad);
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{bad}");
    }
    let big = format!(
        r#"{{"brief":{{"to":"child","session":"dev","text":"{}"}}}}"#,
        "x".repeat(5000)
    );
    let (status, _) = post(addr, Some("parent"), &big);
    assert_eq!(status, "HTTP/1.1 413 Content Too Large");
    assert_eq!(
        fs::read_dir(d.join("inbox")).unwrap().count(),
        2,
        "nothing else was filed"
    );

    let (status, _) = exchange(addr, b"PUT /say HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
    let (status, _) = exchange(addr, b"GET /inbox HTTP/1.1\r\nHost: door\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    let (status, _) = exchange(addr, b"GET /said HTTP/2\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 400 Bad Request");
    let (status, _) = exchange(addr, b"GET /said HTTP/1.1\r\nHost: door\r\n");
    assert_eq!(status, "HTTP/1.1 408 Request Timeout", "a head that stalls");
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(b"GET /said HTTP/1.1\r\nHost: door\r\n")
        .unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(
        out.starts_with("HTTP/1.1 400 Bad Request"),
        "a head cut short: {out}"
    );
    let (status, _) = exchange(addr, &[b'a'; 3000]);
    assert_eq!(status, "HTTP/1.1 413 Content Too Large");
    let (status, _) = exchange(addr, b"POST /say HTTP/1.1\r\nX-Exedev-Source-Vm: parent\r\nContent-Length: 50\r\n\r\n{\"brief\":");
    assert_eq!(
        status, "HTTP/1.1 408 Request Timeout",
        "a body that never comes"
    );
}
