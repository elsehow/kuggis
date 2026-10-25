#!/usr/bin/env python3
"""Trusted host driver for a disposable VM. Never run on a production cell."""
from dataclasses import asdict, dataclass
from enum import Enum
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess as sp
import sys
import time
import traceback
import uuid

BASE = Path("/opt/kuggis-bench")
STATE = Path("/var/lib/kuggis-bench")
ETC = Path("/etc/kuggis-bench")
ROOT = Path("/var/lib/machines/cellbench")
OUTBOX = Path("/var/lib/kuggis-bench-outbox")
CELL = str(BASE / "cell")


@dataclass(frozen=True)
class BoundarySpec:
    machine: str = "cellbench"
    uid_base: int = 524288
    uid_count: int = 65536
    interface: str = "ve-cellbench"
    host_ip: str = "10.203.0.1"
    guest_ip: str = "10.203.0.2"
    proxy_port: int = 3128


SPEC = BoundarySpec()
PROXY = f"http://{SPEC.host_ip}:{SPEC.proxy_port}"


class Status(str, Enum):
    PASS = "pass"
    FAIL = "fail"
    UNVERIFIED = "unverified"


@dataclass
class Result:
    id: str
    expected: str
    status: Status = Status.UNVERIFIED
    detail: str = "Not run"


CASES = {
    "namespaces": "Guest root maps to an unprivileged host UID; user/mount/PID/net namespaces differ",
    "host-controls": "Host can reach test TCP IPv4/IPv6, DNS, SSH-port canary and forbidden HTTPS targets",
    "minimal-start": "Initial guest has no Cargo, Node, npm or build-essential",
    "sudo": "A normal guest user can become root and edit system configuration",
    "toolchains": "Guest installs Rust, Node, npm and build-essential through the real proxy",
    "rust-build": "Rust project fetches a crate, builds and passes a test",
    "node-build": "Node project fetches an npm package and passes a test",
    "kuggis-rust": "Current Kuggis Rust source passes cargo test --locked",
    "kuggis-web": "Kuggis frontend builds and its integration tests pass",
    "local-service": "Guest starts a persistent service and connects through localhost",
    "root-boundary": "Guest root cannot reach forbidden HTTPS, direct TCP, host SSH, gateway, DNS or sockets",
    "other-user-boundary": "A newly created guest user is subject to the same boundary",
    "tampered-network": "Clearing proxies and changing guest firewall/routes doesn't bypass the host wall",
    "host-files": "Guest root cannot read/modify host key, host config or host process namespace",
    "declaration-tamper": "Replacing the guest's declaration doesn't change effective proxy grants",
    "signed-outbox": "Real shipper signs permitted statements and refuses forged Checked records",
    "container-restart": "Tools, config and service survive container restart",
    "reapply": "Reapplying host boundary twice preserves guest software/config and isolation",
    "proxy-down": "Stopping proxy blocks allowed HTTPS with no direct fallback; recovery works",
    "missing-wall": "Container start fails when host firewall is absent; restored wall permits start",
    "broken-guest": "Stopping guest service leaves host controls intact and host can recover it",
    "vm-restart": "Environment and host-enforced isolation survive a real VM reboot",
}
RESULTS = {k: Result(k, v) for k, v in CASES.items()}
SEQ = 0
EVIDENCE_ID = uuid.uuid4().hex


def run(args, *, timeout=120, check=True, input=None):
    global SEQ
    SEQ += 1
    command = [str(a) for a in args]
    print("COMMAND " + shlex.join(command)[:180], flush=True)
    start = time.monotonic()
    try:
        out = sp.run(command, input=input, capture_output=True, timeout=timeout, text=True)
    except sp.TimeoutExpired as e:
        def string(x):
            return x.decode(errors="replace") if isinstance(x, bytes) else x or ""
        out = sp.CompletedProcess(command, 124, string(e.stdout), string(e.stderr) + "\nTIMED OUT")
    evidence = {"argv": command, "returncode": out.returncode,
                "seconds": round(time.monotonic() - start, 3), "stdout": out.stdout, "stderr": out.stderr}
    (STATE / "commands").mkdir(parents=True, exist_ok=True)
    (STATE / "commands" / f"{EVIDENCE_ID}-{SEQ:04d}.json").write_text(json.dumps(evidence, indent=2))
    if check and out.returncode:
        raise RuntimeError(f"{shlex.join(command)[:180]}: exit {out.returncode}\n{out.stderr[-2000:]}\n{out.stdout[-1000:]}")
    return out


def shell(script, **kwargs):
    return run(["/bin/bash", "-euc", script], **kwargs)


def guest(script, *, user="root", **kwargs):
    return run(["systemd-run", "--machine=" + SPEC.machine, "--quiet", "--wait", "--pipe", "--expand-environment=no",
                "--property=RuntimeMaxSec=" + str(kwargs.get("timeout", 120) + 5),
                "--uid=" + user, "/usr/bin/env", "HOME=" + ("/root" if user == "root" else f"/home/{user}"),
                "PATH=/root/.cargo/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HTTPS_PROXY=" + PROXY, "HTTP_PROXY=" + PROXY,
                "https_proxy=" + PROXY, "http_proxy=" + PROXY,
                "NO_PROXY=localhost,127.0.0.1,::1", "no_proxy=localhost,127.0.0.1,::1",
                "/bin/bash", "-euc", script], **kwargs)


def write(path, text, mode=0o644):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(mode)


def load():
    if (STATE / "report.json").exists():
        for r in json.loads((STATE / "report.json").read_text())["results"]:
            if r["id"] in RESULTS:
                RESULTS[r["id"]] = Result(**r)


def save():
    STATE.mkdir(parents=True, exist_ok=True)
    counts = {s.value: sum(r.status == s for r in RESULTS.values()) for s in Status}
    report = {"schema": 1, "boundary": asdict(SPEC), "summary": counts,
              "results": [asdict(r) for r in RESULTS.values()]}
    write(STATE / "report.json", json.dumps(report, indent=2) + "\n")
    rows = ["# Environment autonomy bench", "", f"{counts}", "", "| Test | Status | Expected |", "|---|---|---|"]
    rows += [f"| {r.id} | {r.status.value if isinstance(r.status, Status) else r.status} | {r.expected} |" for r in RESULTS.values()]
    rows += ["", "## Details", ""]
    for r in RESULTS.values():
        rows += [f"### {r.id}", "", r.detail, ""]
    write(STATE / "report.md", "\n".join(rows))


class Unverified(Exception):
    pass


def case(name, fn, requires=()):
    print(f"TEST {name}: {CASES[name]}", flush=True)
    try:
        missing = [r for r in requires if RESULTS[r].status != Status.PASS]
        if missing:
            raise Unverified("Prerequisites did not pass: " + ", ".join(missing))
        detail = fn()
        RESULTS[name] = Result(name, CASES[name], Status.PASS, detail or "Matched expectation; see command evidence.")
    except Unverified as e:
        RESULTS[name] = Result(name, CASES[name], Status.UNVERIFIED, str(e))
    except Exception as e:
        RESULTS[name] = Result(name, CASES[name], Status.FAIL, str(e))
    print(f"{RESULTS[name].status}: {name}", flush=True)
    save()


def inspect():
    out = shell("uname -a; cat /etc/os-release; systemd --version; cat /proc/sys/user/max_user_namespaces; findmnt /; df -h /")
    write(STATE / "platform.txt", out.stdout)
    print(out.stdout, flush=True)
    run(["unshare", "--user", "--map-root-user", "/usr/bin/id"])


def firewall():
    rules = f"""table inet kuggis_bench {{}}
flush table inet kuggis_bench
table inet kuggis_bench {{
 chain input {{
  type filter hook input priority -200; policy accept;
  iifname "{SPEC.interface}" ip daddr {SPEC.host_ip} tcp dport {SPEC.proxy_port} counter accept
  iifname "{SPEC.interface}" counter reject
  iifname != "lo" tcp dport {SPEC.proxy_port} counter reject
 }}
 chain forward {{
  type filter hook forward priority -200; policy accept;
  iifname "{SPEC.interface}" counter reject
 }}
}}
"""
    write(ETC / "wall.nft", rules)
    run(["nft", "-f", ETC / "wall.nft"])


def guard():
    current = json.loads(run(["nft", "-j", "list", "table", "inet", "kuggis_bench"]).stdout)
    # Compare full ordered rule expressions and chain hooks, ignoring counters/handles.
    def normalize(v):
        if isinstance(v, list):
            return [normalize(i) for i in v if not isinstance(i, dict) or "metainfo" not in i]
        if isinstance(v, dict):
            return {k: ({} if k == "counter" else normalize(val)) for k, val in v.items()
                    if k not in {"handle", "index"}}
        return v
    wanted = json.loads((ETC / "wall.expected.json").read_text())
    if normalize(current) != normalize(wanted):
        raise RuntimeError("Live firewall differs from the host-approved rules")


def network_up():
    for _ in range(100):
        if run(["ip", "link", "show", SPEC.interface], check=False).returncode == 0:
            break
        time.sleep(.1)
    run(["ip", "address", "replace", SPEC.host_ip + "/30", "dev", SPEC.interface])
    run(["ip", "-6", "address", "replace", "fd42:203::1/64", "dev", SPEC.interface])
    run(["ip", "link", "set", SPEC.interface, "up"])


def setup():
    if not (BASE / "source.tar").exists():
        raise RuntimeError("Missing controller payload")
    shell("export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; apt-get install -y -qq systemd-container debootstrap nftables python3 curl ca-certificates iproute2", timeout=600)
    if not (ROOT / "etc/os-release").exists():
        run(["debootstrap", "--variant=minbase", "--include=systemd,systemd-sysv,dbus,ca-certificates,curl,python3,sudo,iproute2,nftables",
             "noble", ROOT, "https://archive.ubuntu.com/ubuntu"], timeout=900)
    # Setup is an initializer: never rewrite an existing guest filesystem.
    if not (ETC / "initialized").exists():
        write(ROOT / "etc/apt/sources.list", "deb https://archive.ubuntu.com/ubuntu noble main universe\ndeb https://archive.ubuntu.com/ubuntu noble-updates main universe\ndeb https://security.ubuntu.com/ubuntu noble-security main universe\n")
        write(ROOT / "etc/apt/apt.conf.d/80bench-proxy", f'Acquire::https::Proxy "{PROXY}";\nAcquire::http::Proxy "{PROXY}";\n')
        write(ROOT / "etc/systemd/system/bench-network.service", f"""[Unit]
Description=Bench guest network
Before=network.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/ip link set host0 up
ExecStart=/usr/sbin/ip address replace {SPEC.guest_ip}/30 dev host0
ExecStart=/usr/sbin/ip route replace default via {SPEC.host_ip}
ExecStart=/usr/sbin/ip -6 address replace fd42:203::2/64 dev host0
ExecStart=/usr/sbin/ip -6 route replace default via fd42:203::1
[Install]
WantedBy=multi-user.target
""")
        wants = ROOT / "etc/systemd/system/multi-user.target.wants"
        wants.mkdir(parents=True, exist_ok=True)
        (wants / "bench-network.service").symlink_to("../bench-network.service")
        for unit in ["systemd-networkd.service", "systemd-networkd.socket", "systemd-resolved.service"]:
            p = ROOT / "etc/systemd/system" / unit
            p.unlink(missing_ok=True)
            p.symlink_to("/dev/null")
        (ROOT / "etc/resolv.conf").unlink(missing_ok=True)
        write(ROOT / "etc/resolv.conf", "nameserver 1.1.1.1\noptions timeout:1 attempts:1\n")
        shutil.copy(BASE / "guest.py", ROOT / "root/guest.py")
        run(["chroot", ROOT, "useradd", "-m", "-s", "/bin/bash", "developer"])
        write(ROOT / "etc/sudoers.d/developer", "developer ALL=(ALL) NOPASSWD: ALL\n", 0o440)
        write(ETC / "initialized", "yes\n")
    hosts = ["archive.ubuntu.com", "security.ubuntu.com", "static.rust-lang.org", "nodejs.org",
             "registry.npmjs.org", "index.crates.io", "static.crates.io"]
    write(ETC / "cell.json", json.dumps({"name": "envbench", "grants": [{"host": h} for h in hosts]}))
    write(STATE / "boundary.json", json.dumps(asdict(SPEC), indent=2))
    if not (ETC / "secret").exists():
        run([CELL, "key", "new", ETC / "secret"])
    (STATE / "said").mkdir(exist_ok=True)
    OUTBOX.mkdir(exist_ok=True)
    os.chown(OUTBOX, SPEC.uid_base, SPEC.uid_base)
    OUTBOX.chmod(0o700)
    run(["id", "benchproxy"], check=False)
    shell("id benchproxy >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin benchproxy")
    write("/etc/systemd/network/00-bench.network", f"[Match]\nName={SPEC.interface}\n[Link]\nUnmanaged=yes\n")
    firewall()
    write(ETC / "wall.expected.json", run(["nft", "-j", "list", "table", "inet", "kuggis_bench"]).stdout)
    write("/etc/systemd/system/bench-wall.service", f"""[Unit]
Description=Bench host firewall
Before=bench-container.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f {ETC}/wall.nft
[Install]
WantedBy=multi-user.target
""")
    write("/etc/systemd/system/bench-container.service", f"""[Unit]
Description=Persistent autonomous bench environment
Requires=bench-wall.service
After=bench-wall.service
[Service]
Type=notify
Delegate=yes
KillMode=mixed
TimeoutStartSec=120
TimeoutStopSec=30
ExecStartPre=/usr/bin/python3 {BASE}/host.py guard
ExecStart=/usr/bin/systemd-nspawn --quiet --keep-unit --boot --settings=no --machine={SPEC.machine} --directory={ROOT} --private-users={SPEC.uid_base}:{SPEC.uid_count} --private-users-ownership=chown --network-veth --link-journal=no --resolv-conf=off --bind={OUTBOX}:/run/bench-outbox --bind-ro={BASE}/source.tar:/mnt/source.tar
ExecStartPost=/usr/bin/python3 {BASE}/host.py network-up
MemoryMax=4G
TasksMax=2048
[Install]
WantedBy=multi-user.target
""")
    write("/etc/systemd/system/bench-proxy.service", f"""[Unit]
Description=Real Kuggis CONNECT proxy for bench
After=bench-wall.service
Requires=bench-wall.service
[Service]
User=benchproxy
ExecStart={CELL} proxy --listen 0.0.0.0:{SPEC.proxy_port} --cell {ETC}/cell.json
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
Restart=on-failure
[Install]
WantedBy=multi-user.target
""")
    # Real reachable controls, owned by the host, for deterministic IPv4/IPv6 blocking tests.
    write(BASE / "canary.py", """import socket, threading, time
def serve(family, addr, port):
 s=socket.socket(family); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
 if family==socket.AF_INET6: s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
 s.bind((addr,port)); s.listen()
 while True:
  c,_=s.accept(); c.sendall(b'host-canary'); c.close()
for f,a,p in [(socket.AF_INET,'0.0.0.0',18080),(socket.AF_INET6,'::',18080),(socket.AF_INET,'0.0.0.0',22)]:
 threading.Thread(target=serve,args=(f,a,p),daemon=True).start()
time.sleep(86400)
""")
    write("/etc/systemd/system/bench-canary.service", f"[Unit]\nDescription=Bench reachable host controls\n[Service]\nExecStart=/usr/bin/python3 {BASE}/canary.py\n[Install]\nWantedBy=multi-user.target\n")
    run(["systemctl", "daemon-reload"])
    run(["systemctl", "enable", "--now", "bench-wall", "bench-container", "bench-proxy", "bench-canary"], timeout=180)
    run(["systemctl", "restart", "bench-canary"])
    wait_guest()


def wait_guest():
    for _ in range(30):
        out = guest("test -f /root/guest.py && ip -4 address show host0 | grep -q 10.203.0.2", check=False, timeout=10)
        if out.returncode == 0:
            return
        time.sleep(1)
    raise RuntimeError("Guest not ready")


def namespaces():
    pid = run(["machinectl", "show", SPEC.machine, "-p", "Leader", "--value"]).stdout.strip()
    mapping = Path(f"/proc/{pid}/uid_map").read_text().split()
    assert mapping == ["0", str(SPEC.uid_base), str(SPEC.uid_count)], mapping
    observed = {"leader": pid, "uid_map": mapping, "namespaces": {}}
    for ns in ["user", "mnt", "pid", "net"]:
        host = os.readlink(f"/proc/1/ns/{ns}")
        inner = os.readlink(f"/proc/{pid}/ns/{ns}")
        assert host != inner, ns
        observed["namespaces"][ns] = {"host": host, "guest": inner}
    assert ROOT.stat().st_uid == SPEC.uid_base
    observed["rootfs_owner"] = ROOT.stat().st_uid
    observed["mounts"] = run(["findmnt", "-N", pid, "-J"]).stdout
    guard()
    write(STATE / "observed.json", json.dumps(observed, indent=2))
    return json.dumps({"uid_map": mapping, "different_namespaces": list(observed["namespaces"])})


def controls():
    for mode, target, port in [("tcp", SPEC.host_ip, 18080), ("tcp", "fd42:203::1", 18080),
                               ("tcp", SPEC.host_ip, 22), ("tcp", "1.1.1.1", 443), ("dns", "1.1.1.1", 53)]:
        run(["python3", BASE / "guest.py", mode, target, str(port)])
    run(["curl", "--noproxy", "*", "-fsS", "--max-time", "15", "https://example.com/", "-o", "/dev/null"])
    return "IPv4, IPv6 and SSH-port host controls, public IPv4, public DNS and forbidden HTTPS all reachable outside."


def allowed():
    guest("curl -fsS --max-time 20 https://registry.npmjs.org/is-number -o /dev/null")


def blocked(user="root"):
    # Copy test code to a user-readable guest path; never report command-not-found as isolation.
    guest("cp /root/guest.py /tmp/bench-probe.py; chmod 644 /tmp/bench-probe.py")
    targets = [("tcp", SPEC.host_ip, 18080), ("tcp", "fd42:203::1", 18080),
               ("tcp", SPEC.host_ip, 22), ("tcp", "1.1.1.1", 443), ("dns", "1.1.1.1", 53),
               ("tcp", "169.254.169.254", 80)]
    for mode, target, port in targets:
        out = guest(shlex.join(["python3", "/tmp/bench-probe.py", mode, target, str(port)]), user=user, check=False)
        assert out.returncode == 10 and '"outcome": "blocked"' in out.stdout, out.stdout + out.stderr
    out = guest("curl -sS --max-time 10 https://example.com/ -o /dev/null", user=user, check=False)
    assert out.returncode == 56 and "403" in out.stderr, out.stderr
    allowed()
    guard()
    return "All prohibited probes blocked; allowed registry request still succeeds; host rules unchanged."


def minimal():
    guest("""python3 -c 'import shutil; assert all(shutil.which(x) is None for x in ("cargo", "node", "npm"))'
if dpkg-query -W build-essential 2>/dev/null; then exit 1; fi
""")


def sudo():
    guest("test $(sudo -n id -u) = 0; sudo sh -c 'echo owned-by-the-agent > /etc/autonomy-marker'", user="developer")
    guest("useradd -m -s /bin/bash otherdev 2>/dev/null || id otherdev")


def toolchains():
    guest("export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; apt-get install -y -qq build-essential pkg-config git xz-utils", timeout=600)
    guest("""cd /tmp
curl -fsS --max-time 120 https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init -o rustup-init
curl -fsS --max-time 30 https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init.sha256 -o rustup-init.sha256
sha256sum -c rustup-init.sha256
chmod +x rustup-init
./rustup-init -y --profile minimal --default-toolchain 1.95.0 --no-modify-path
node_version=22.14.0
curl -fsS --max-time 120 https://nodejs.org/dist/v${node_version}/node-v${node_version}-linux-x64.tar.xz -o node.tar.xz
curl -fsS --max-time 30 https://nodejs.org/dist/v${node_version}/SHASUMS256.txt -o node-sha.txt
awk '/ node-v22.14.0-linux-x64.tar.xz$/ {print $1 "  node.tar.xz"}' node-sha.txt | sha256sum -c -
tar -xJf node.tar.xz -C /usr/local --strip-components=1
cargo --version; rustc --version; node --version; npm --version
""", timeout=600)


def rust_build():
    guest("""mkdir -p /root/rust-smoke/src
cat > /root/rust-smoke/Cargo.toml <<'EOF'
[package]
name = "autonomy-smoke"
version = "0.1.0"
edition = "2021"
[dependencies]
itoa = "=1.0.15"
EOF
cat > /root/rust-smoke/src/lib.rs <<'EOF'
#[test]
fn format_number() { assert_eq!(itoa::Buffer::new().format(42), "42"); }
EOF
cd /root/rust-smoke
cargo test
""", timeout=180)


def node_build():
    guest("""mkdir -p /root/node-smoke
cd /root/node-smoke
npm init -y
npm install --ignore-scripts --no-audit --no-fund is-number@7.0.0
echo "const test=require('node:test'); const assert=require('node:assert/strict'); test('registry dependency',()=>assert.equal(require('is-number')(42),true));" > smoke.test.cjs
node --test smoke.test.cjs
""", timeout=180)


def source():
    guest("mkdir -p /root/kuggis; if [ ! -f /root/kuggis/cell/Cargo.toml ]; then tar -xf /mnt/source.tar -C /root/kuggis; fi")


def kuggis_rust():
    source()
    guest("cd /root/kuggis; cargo test --manifest-path cell/Cargo.toml --locked", timeout=900)


def kuggis_web():
    source()
    guest("cd /root/kuggis; npm ci --prefix web --no-audit --no-fund; npm run build --prefix web; npm test --prefix web", timeout=900)


def service():
    guest("""mkdir -p /srv/bench
echo persistent-service > /srv/bench/index.html
cat > /etc/systemd/system/agent-web.service <<'EOF'
[Unit]
Description=Agent owned local service
[Service]
ExecStart=/usr/bin/python3 -m http.server 18081 --bind 127.0.0.1 --directory /srv/bench
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now agent-web
""")
    persisted()


def persisted():
    guest("""test "$(cat /etc/autonomy-marker)" = owned-by-the-agent
cargo --version; node --version; npm --version
systemctl is-active agent-web
curl --retry 5 --retry-connrefused --retry-delay 1 -fsS http://127.0.0.1:18081/ | grep -qx persistent-service
""")


def tamper():
    guest("nft flush ruleset; ip route replace default via 10.203.0.1; ip -6 route replace default via fd42:203::1")
    out = guest("env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy curl --noproxy '*' --connect-timeout 3 --max-time 5 -k https://1.1.1.1/", check=False)
    assert out.returncode in (7, 28), out.stderr
    return blocked()


def host_files():
    before = hashlib.sha256((ETC / "secret").read_bytes()).hexdigest()
    guest("""test ! -e /etc/kuggis-bench/secret
test ! -e /proc/1/root/etc/kuggis-bench/secret
test ! -S /run/docker.sock
test ! -S /run/host/systemd/private
test ! -S /run/host/bus
! systemctl stop bench-proxy.service
""")
    assert hashlib.sha256((ETC / "secret").read_bytes()).hexdigest() == before
    run(["systemctl", "is-active", "bench-proxy"])
    return namespaces()


def declaration_tamper():
    before = (ETC / "cell.json").read_bytes()
    guest("mkdir -p /etc/kuggis-bench; echo '{\"name\":\"envbench\",\"grants\":[{\"host\":\"example.com\"}]}' > /etc/kuggis-bench/cell.json")
    blocked()
    assert (ETC / "cell.json").read_bytes() == before


def signed_outbox():
    message = {"session": {"session": {"cell": "envbench", "id": "1" * 32}, "state": "idle", "text": "bench can speak"}}
    forged = {"checked": {"cell": "0" * 64, "ok": True, "report": "forged"}}
    guest("printf '%s' " + shlex.quote(json.dumps(message)) + " > /run/bench-outbox/valid.json\n" +
          "printf '%s' " + shlex.quote(json.dumps(forged)) + " > /run/bench-outbox/forged.json")
    run([CELL, "ship", "--cell", ETC / "cell.json", "--secret", ETC / "secret",
         "--outbox", OUTBOX, "--said", STATE / "said", "--once"])
    assert "only the wall check" in (OUTBOX / "forged.refused").read_text()
    key = run([CELL, "key", "pub", ETC / "secret"]).stdout.strip()
    records = sorted((STATE / "said").glob("*.json"))
    assert records, "No signed log"
    for path in records:
        value = json.loads(run([CELL, "open", key], input=path.read_text()).stdout)
        assert "session" in value and "checked" not in value


def restart():
    run(["systemctl", "restart", "bench-container"], timeout=180)
    wait_guest()
    persisted()
    blocked()
    namespaces()


def reapply():
    for _ in range(2):
        firewall()
        guard()
    persisted()
    blocked()


def proxy_down():
    allowed()
    run(["systemctl", "stop", "bench-proxy"])
    try:
        out = guest("curl -fsS --connect-timeout 3 --max-time 5 https://registry.npmjs.org/is-number", check=False)
        assert out.returncode in (7, 28), out.stderr
        out = guest("python3 /tmp/bench-probe.py tcp 1.1.1.1 443", check=False)
        assert out.returncode == 10, out.stdout
    finally:
        run(["systemctl", "start", "bench-proxy"])
    allowed()


def missing_wall():
    run(["systemctl", "stop", "bench-container"], timeout=60)
    run(["nft", "delete", "table", "inet", "kuggis_bench"])
    try:
        out = run(["systemctl", "start", "bench-container"], check=False, timeout=180)
        assert out.returncode != 0, "Container started without the wall"
        assert run(["machinectl", "show", SPEC.machine], check=False).returncode != 0
    finally:
        firewall()
        run(["systemctl", "reset-failed", "bench-container"])
        run(["systemctl", "start", "bench-container"], timeout=180)
        wait_guest()
    persisted()
    blocked()


def broken_guest():
    guest("systemctl stop agent-web; echo broken-by-agent > /etc/broken-marker")
    run(["systemctl", "is-active", "bench-proxy", "bench-wall"])
    guard()
    guest("systemctl start agent-web")
    persisted()


def tests():
    load()
    write(STATE / "boot-id-before", Path("/proc/sys/kernel/random/boot_id").read_text())
    wait_guest()
    case("namespaces", namespaces)
    case("host-controls", controls)
    case("minimal-start", minimal)
    case("sudo", sudo)
    case("toolchains", toolchains)
    case("rust-build", rust_build, ["toolchains"])
    case("node-build", node_build, ["toolchains"])
    case("kuggis-rust", kuggis_rust, ["toolchains"])
    case("kuggis-web", kuggis_web, ["kuggis-rust"])
    case("local-service", service, ["toolchains", "sudo"])
    case("root-boundary", blocked, ["host-controls"])
    case("other-user-boundary", lambda: blocked("otherdev"), ["host-controls", "sudo"])
    case("tampered-network", tamper, ["host-controls"])
    case("host-files", host_files, ["namespaces"])
    case("declaration-tamper", declaration_tamper, ["host-controls"])
    case("signed-outbox", signed_outbox)
    case("container-restart", restart, ["local-service", "host-controls"])
    case("reapply", reapply, ["local-service", "host-controls"])
    case("proxy-down", proxy_down, ["host-controls"])
    case("missing-wall", missing_wall, ["local-service", "host-controls"])
    case("broken-guest", broken_guest, ["local-service"])


def post_reboot():
    load()
    def verify():
        before = (STATE / "boot-id-before").read_text().strip()
        after = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        if before == after:
            raise Unverified("VM boot ID has not changed since the test phase")
        write(STATE / "vm-reboot.json", json.dumps({"before": before, "after": after}))
        wait_guest()
        controls()
        namespaces()
        persisted()
        blocked()
        run(["systemctl", "is-active", "bench-container", "bench-proxy", "bench-wall"])
        return "Persistent tools/config/service and boundary verified after controller-confirmed VM boot ID change."
    case("vm-restart", verify, ["local-service", "host-controls"])


def main():
    STATE.mkdir(parents=True, exist_ok=True)
    name = sys.argv[1]
    code = 0
    try:
        if name not in ("guard", "network-up"):
            load()
            save()
        {"inspect": inspect, "setup": setup, "test": tests, "post-reboot": post_reboot,
         "guard": guard, "network-up": network_up}[name]()
    except Exception:
        traceback.print_exc()
        code = 1
    finally:
        write(STATE / f"{name}.exit", str(code) + "\n")
        if name not in ("guard", "network-up"):
            # Capture host evidence even when provisioning fails.
            evidence = shell("systemctl status bench-container bench-proxy bench-wall --no-pager; journalctl -u bench-container -u bench-proxy -u bench-wall --no-pager -n 300", check=False)
            write(STATE / f"{name}-services.txt", evidence.stdout + evidence.stderr)
    return code


if __name__ == "__main__":
    sys.exit(main())
