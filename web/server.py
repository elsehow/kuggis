#!/usr/bin/env python3
"""kuggis web: serves web/ on 127.0.0.1 and runs `kuggis _web ...` for the API.

Usage (kuggis web runs this): server.py <port> <path to kuggis>, with
KUGGIS_WEB_TOKEN_FILE naming the token file.

The API can merge PRs and send reviews to sessions, so a request must carry
the token (X-Kuggis-Token; the page gets it from the URL's #fragment, which is
never sent to a server) and a Host header naming this server, which stops
other sites and DNS rebinding. Nothing sets CORS headers. Page assets need no
token: there is nothing in them to protect.
"""
import hmac
import http.server
import json
import os
import re
import subprocess
import sys
from urllib.parse import parse_qs, urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = {
    "/": ("index.html", "text/html; charset=utf-8"),
    "/app.js": ("app.js", "text/javascript; charset=utf-8"),
    "/app.css": ("app.css", "text/css; charset=utf-8"),
    "/theme.css": ("theme.css", "text/css; charset=utf-8"),
    "/hanken-grotesk-latin.woff2": ("hanken-grotesk-latin.woff2", "font/woff2"),
}
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
NUMBER = re.compile(r"[0-9]{1,7}\Z")
SHA = re.compile(r"[0-9a-f]{40}\Z")
HASH = re.compile(r"[0-9a-f]{64}\Z")
PLAIN = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")  # a project or session name
HEADERS = {
    "Content-Security-Policy": "default-src 'none'; script-src 'self'; style-src 'self'; "
    "img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; "
    "form-action 'none'; frame-ancestors 'none'",
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Cache-Control": "no-store",
}


def pr_args(q):
    repo, n = q.get("repo", ""), str(q.get("n", ""))
    if not REPO.match(repo) or not NUMBER.match(n):
        raise ValueError("bad PR")
    return [repo, n]


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = "kuggis"
    sys_version = ""

    def log_message(self, fmt, *args):  # one line per action, none per poll
        if self.command == "POST":
            sys.stderr.write("kuggis web: %s\n" % (fmt % args))

    def send(self, code, body, ctype="application/json"):
        if not isinstance(body, bytes):
            body = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in HEADERS.items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def allowed(self, api):
        host = self.headers.get("Host", "")
        if host not in self.server.hosts:
            self.send(403, {"error": "wrong host"})
            return False
        if api and not hmac.compare_digest(self.headers.get("X-Kuggis-Token", ""), self.server.token):
            self.send(403, {"error": "no token: open the link kuggis web prints"})
            return False
        return True

    def kuggis(self, *args, action=False):
        try:
            p = subprocess.run([self.server.kuggis, "_web", *args], stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=300)
        except subprocess.TimeoutExpired:
            return self.send(504, {"error": "kuggis took too long"})
        out, err = p.stdout.strip(), p.stderr.strip()
        if p.returncode != 0:
            return self.send(409, {"error": err.removeprefix("kuggis: ") or out or "failed"})
        if action:
            return self.send(200, {"message": out})
        self.send(200, out.encode())

    def do_GET(self):
        u = urlsplit(self.path)
        if u.path in ASSETS:
            if not self.allowed(api=False):
                return
            name, ctype = ASSETS[u.path]
            with open(os.path.join(HERE, name), "rb") as f:
                return self.send(200, f.read(), ctype)
        if not self.allowed(api=True):
            return
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        try:
            if u.path == "/api/ping":
                return self.send(200, {"kuggis": True})
            if u.path == "/api/state":
                return self.kuggis("state")
            if u.path in ("/api/pr", "/api/log"):
                return self.kuggis(u.path[5:], *pr_args(q))
        except ValueError as e:
            return self.send(400, {"error": str(e)})
        self.send(404, {"error": "not found"})

    def do_POST(self):
        if not self.allowed(api=True):
            return
        try:
            if self.headers.get("Content-Type", "") != "application/json":
                raise ValueError("send JSON")
            n = int(self.headers.get("Content-Length", "0"))
            q = json.loads(self.rfile.read(min(n, 10000)) or b"{}")
            if not isinstance(q, dict):
                raise ValueError("send an object")
            path = urlsplit(self.path).path
            if path == "/api/ask":
                return self.kuggis("ask", *pr_args(q), action=True)
            if path == "/api/merge":
                if not SHA.match(str(q.get("sha", ""))):
                    raise ValueError("bad commit")
                return self.kuggis("merge", *pr_args(q), q["sha"], action=True)
            if path == "/api/close":
                project, session = str(q.get("project", "")), str(q.get("session", ""))
                if not PLAIN.match(project) or not PLAIN.match(session):
                    raise ValueError("bad session")
                return self.kuggis("close", project, session, action=True)
            if path == "/api/send":
                if not HASH.match(str(q.get("hash", ""))):
                    raise ValueError("bad review hash")
                return self.kuggis("send", *pr_args(q), q["hash"], action=True)
        except (ValueError, json.JSONDecodeError) as e:
            return self.send(400, {"error": str(e)})
        self.send(404, {"error": "not found"})


def main():
    port, kuggis = int(sys.argv[1]), sys.argv[2]
    with open(os.environ["KUGGIS_WEB_TOKEN_FILE"]) as f:
        token = f.read().strip()
    if len(token) < 32:
        sys.exit("kuggis web: the token file is too short")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.token, server.kuggis = token, kuggis
    server.hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
