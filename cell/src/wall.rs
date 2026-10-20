//! The wall: what root does once on a box, and the check that proves it
//! from inside. Linux only. The binary runs the same commands an operator
//! would, so the wall reads as a script and leaves files an operator can
//! inspect.
//!
//! After setup the box has two users that matter. `agent` runs the
//! harness, owns nothing outside its home, has no sudo, and can open one
//! socket: the proxy on loopback. `cell-proxy` runs the proxy, may resolve
//! names at the gateway and may open TLS ports, nothing else. Root owns the
//! cell file, the signing key, the firewall and the services. The agent
//! never touches the key: `cell check` runs as root, runs the probes as the
//! agent, and signs what it saw.

use crate::cell::Cell;
use crate::grant::{Harness, EDGE_SUFFIX};
use crate::log;
use crate::prim::Hash;
use crate::said::{Said, Secret, State, Statement};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

pub const ETC: &str = "/etc/cell";
pub const SAID: &str = "/var/lib/cell/said";
pub const BIN: &str = "/usr/local/bin/cell";
pub const INBOX: &str = "/var/lib/cell/inbox";
/// Where the agent drops what it wants said; the shipper signs it.
pub const OUTBOX: &str = "/var/lib/cell/outbox";
pub const AGENT: &str = "agent";
pub const PROXY_USER: &str = "cell-proxy";
pub const DOOR_USER: &str = "cell-door";
/// Owns the key and the log; the only user besides root that can sign.
pub const SAY_USER: &str = "cell-say";
/// The agent's home, where the adapter keeps its work and its sessions.
pub const AGENT_HOME: &str = "/home/agent";
/// Where the harness binary goes, root-owned, for the agent to run.
pub const CLAUDE_BIN: &str = "/usr/local/bin/claude";
/// Claude Code behind the wall: no updater, since the binary is root's; no
/// claude.ai connectors, since the account's services are not the cell's;
/// no optional telemetry. The wall refuses those roads anyway; this keeps
/// the harness from trying them.
pub const CLAUDE_ENV: &str =
    "DISABLE_AUTOUPDATER=1 ENABLE_CLAUDEAI_MCP_SERVERS=false CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1";
pub const PROXY_ADDR: &str = "127.0.0.1:3128";
/// Where exe.dev's in-VM auth proxy delivers the VM's web traffic, peer
/// knocks included, from loopback.
pub const DOOR_ADDR: &str = "127.0.0.1:8000";
/// exe.dev's resolver, as /etc/resolv.conf names it inside a VM.
pub const RESOLVER: &str = "169.254.169.254";

type Fallible<T = ()> = Result<T, Box<dyn Error>>;

fn etc(file: &str) -> PathBuf {
    Path::new(ETC).join(file)
}

/// Run a command; fail with its stderr if it fails.
fn run(cmd: &str, args: &[&str]) -> Fallible<String> {
    let out = Command::new(cmd).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "{cmd} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Exit status and combined output, however it went.
fn attempt(cmd: &str, args: &[&str]) -> (bool, String) {
    match Command::new(cmd).args(args).output() {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), s.trim().to_string())
        }
        Err(e) => (false, e.to_string()),
    }
}

fn write_as_root(path: &Path, bytes: &[u8], mode: u32) -> Fallible {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn user_exists(name: &str) -> bool {
    attempt("id", &["-u", name]).0
}

fn wall_nft() -> String {
    format!(
        "table inet cell {{}}
flush table inet cell
table inet cell {{
  chain out {{
    type filter hook output priority filter; policy accept;
    # the agent reaches the proxy on loopback and nothing else, in any protocol
    meta skuid \"{AGENT}\" oif \"lo\" tcp dport {port} accept
    meta skuid \"{AGENT}\" counter reject
    # the proxy answers the agent, resolves at the gateway, and opens tls ports
    meta skuid \"{PROXY_USER}\" oif \"lo\" accept
    meta skuid \"{PROXY_USER}\" ip daddr {RESOLVER} udp dport 53 accept
    meta skuid \"{PROXY_USER}\" ip daddr {RESOLVER} tcp dport 53 accept
    meta skuid \"{PROXY_USER}\" tcp dport {{ 443, 8443 }} accept
    meta skuid \"{PROXY_USER}\" counter reject
  }}
}}
",
        port = PROXY_ADDR.rsplit(':').next().unwrap_or("3128")
    )
}

fn proxy_unit() -> String {
    format!(
        "[Unit]
Description=cell proxy: the wall's one road out
After=network-online.target

[Service]
User={PROXY_USER}
ExecStart={BIN} proxy --listen {PROXY_ADDR} --cell {ETC}/cell.json
Restart=always
RestartSec=1
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
"
    )
}

fn door_unit() -> String {
    format!(
        "[Unit]
Description=cell door: the one way in, and the log read out
After=network-online.target

[Service]
User={DOOR_USER}
ExecStart={BIN} door --listen {DOOR_ADDR} --cell {ETC}/cell.json --said {SAID} --inbox {INBOX}
Restart=always
RestartSec=1
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths={INBOX}

[Install]
WantedBy=multi-user.target
"
    )
}

/// Claude Code, installed by root with root's open internet, then copied
/// where the agent can run it and nobody can change it. The agent cannot
/// update it: its only road is the proxy.
fn install_claude() -> Fallible {
    if Path::new(CLAUDE_BIN).exists() {
        return Ok(());
    }
    run(
        "bash",
        &["-c", "curl -fsSL https://claude.ai/install.sh | bash"],
    )?;
    let installed = run("readlink", &["-f", "/root/.local/bin/claude"])?;
    let tmp = format!("{CLAUDE_BIN}.tmp");
    fs::copy(installed.trim(), &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, CLAUDE_BIN)?;
    Ok(())
}

fn agent_unit() -> String {
    format!(
        "[Unit]
Description=cell agent: runs the harness on what the inbox brings
After=cell-say.service

[Service]
User={AGENT}
WorkingDirectory={AGENT_HOME}
Environment=HOME={AGENT_HOME}
Environment=HTTPS_PROXY=http://{PROXY_ADDR} HTTP_PROXY=http://{PROXY_ADDR} NO_PROXY=
Environment=https_proxy=http://{PROXY_ADDR} http_proxy=http://{PROXY_ADDR} no_proxy=
Environment={CLAUDE_ENV}
ExecStart={BIN} run --cell {ETC}/cell.json --inbox {INBOX} --outbox {OUTBOX} --home {AGENT_HOME}
Restart=always
RestartSec=2
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
"
    )
}

fn say_unit() -> String {
    format!(
        "[Unit]
Description=cell say: signs what the agent drops in the outbox
After=local-fs.target

[Service]
User={SAY_USER}
ExecStart={BIN} ship --cell {ETC}/cell.json --secret {ETC}/secret --outbox {OUTBOX} --said {SAID}
Restart=always
RestartSec=1
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths={OUTBOX} {SAID}

[Install]
WantedBy=multi-user.target
"
    )
}

fn wall_unit() -> String {
    format!(
        "[Unit]
Description=cell wall: what each user may open
DefaultDependencies=no
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f {ETC}/wall.nft

[Install]
WantedBy=multi-user.target
"
    )
}

fn profile() -> String {
    format!(
        "# the agent's roads all go through the proxy
if [ \"$(id -un)\" = {AGENT} ]; then
  export HTTPS_PROXY=http://{PROXY_ADDR} HTTP_PROXY=http://{PROXY_ADDR} NO_PROXY=
  export https_proxy=$HTTPS_PROXY http_proxy=$HTTP_PROXY no_proxy=
  export {CLAUDE_ENV}
fi
"
    )
}

/// Build the wall. Idempotent: run it again after changing the cell file.
pub fn setup(cell_path: &Path) -> Fallible<Statement> {
    if run("id", &["-u"])?.trim() != "0" {
        return Err("setup runs as root".into());
    }
    let cell: Cell = serde_json::from_str(&fs::read_to_string(cell_path)?)?;
    fs::create_dir_all(ETC)?;
    fs::create_dir_all(SAID)?;
    fs::create_dir_all(INBOX)?;
    fs::create_dir_all(OUTBOX)?;

    let system = [
        "--system",
        "--no-create-home",
        "--shell",
        "/usr/sbin/nologin",
    ];
    for user in [PROXY_USER, DOOR_USER, SAY_USER] {
        if !user_exists(user) {
            run("useradd", &[&system[..], &[user]].concat())?;
        }
    }
    if !user_exists(AGENT) {
        run("useradd", &["--create-home", "--shell", "/bin/bash", AGENT])?;
    }
    for group in ["sudo", "admin", "wheel", "docker", "adm"] {
        let _ = attempt("gpasswd", &["-d", AGENT, group]);
    }
    run("chown", &[&format!("{DOOR_USER}:{DOOR_USER}"), INBOX])?;
    fs::set_permissions(INBOX, fs::Permissions::from_mode(0o755))?;
    // the log and its lock are the shipper's; root appends as root
    let lock = Path::new(SAID).join(log::LOCK);
    if !lock.exists() {
        write_as_root(&lock, b"", 0o644)?;
    }
    run(
        "chown",
        &[
            &format!("{SAY_USER}:{SAY_USER}"),
            SAID,
            &lock.to_string_lossy(),
        ],
    )?;
    fs::set_permissions(SAID, fs::Permissions::from_mode(0o755))?;
    // the agent drops, the shipper takes: both may write, nobody else
    run("chown", &[&format!("{SAY_USER}:{AGENT}"), OUTBOX])?;
    fs::set_permissions(OUTBOX, fs::Permissions::from_mode(0o775))?;

    let me = std::env::current_exe()?;
    if me != Path::new(BIN) {
        let tmp = format!("{BIN}.tmp");
        fs::copy(&me, &tmp)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, BIN)?;
    }

    write_as_root(&etc("cell.json"), &cell.canonical(), 0o644)?;
    let secret_path = etc("secret");
    if !secret_path.exists() {
        write_as_root(
            &secret_path,
            format!("{}\n", Secret::generate()).as_bytes(),
            0o600,
        )?;
    }
    run(
        "chown",
        &[
            &format!("{SAY_USER}:{SAY_USER}"),
            &secret_path.to_string_lossy(),
        ],
    )?;
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600))?;
    let secret: Secret = fs::read_to_string(&secret_path)?.parse()?;
    write_as_root(
        &etc("key.pub"),
        format!("{}\n", secret.public()).as_bytes(),
        0o644,
    )?;
    write_as_root(&etc("wall.nft"), wall_nft().as_bytes(), 0o644)?;
    write_as_root(
        Path::new("/etc/systemd/system/cell-proxy.service"),
        proxy_unit().as_bytes(),
        0o644,
    )?;
    write_as_root(
        Path::new("/etc/systemd/system/cell-door.service"),
        door_unit().as_bytes(),
        0o644,
    )?;
    write_as_root(
        Path::new("/etc/systemd/system/cell-say.service"),
        say_unit().as_bytes(),
        0o644,
    )?;
    if cell.harnesses().contains(&Harness::Claude) {
        install_claude()?;
    }
    write_as_root(
        Path::new("/etc/systemd/system/cell-agent.service"),
        agent_unit().as_bytes(),
        0o644,
    )?;
    write_as_root(
        Path::new("/etc/systemd/system/cell-wall.service"),
        wall_unit().as_bytes(),
        0o644,
    )?;
    write_as_root(
        Path::new("/etc/profile.d/cell.sh"),
        profile().as_bytes(),
        0o644,
    )?;

    run("systemctl", &["daemon-reload"])?;
    run("systemctl", &["enable", "--now", "cell-wall"])?;
    run("nft", &["-f", &etc("wall.nft").to_string_lossy()])?;
    let units = ["cell-proxy", "cell-door", "cell-say", "cell-agent"];
    run("systemctl", &[&["enable"][..], &units].concat())?;
    run("systemctl", &[&["restart"][..], &units].concat())?;
    std::thread::sleep(Duration::from_millis(300));

    check()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub name: String,
    pub expect: String,
    pub got: String,
    pub detail: String,
}

impl Probe {
    fn new(name: &str, expect: &str, got: &str, detail: impl Into<String>) -> Probe {
        Probe {
            name: name.into(),
            expect: expect.into(),
            got: got.into(),
            detail: detail.into(),
        }
    }

    pub fn ok(&self) -> bool {
        self.expect == self.got
    }
}

fn tcp(name: &str, addr: &str) -> Probe {
    let addr: SocketAddr = addr.parse().expect("literal");
    match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(_) => Probe::new(name, "refused", "connected", ""),
        Err(e) if e.kind() == ErrorKind::TimedOut => {
            Probe::new(name, "refused", "refused", "timed out")
        }
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

fn udp(name: &str, addr: &str) -> Probe {
    // a minimal DNS query for example.com; any reply at all means a road exists
    let query: &[u8] =
        b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01";
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
        return Probe::new(name, "refused", "refused", "cannot bind");
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
    if let Err(e) = sock.send_to(query, addr) {
        return Probe::new(name, "refused", "refused", e.to_string());
    }
    let mut buf = [0u8; 512];
    match sock.recv_from(&mut buf) {
        Ok((n, _)) => Probe::new(name, "refused", "answered", format!("{n} bytes back")),
        Err(e) => Probe::new(name, "refused", "refused", format!("sent, then {e}")),
    }
}

fn resolve(name: &str, host: &str) -> Probe {
    match (host, 443).to_socket_addrs().map(Iterator::count) {
        Ok(n) if n > 0 => Probe::new(name, "refused", "resolved", format!("{n} addresses")),
        Ok(_) => Probe::new(name, "refused", "refused", "no addresses"),
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

/// Ask the proxy for a tunnel; report its status line.
fn connect_via_proxy(target: &str) -> Result<String, String> {
    let addr: SocketAddr = PROXY_ADDR.parse().expect("literal");
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = String::from_utf8_lossy(&buf);
    Ok(line.lines().next().unwrap_or("").to_string())
}

fn via_proxy(name: &str, target: &str, expect: &str) -> Probe {
    match connect_via_proxy(target) {
        Ok(line) if line.contains(" 200 ") => Probe::new(name, expect, "tunnelled", line),
        Ok(line) => Probe::new(name, expect, "refused", line),
        Err(e) => Probe::new(name, expect, "unreachable", e),
    }
}

/// curl through the proxy: the whole road, TLS included.
fn curl(name: &str, url: &str, expect: &str) -> Probe {
    let (_, out) = attempt(
        "curl",
        &[
            "-s",
            "-o",
            "/dev/null",
            "-m",
            "20",
            "-w",
            "%{http_code}",
            "-x",
            &format!("http://{PROXY_ADDR}"),
            url,
        ],
    );
    let code = out.trim().to_string();
    let got = match code.as_str() {
        "000" | "" => "unreachable",
        "403" if url.contains(EDGE_SUFFIX) => "gateway-refused",
        _ => "answered",
    };
    Probe::new(name, expect, got, format!("http {code}"))
}

fn curl_direct(name: &str, url: &str) -> Probe {
    let (_, out) = attempt(
        "curl",
        &[
            "-s",
            "-o",
            "/dev/null",
            "-m",
            "10",
            "-w",
            "%{http_code}",
            "--noproxy",
            "*",
            url,
        ],
    );
    let code = out.trim().to_string();
    let got = if code == "000" || code.is_empty() {
        "refused"
    } else {
        "answered"
    };
    Probe::new(name, "refused", got, format!("http {code}"))
}

/// A root daemon's socket is root by another road.
fn unix(name: &str, path: &str) -> Probe {
    match UnixStream::connect(path) {
        Ok(_) => Probe::new(name, "refused", "connected", ""),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Probe::new(name, "refused", "refused", "absent")
        }
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

fn read_file(name: &str, path: &Path) -> Probe {
    match fs::read(path) {
        Ok(b) => Probe::new(name, "refused", "readable", format!("{} bytes", b.len())),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            Probe::new(name, "refused", "refused", "")
        }
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

fn write_file(name: &str, path: &Path) -> Probe {
    match fs::OpenOptions::new().append(true).open(path) {
        Ok(_) => Probe::new(name, "refused", "writable", ""),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            Probe::new(name, "refused", "refused", "")
        }
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

/// Try to make a file in a directory.
fn create_in(name: &str, dir: &str) -> Probe {
    let path = Path::new(dir).join(format!(".probe-{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(_) => {
            let _ = fs::remove_file(&path);
            Probe::new(name, "refused", "writable", "")
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            Probe::new(name, "refused", "refused", "")
        }
        Err(e) => Probe::new(name, "refused", "refused", e.to_string()),
    }
}

/// Drop a Said in the outbox the way the agent does, tmp then rename, and
/// wait for the shipper's answer: a receipt naming a sequence number, or a
/// refusal with its reason. Cleans up after itself.
fn drop_in_outbox(stem: &str, body: &str) -> Result<Result<u64, String>, String> {
    let at = |ext: &str| Path::new(OUTBOX).join(format!("{stem}.{ext}"));
    fs::write(at("tmp"), body)
        .and_then(|()| fs::rename(at("tmp"), at("json")))
        .map_err(|e| e.to_string())?;
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        if let Ok(why) = fs::read_to_string(at("refused")) {
            let _ = fs::remove_file(at("refused"));
            return Ok(Err(why.trim().to_string()));
        }
        let receipt = fs::read_dir(OUTBOX)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .find_map(|n| n.strip_prefix(&format!("{stem}."))?.parse::<u64>().ok());
        if let Some(seq) = receipt {
            let _ = fs::remove_file(at(&seq.to_string()));
            return Ok(Ok(seq));
        }
    }
    Err("no answer in 5 s".into())
}

fn voice(cell: Option<&Cell>) -> Probe {
    let name = "the voice: a session statement through the outbox";
    let Some(harness) = cell.and_then(|c| c.harnesses().into_iter().next()) else {
        return Probe::new(name, "none", "none", "the cell runs no harness");
    };
    let said = Said::Session {
        name: "wall-check".parse().expect("a name"),
        harness,
        state: State::Idle,
        text: Some("the agent can speak".into()),
    };
    let body = serde_json::to_string(&said).expect("a Said serializes");
    match drop_in_outbox(&format!("probe-{}-voice", std::process::id()), &body) {
        Ok(Ok(seq)) => Probe::new(name, "signed", "signed", format!("seq {seq}")),
        Ok(Err(why)) => Probe::new(name, "signed", "refused", why),
        Err(e) => Probe::new(name, "signed", "unanswered", e),
    }
}

fn forge() -> Probe {
    let name = "the voice: a forged wall check";
    let body = format!(
        r#"{{"checked":{{"cell":"{}","ok":true,"report":"forged"}}}}"#,
        "0".repeat(64)
    );
    match drop_in_outbox(&format!("probe-{}-forge", std::process::id()), &body) {
        Ok(Err(why)) => Probe::new(name, "refused", "refused", why),
        Ok(Ok(seq)) => Probe::new(name, "refused", "signed", format!("seq {seq}")),
        Err(e) => Probe::new(name, "refused", "unanswered", e),
    }
}

fn command(name: &str, cmd: &str, args: &[&str]) -> Probe {
    let (ok, out) = attempt(cmd, args);
    let line = out
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect::<String>();
    if ok {
        Probe::new(name, "refused", "allowed", line)
    } else {
        Probe::new(name, "refused", "refused", line)
    }
}

/// Everything the threat model says the agent cannot do, tried. Runs as the
/// agent; prints one JSON line per probe.
pub fn probe() -> Vec<Probe> {
    let cell: Option<Cell> = fs::read_to_string(etc("cell.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let allowed_host = cell.as_ref().and_then(|c| {
        c.allowlist()
            .into_iter()
            .find(|h| !h.as_str().ends_with(EDGE_SUFFIX))
    });

    let mut p = vec![
        tcp("direct tcp to a public address", "1.1.1.1:443"),
        tcp("direct tcp to the gateway", &format!("{RESOLVER}:80")),
        tcp("tcp to local sshd", "127.0.0.1:22"),
        tcp("knock on our own door from inside", DOOR_ADDR),
        tcp("direct tcp over ipv6", "[2606:4700:4700::1111]:443"),
        udp("dns straight to the gateway", &format!("{RESOLVER}:53")),
        udp("dns to a public resolver", "8.8.8.8:53"),
        resolve("resolve a name ourselves", "example.com"),
        curl_direct(
            "https to the internet without the proxy",
            "https://example.com/",
        ),
        via_proxy(
            "proxy: a host off the allowlist",
            "example.com:443",
            "refused",
        ),
        via_proxy(
            "proxy: the account's claude.ai connectors",
            "mcp-proxy.anthropic.com:443",
            "refused",
        ),
        via_proxy("proxy: an address literal", "1.1.1.1:443", "refused"),
        via_proxy(
            "proxy: an allowed host on port 80",
            "api.anthropic.com:80",
            "refused",
        ),
        via_proxy(
            "proxy: a door name, tunnelled to the gateway",
            "door-nobody.int.exe.xyz:443",
            "tunnelled",
        ),
        curl(
            "gateway: a door nobody attached",
            "https://door-nobody.int.exe.xyz/",
            "gateway-refused",
        ),
        unix("docker's socket", "/run/docker.sock"),
        unix("snapd's socket", "/run/snapd.socket"),
        read_file("read the signing key", &etc("secret")),
        write_file("write the cell file", &etc("cell.json")),
        create_in("write the inbox", INBOX),
        create_in("write the log", SAID),
        write_file("write the wall", &etc("wall.nft")),
        write_file(
            "write the proxy unit",
            Path::new("/etc/systemd/system/cell-proxy.service"),
        ),
        command("sudo", "sudo", &["-n", "true"]),
        command("stop the proxy", "systemctl", &["stop", "cell-proxy"]),
        command("read the firewall", "nft", &["list", "ruleset"]),
        voice(cell.as_ref()),
        forge(),
    ];
    match allowed_host {
        Some(h) => {
            p.push(via_proxy(
                "proxy: an allowed host",
                &format!("{h}:443"),
                "tunnelled",
            ));
            p.push(curl(
                "the whole road to an allowed host",
                &format!("https://{h}/"),
                "answered",
            ));
        }
        None => p.push(Probe::new(
            "an allowed host",
            "none",
            "none",
            "the cell reaches nothing",
        )),
    }
    p
}

/// Run the probes as the agent, add what root can see, sign the result as
/// `Checked`, and append it to the statement log.
pub fn check() -> Fallible<Statement> {
    if run("id", &["-u"])?.trim() != "0" {
        return Err("check runs as root".into());
    }
    let cell_bytes = fs::read(etc("cell.json"))?;
    let cell: Cell = serde_json::from_slice(&cell_bytes)?;
    let hash = Hash::of(&cell_bytes);
    let secret: Secret = fs::read_to_string(etc("secret"))?.parse()?;

    let raw = run("runuser", &["-u", AGENT, "--", BIN, "probe"])?;
    let mut probes: Vec<Probe> = raw
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;

    let (_, sudo) = attempt("sudo", &["-l", "-U", AGENT]);
    probes.push(Probe::new(
        "root: sudo rules for the agent",
        "none",
        if sudo.contains("is not allowed to run sudo") || sudo.contains("unknown user") {
            "none"
        } else {
            "some"
        },
        sudo.lines().last().unwrap_or(""),
    ));
    let (_, groups) = attempt("id", &["-nG", AGENT]);
    let risky: Vec<&str> = groups
        .split_whitespace()
        .filter(|g| ["sudo", "admin", "wheel", "docker", "adm", "root"].contains(g))
        .collect();
    probes.push(Probe::new(
        "root: the agent's groups",
        "harmless",
        if risky.is_empty() {
            "harmless"
        } else {
            "risky"
        },
        groups.trim(),
    ));
    let (_, owner) = attempt("stat", &["-c", "%U:%a", &etc("secret").to_string_lossy()]);
    probes.push(Probe::new(
        "root: the key's owner and mode",
        &format!("{SAY_USER}:600"),
        owner.trim(),
        "",
    ));
    for unit in ["cell-proxy", "cell-door", "cell-say", "cell-agent"] {
        let (active, _) = attempt("systemctl", &["is-active", "--quiet", unit]);
        probes.push(Probe::new(
            &format!("root: {unit} service"),
            "active",
            if active { "active" } else { "down" },
            "",
        ));
    }
    let (walled, _) = attempt("nft", &["list", "chain", "inet", "cell", "out"]);
    probes.push(Probe::new(
        "root: wall loaded",
        "loaded",
        if walled { "loaded" } else { "missing" },
        "",
    ));

    let ok = probes.iter().all(Probe::ok);
    let report = probes
        .iter()
        .map(|p| {
            let mark = if p.ok() { "ok " } else { "BAD" };
            let detail = if p.detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", p.detail)
            };
            format!("{mark} {}: {}{detail}", p.name, p.got)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let statement = log::append(
        Path::new(SAID),
        &secret,
        cell.name.clone(),
        Said::Checked {
            cell: hash,
            ok,
            report,
            binary: Hash::of(&fs::read(BIN)?),
            signed: signed_in(),
        },
    )?;
    Ok(statement)
}

/// Where each harness keeps its credential on the box. Claude Code writes
/// its OAuth tokens here on Linux. Codex keeps none on the box: its
/// credential is the edge's, so attachment there is its sign-in. Pi
/// borrows whichever is present.
const CREDENTIALS: &[(Harness, &str)] =
    &[(Harness::Claude, "/home/agent/.claude/.credentials.json")];

fn signed_in() -> BTreeSet<Harness> {
    CREDENTIALS
        .iter()
        .filter(|(_, path)| fs::metadata(path).is_ok_and(|m| m.len() > 0))
        .map(|(h, _)| *h)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_names_both_users_and_only_the_proxy_port_for_the_agent() {
        let nft = wall_nft();
        assert!(nft.contains("meta skuid \"agent\" oif \"lo\" tcp dport 3128 accept"));
        assert!(nft.contains("meta skuid \"agent\" counter reject"));
        assert!(nft.contains("meta skuid \"cell-proxy\" tcp dport { 443, 8443 } accept"));
        assert!(nft.contains("meta skuid \"cell-proxy\" counter reject"));
        assert!(
            nft.starts_with("table inet cell {}\nflush table inet cell\n"),
            "idempotent load"
        );
        assert_eq!(nft.matches("accept").count(), 6);
    }

    #[test]
    fn the_proxy_runs_unprivileged_and_the_agent_is_pointed_at_it() {
        assert!(proxy_unit().contains("User=cell-proxy"));
        assert!(proxy_unit().contains("NoNewPrivileges=yes"));
        assert!(say_unit().contains("User=cell-say"));
        assert!(agent_unit().contains("User=agent"));
        assert!(agent_unit().contains("Environment=HTTPS_PROXY=http://127.0.0.1:3128"));
        assert!(say_unit().contains("ReadWritePaths=/var/lib/cell/outbox /var/lib/cell/said"));
        assert!(profile().contains("HTTPS_PROXY=http://127.0.0.1:3128"));
        assert!(profile().contains("= agent ]"));
    }

    #[test]
    fn a_probe_passes_only_when_it_got_what_it_expected() {
        assert!(Probe::new("x", "refused", "refused", "").ok());
        assert!(!Probe::new("x", "refused", "connected", "").ok());
        let p = Probe::new("x", "tunnelled", "tunnelled", "HTTP/1.1 200");
        let back: Probe = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
