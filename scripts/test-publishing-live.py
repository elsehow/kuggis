#!/usr/bin/env python3
"""Opt-in local end-to-end publishing smoke test (Linux, sudo, curl, openssl).

Uses disposable keys only. Creates a root-owned signer under /run, HTTPS
frontends on loopback, and an unprivileged publisher/client. Nothing is public,
no exe.dev account is contacted, and all test processes/state are removed.
Build first: cargo build --manifest-path cell/Cargo.toml --locked
Run: python3 scripts/test-publishing-live.py [path/to/cell]
"""
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid

BINARY = Path(sys.argv[1] if len(sys.argv) > 1 else "cell/target/debug/cell").resolve()
TRUSTED = Path("/run/kuggis-publish-smoke-" + uuid.uuid4().hex)
processes = []
frontends = []


def command(args, **kwargs):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"command failed: {args}\n{result.stderr}\n{result.stdout}")
    return result.stdout.strip()


def port():
    with socket.socket() as stream:
        stream.bind(("127.0.0.1", 0))
        return stream.getsockname()[1]


def start(args, log, root=False, env=None):
    handle = open(log, "w")
    process = subprocess.Popen([str(a) for a in args], stdout=handle, stderr=handle, start_new_session=True, env=env)
    handle.close()
    processes.append((process, root))
    return process


def wait_ready(check, process, log):
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError(Path(log).read_text())
        if check():
            return
        time.sleep(0.05)
    raise RuntimeError(f"service not ready: {Path(log).read_text()}")


def listening(at):
    try:
        socket.create_connection(("127.0.0.1", at), timeout=0.1).close()
        return True
    except OSError:
        return False


def https_frontend(backend, cert, key):
    class Proxy(http.server.BaseHTTPRequestHandler):
        def forward(self):
            data = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            upstream = http.client.HTTPConnection("127.0.0.1", backend, timeout=10)
            upstream.request(self.command, self.path, data, {"Content-Type": "application/json"})
            response = upstream.getresponse()
            body = response.read()
            self.send_response(response.status)
            for name, value in response.getheaders():
                if name.lower() not in {"connection", "transfer-encoding", "content-length"}:
                    self.send_header(name, value)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            upstream.close()

        do_GET = do_POST = forward

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    frontends.append(server)
    return f"https://localhost:{server.server_port}"


if os.getuid() == 0:
    sys.exit("Run as an unprivileged user with sudo, not as root.")
command(["sudo", "-n", "true"])
try:
    with tempfile.TemporaryDirectory(prefix="kuggis-publishing-smoke-") as work:
        work = Path(work)
        # Trust boundary: the signer binary, key, declaration, config and socket
        # parent are root-owned. The unprivileged client cannot read the key.
        command(["sudo", "-n", "install", "-d", "-m", "0755", TRUSTED])
        command(["sudo", "-n", "install", "-m", "0755", BINARY, TRUSTED / "cell"])
        public_key = command(["sudo", "-n", TRUSTED / "cell", "key", "new", TRUSTED / "secret"])
        try:
            (TRUSTED / "secret").read_bytes()
            raise AssertionError("agent can read the signer key")
        except PermissionError:
            pass
        registry = work / "registry"
        audience = command([BINARY, "publisher", "init", registry])
        namespace = command([BINARY, "publisher", "namespace", registry, public_key])
        tree = work / "tree"
        (tree / ".pins").mkdir(parents=True)
        declaration = {"name": "smoke", "grants": [{"publish": {"namespace": namespace}}]}
        (tree / "smoke.json").write_text(json.dumps(declaration))
        (tree / ".pins/smoke.pub").write_text(public_key)
        cert, key = work / "cert.pem", work / "tls.key"
        command(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost", "-keyout", key, "-out", cert])
        control, public = port(), port()
        endpoint = https_frontend(control, cert, key)
        public_base = https_frontend(public, cert, key)
        command([BINARY, "publisher", "configure", registry, "--public-base", public_base])
        plan = subprocess.run([str(BINARY), "publisher", "plan", str(registry), str(tree)], capture_output=True, text=True, check=True)
        approval = plan.stderr.strip().removeprefix("approve with ")
        command([BINARY, "publisher", "apply", registry, tree, "--approve", approval])
        env = dict(os.environ, CURL_CA_BUNDLE=str(cert), HTTPS_PROXY="", HTTP_PROXY="", ALL_PROXY="", NO_PROXY="localhost")
        for service, listen in [("serve", control), ("host", public)]:
            log = work / f"{service}.log"
            process = start([BINARY, "publisher", service, registry, "--listen", f"127.0.0.1:{listen}"], log, env=env)
            wait_ready(lambda: listening(listen), process, log)
        config = {"socket": str(TRUSTED / "bridge.sock"), "namespace": namespace, "audience": audience, "endpoint": endpoint}
        local_config = work / "publisher.json"
        local_config.write_text(json.dumps(config))
        command(["sudo", "-n", "install", "-m", "0644", local_config, TRUSTED / "publisher.json"])
        command(["sudo", "-n", "install", "-m", "0644", tree / "smoke.json", TRUSTED / "cell.json"])
        bridge_log = work / "bridge.log"
        bridge = start(["sudo", "-n", TRUSTED / "cell", "publisher", "bridge", "--config", TRUSTED / "publisher.json", "--cell", TRUSTED / "cell.json", "--secret", TRUSTED / "secret", "--agent-uid", str(os.getuid())], bridge_log, root=True)
        wait_ready(lambda: Path(config["socket"]).exists(), bridge, bridge_log)
        dist = work / "dist"
        dist.mkdir()
        (work / "private.txt").write_text("must not be published")
        (dist / "index.html").write_text('<!doctype html><h1>version one</h1><p id="loaded"></p><script type="module" src="./app.mjs"></script>')
        (dist / "app.mjs").write_text('const data = await (await fetch("./data.json")).json(); document.querySelector("#loaded").textContent = data.text;')
        (dist / "data.json").write_text(json.dumps({"text": "dataset works"}))
        env = dict(os.environ, CURL_CA_BUNDLE=str(cert), HTTPS_PROXY="", HTTP_PROXY="", ALL_PROXY="", NO_PROXY="localhost")

        def publish(*args):
            return json.loads(command([BINARY, "publish", *args, "--config", TRUSTED / "publisher.json"], env=env))

        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(cert))))
        first = publish(dist, "--site", "demo", "--visibility", "public")["published"]
        url = first["url"]
        assert b"version one" in opener.open(url).read()
        # Optional real-browser coverage; no browser download is performed here.
        playwright = os.environ.get("PUBLISH_SMOKE_PLAYWRIGHT")
        if playwright:
            browser_test = r"""
const {chromium} = require(process.argv[1]);
(async () => {
  const browser = await chromium.launch({headless:true});
  try {
    const page = await browser.newPage({ignoreHTTPSErrors:true});
    page.on('console', m => console.error(m.type(), m.text()));
    await page.goto(process.argv[2]);
    await page.waitForFunction(() => document.querySelector('#loaded')?.textContent === 'dataset works');
    const isolated = await page.evaluate(() => { try { localStorage.setItem('test','bad'); return false; } catch { return true; } });
    if (!isolated) throw new Error('published page escaped opaque-origin sandbox');
    console.log('PASS: real Chromium module + JSON fetch; opaque-origin isolation');
  } finally { await browser.close(); }
})().catch(e => { console.error(e); process.exit(1); });
"""
            print(command(["node", "-e", browser_test, playwright, url]))
        (dist / "index.html").write_text("<!doctype html><h1>version two</h1>")
        second = publish(dist, "--site", "demo", "--visibility", "public")["published"]
        assert second["url"] == url and first["version"] != second["version"]
        assert b"version two" in opener.open(url).read()
        status = publish("status", "demo")["status"]
        assert len(status["versions"]) == 2
        publish("rollback", "demo", first["version"])
        assert b"version one" in opener.open(url).read()
        publish("unpublish", "demo")
        try:
            opener.open(url)
            raise AssertionError("unpublished site still served")
        except urllib.error.HTTPError as error:
            assert error.code == 404
        publish("rollback", "demo", second["version"])
        assert b"version two" in opener.open(url).read()
        # Local revocation is re-read by the already-running privileged signer.
        (tree / "smoke.json").write_text(json.dumps({"name": "smoke", "grants": []}))
        command(["sudo", "-n", "install", "-m", "0644", tree / "smoke.json", TRUSTED / "cell.json"])
        refused = subprocess.run([str(BINARY), "publish", "unpublish", "demo", "--config", str(TRUSTED / "publisher.json")], env=env, capture_output=True, text=True)
        assert refused.returncode != 0 and "no local publishing grant" in refused.stderr
        assert b"version two" in opener.open(url).read()
        print("PASS: separate-UID signer; key unreadable; CLI -> Unix socket -> HTTPS -> publisher -> public HTTPS")
        print("PASS: publish, update, status, rollback, unpublish, restore, local revocation")
finally:
    for process, root in reversed(processes):
        if process.poll() is None:
            if root:
                subprocess.run(["sudo", "-n", "kill", "--", f"-{process.pid}"], capture_output=True)
            else:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                subprocess.run(["sudo", "-n", "kill", "-KILL", "--", f"-{process.pid}"], capture_output=True)
    for server in frontends:
        server.shutdown()
        server.server_close()
    subprocess.run(["sudo", "-n", "rm", "-rf", "--", TRUSTED], capture_output=True)
