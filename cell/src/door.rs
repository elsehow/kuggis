//! The door: the one way into a cell, and the way its log is read.
//!
//! A parent reaches it through an exe.dev peer integration, so every
//! request that arrives carries `X-Exedev-Source-Vm`, set by the platform
//! after stripping anything the caller sent. The door trusts that header
//! and nothing else: it needs no key of its own, and the child never learns
//! who its parent is beyond the name stamped on each knock.
//!
//! Two routes. `GET /said?since=N` serves the signed statement log from
//! N+1 on, one JSON object per line. `POST /say` takes one `Said`, a brief
//! or a reply, and files it in the inbox for the agent to read. Everything
//! else is refused. The door runs as its own user: it reads the log, writes
//! the inbox, and touches nothing else.

use crate::cell::Cell;
use crate::prim::Name;
use crate::said::Said;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// The header exe.dev sets on a peer request, naming the calling VM.
pub const SOURCE_HEADER: &str = "x-exedev-source-vm";

/// What the inbox holds: who knocked, and what they said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Knock {
    pub from: Name,
    pub said: Said,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub header_bytes: usize,
    pub body_bytes: usize,
    pub read_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            header_bytes: 8192,
            body_bytes: 65536,
            read_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Timeout,
    TooLarge,
    Malformed,
    Method,
    NotFound,
    /// No platform-set source header: the knock did not come through a door.
    Unvouched,
    /// A well-formed Said the door does not take: not a brief for this cell
    /// or a reply.
    NotForUs,
}

impl Refusal {
    fn status(self) -> &'static str {
        match self {
            Refusal::Timeout => "408 Request Timeout",
            Refusal::TooLarge => "413 Content Too Large",
            Refusal::Malformed => "400 Bad Request",
            Refusal::Method => "405 Method Not Allowed",
            Refusal::NotFound => "404 Not Found",
            Refusal::Unvouched => "401 Unauthorized",
            Refusal::NotForUs => "422 Unprocessable Content",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Served the log from this sequence on.
    Served(u64),
    /// Filed a knock at this inbox number.
    Filed(u64),
    Refused(Refusal),
}

pub struct Door {
    cell: Name,
    said: PathBuf,
    inbox: PathBuf,
    limits: Limits,
}

impl Door {
    pub fn new(cell: &Cell, said: impl Into<PathBuf>, inbox: impl Into<PathBuf>) -> Door {
        Door {
            cell: cell.name.clone(),
            said: said.into(),
            inbox: inbox.into(),
            limits: Limits::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Door {
        self.limits = limits;
        self
    }

    /// Serve forever, one thread per connection, one log line per decision.
    pub fn serve(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            let me = Arc::clone(&self);
            thread::spawn(move || {
                let (from, outcome) = me.handle(stream);
                eprintln!("{from} {outcome:?}");
            });
        }
        Ok(())
    }

    /// Handle one connection to completion. Returns who knocked, for the
    /// log, and what was decided.
    pub fn handle(&self, mut client: TcpStream) -> (String, Outcome) {
        let _ = client.set_read_timeout(Some(self.limits.read_timeout));
        let (head, mut body) = match read_head(&mut client, self.limits.header_bytes) {
            Ok(x) => x,
            Err(r) => return refuse(client, r, "-"),
        };
        let Ok(head) = std::str::from_utf8(&head) else {
            return refuse(client, Refusal::Malformed, "-");
        };
        let mut lines = head.split("\r\n");
        let Some(request) = lines.next() else {
            return refuse(client, Refusal::Malformed, "-");
        };
        let mut parts = request.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return refuse(client, Refusal::Malformed, "-");
        };
        if !version.starts_with("HTTP/1.") {
            return refuse(client, Refusal::Malformed, "-");
        }
        let mut from = None;
        let mut length = 0usize;
        for line in lines {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == SOURCE_HEADER {
                from = v.parse::<Name>().ok();
            } else if k == "content-length" {
                let Ok(n) = v.parse::<usize>() else {
                    return refuse(client, Refusal::Malformed, "-");
                };
                length = n;
            }
        }
        let shown = from.as_ref().map_or("-".to_string(), ToString::to_string);
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        match (method, path) {
            ("GET", "/said") => {
                let since = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("since="))
                    .map(str::parse::<u64>)
                    .unwrap_or(Ok(0));
                let Ok(since) = since else {
                    return refuse(client, Refusal::Malformed, &shown);
                };
                self.serve_log(client, since, &shown)
            }
            ("POST", "/say") => {
                let Some(from) = from else {
                    return refuse(client, Refusal::Unvouched, &shown);
                };
                if length > self.limits.body_bytes {
                    return refuse(client, Refusal::TooLarge, &shown);
                }
                if body.len() < length {
                    let mut rest = vec![0u8; length - body.len()];
                    if client.read_exact(&mut rest).is_err() {
                        return refuse(client, Refusal::Timeout, &shown);
                    }
                    body.extend_from_slice(&rest);
                }
                body.truncate(length);
                let Ok(said) = serde_json::from_slice::<Said>(&body) else {
                    return refuse(client, Refusal::Malformed, &shown);
                };
                self.file(client, Knock { from, said }, &shown)
            }
            ("GET" | "POST", _) => refuse(client, Refusal::NotFound, &shown),
            _ => refuse(client, Refusal::Method, &shown),
        }
    }

    fn serve_log(&self, mut client: TcpStream, since: u64, shown: &str) -> (String, Outcome) {
        let mut out = Vec::new();
        let mut n = since;
        while let Some(next) = n.checked_add(1) {
            let Ok(bytes) = fs::read(self.said.join(format!("{next}.json"))) else {
                break;
            };
            out.extend_from_slice(bytes.trim_ascii());
            out.push(b'\n');
            n = next;
        }
        let _ = client.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                out.len()
            )
            .as_bytes(),
        );
        let _ = client.write_all(&out);
        let _ = client.shutdown(Shutdown::Both);
        (shown.to_string(), Outcome::Served(since))
    }

    fn file(&self, client: TcpStream, knock: Knock, shown: &str) -> (String, Outcome) {
        let takes = match &knock.said {
            Said::Brief { to, .. } => *to == self.cell,
            Said::Reply { .. } => true,
            _ => false,
        };
        if !takes {
            return refuse(client, Refusal::NotForUs, shown);
        }
        let n = match next_number(&self.inbox) {
            Ok(n) => n,
            Err(_) => return refuse(client, Refusal::Malformed, shown),
        };
        let bytes = serde_json::to_vec(&knock).expect("a knock serializes");
        let path = self.inbox.join(format!("{n}.json"));
        let tmp = self.inbox.join(format!("{n}.tmp"));
        if fs::write(&tmp, &bytes)
            .and_then(|()| fs::rename(&tmp, &path))
            .is_err()
        {
            return refuse(client, Refusal::Malformed, shown);
        }
        let mut client = client;
        let _ = client
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = client.shutdown(Shutdown::Both);
        (shown.to_string(), Outcome::Filed(n))
    }
}

/// Read up to and including the blank line; return the head and whatever
/// body bytes came with it.
fn read_head(client: &mut TcpStream, limit: usize) -> Result<(Vec<u8>, Vec<u8>), Refusal> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = buf.split_off(i + 4);
            buf.truncate(i);
            return Ok((buf, body));
        }
        if buf.len() >= limit {
            return Err(Refusal::TooLarge);
        }
        let mut chunk = [0u8; 1024];
        let want = chunk.len().min(limit - buf.len());
        match client.read(&mut chunk[..want]) {
            Ok(0) => return Err(Refusal::Malformed),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Refusal::Timeout)
            }
            Err(_) => return Err(Refusal::Malformed),
        }
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

/// The next free number in a directory of `<n>.json`.
pub fn next_number(dir: &Path) -> io::Result<u64> {
    let mut last = 0u64;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(n) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .and_then(|s| s.parse::<u64>().ok())
        {
            last = last.max(n);
        }
    }
    last.checked_add(1)
        .ok_or_else(|| io::Error::other("numbers exhausted"))
}
