#!/usr/bin/python3 -I
"""The door between the Thinker's room and kuggis.

Nothing in the room can reach a VM, the web or GitHub. What it wants crosses
as a *request*: a file it writes to its outbox, with a front matter `kind`.
kuggis, outside the room, carries each request by its kind's rule and writes
the answer to the room's inbox as a *reply*, named by the request's SHA-256.

    kind    rule
    brief   waits for the person; dispatched into a kuggis session
    search  goes; the URLs it returns become known
    fetch   goes if its URL is known (a search returned it, or the person
            gave it with `kuggis url`); otherwise waits for the person

This one file serves both sides, so the format has one definition:

    door.py serve            inside the room: an MCP server over stdio
    door.py fetch URL        outside: the page as text, public hosts only
    door.py search QUERY     outside: results from the search backend
"""
import hashlib
import html.parser
import ipaddress
import json
import os
import re
import secrets
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

OUTBOX = os.environ.get("KUGGIS_OUTBOX", "/Users/thinker/workspace/outbox")
INBOX = os.environ.get("KUGGIS_ROOM_INBOX", "/Users/thinker/workspace/inbox")
STATE = os.path.join(INBOX, "state.json")
REPLIES = os.path.join(INBOX, "replies")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
AGENTS = ("claude", "codex", "pi")
WAIT = 45  # seconds a tool waits for an open request's reply before handing back its id
BLOCKED_SUFFIXES = (".localhost", ".local", ".internal", ".home.arpa", ".lan")
FETCH_BYTES = 2_000_000
FETCH_CHARS = 200_000


# ── Requests and replies ─────────────────────────────────────────────────────

def front_matter(text):
    """(fields, body) of a `---` front-matter file; ({}, text) without one."""
    lines = text.split("\n")
    if not lines or lines[0] != "---":
        return {}, text
    fields = {}
    for i, line in enumerate(lines[1:], 1):
        if line == "---":
            return fields, "\n".join(lines[i + 1:])
        key, sep, value = line.partition(":")
        if sep:
            fields[key.strip()] = value.strip()
    return {}, text


def render(fields, body):
    head = "".join(f"{k}: {v}\n" for k, v in fields.items() if v not in (None, ""))
    return f"---\n{head}---\n{body.rstrip()}\n"


def sha(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def write_request(kind, fields, body=""):
    """Write a request atomically; return its id (the file's SHA-256)."""
    os.makedirs(OUTBOX, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(2)
    name = f"{kind}-{stamp}.md"
    tmp = os.path.join(OUTBOX, f".{name}.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(render({"kind": kind, **fields}, body))
    os.replace(tmp, os.path.join(OUTBOX, name))
    return sha(os.path.join(OUTBOX, name))


def reply(request_id):
    """(status, body) of the reply to a request, or ('waiting', '')."""
    try:
        with open(os.path.join(REPLIES, f"{request_id}.md"), encoding="utf-8") as f:
            fields, body = front_matter(f.read())
    except OSError:
        return "waiting", ""
    return fields.get("status", "waiting"), body


def await_reply(request_id, seconds=None):
    end = time.monotonic() + (WAIT if seconds is None else seconds)
    while True:
        status, body = reply(request_id)
        if status != "waiting" or time.monotonic() >= end:
            return status, body
        time.sleep(0.5)


def state():
    try:
        with open(STATE, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


# ── The URL rule ─────────────────────────────────────────────────────────────

def system_resolve(host):
    return {info[4][0].split("%")[0] for info in socket.getaddrinfo(host, None)}


def public_host(host, resolve=system_resolve):
    """True if every address of host is on the public internet. With
    resolve=None (inside the room, which has no DNS) only the name's shape and
    a literal address are judged; the carrier resolves before it fetches."""
    host = host.lower().rstrip(".")
    if not host or host == "localhost" or host.endswith(BLOCKED_SUFFIXES):
        return False
    try:
        addrs = [ipaddress.ip_address(host.strip("[]"))]
    except ValueError:
        if resolve is None:
            return "." in host
        try:
            addrs = [ipaddress.ip_address(a) for a in resolve(host)]
        except (OSError, ValueError):
            return False
    return bool(addrs) and all(a.is_global for a in addrs)


def url_problem(url, resolve=system_resolve):
    """Why a URL may not be fetched, or None."""
    if not isinstance(url, str) or not url:
        return "no URL"
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("http", "https") or parts.username or parts.password:
        return "only plain http(s) URLs"
    if not public_host(parts.hostname or "", resolve):
        return "only public hosts (no localhost, private or unresolvable addresses)"
    return None


class Text(html.parser.HTMLParser):
    BLOCKS = {"p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6",
              "section", "article", "header", "footer", "blockquote", "pre", "table"}
    SKIP = {"script", "style", "noscript", "svg", "template"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts, self.skip = [], 0

    def handle_starttag(self, tag, attrs):
        if tag in self.SKIP:
            self.skip += 1
        elif tag in self.BLOCKS:
            self.parts.append("\n")

    def handle_endtag(self, tag):
        if tag in self.SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag in self.BLOCKS:
            self.parts.append("\n")

    def handle_data(self, data):
        if not self.skip:
            self.parts.append(data)

    def text(self):
        lines = (re.sub(r"[ \t\r\f\v]+", " ", l).strip() for l in "".join(self.parts).split("\n"))
        return re.sub(r"\n{3,}", "\n\n", "\n".join(lines)).strip()


class PublicOnly(urllib.request.HTTPRedirectHandler):
    """Every hop of a redirect must pass the URL rule too."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        problem = url_problem(newurl)
        if problem:
            raise urllib.error.URLError(f"redirect to {newurl}: {problem}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def fetch(url):
    """The page at url as text. Raises ValueError with the reason to refuse."""
    problem = url_problem(url)
    if problem:
        raise ValueError(problem)
    req = urllib.request.Request(url, headers={"User-Agent": "kuggis-door/1 (+text only)"})
    opener = urllib.request.build_opener(PublicOnly)
    try:
        with opener.open(req, timeout=15) as resp:
            kind = resp.headers.get_content_type()
            charset = resp.headers.get_content_charset() or "utf-8"
            raw = resp.read(FETCH_BYTES + 1)
    except (urllib.error.URLError, OSError, ValueError) as e:
        raise ValueError(f"fetch failed: {getattr(e, 'reason', e)}") from e
    cut = len(raw) > FETCH_BYTES
    text = raw[:FETCH_BYTES].decode(charset, errors="replace")
    if kind in ("text/html", "application/xhtml+xml"):
        parser = Text()
        parser.feed(text)
        text = parser.text()
    elif not kind.startswith("text/") and kind not in ("application/json", "application/xml"):
        raise ValueError(f"not text: {kind}")
    if cut or len(text) > FETCH_CHARS:
        text = text[:FETCH_CHARS].rstrip() + "\n\n(cut: the page is longer)"
    return text


def search(query, key):
    """Results from Tavily as (title, url, snippet) tuples."""
    if not key:
        raise ValueError("no search backend: put TAVILY_API_KEY in ~/.config/kuggis/secrets/thinker.env")
    body = json.dumps({"api_key": key, "query": query, "max_results": 8}).encode()
    req = urllib.request.Request("https://api.tavily.com/search", data=body,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            data = json.load(resp)
    except (urllib.error.URLError, OSError, ValueError) as e:
        raise ValueError(f"search failed: {getattr(e, 'reason', e)}") from e
    return [(r.get("title", ""), r.get("url", ""), r.get("content", ""))
            for r in data.get("results", []) if isinstance(r, dict) and r.get("url")]


# ── The MCP server, inside the room ──────────────────────────────────────────

def t_cells(_):
    s = state()
    if not s:
        return "kuggis hasn't written state.json yet: kuggis watch isn't running."
    out = []
    for c in s["cells"]:
        out.append(f"{c['name']}  ·  {c['status']}  ·  projects: {', '.join(c['projects']) or 'none'}")
    return "\n".join(out)


def t_sessions(args):
    s = state()
    if not s:
        return "kuggis hasn't written state.json yet: kuggis watch isn't running."
    want, lines = args.get("cell"), int(args.get("lines", 40))
    out = []
    for c in s["cells"]:
        if want and c["name"] != want:
            continue
        out.append(f"## {c['name']}  ·  {c['status']}")
        for r in c["rows"]:
            who = f"{r['project']}/{r['name']}" if r.get("project") else r["name"]
            flag = "NEEDS YOU  " if r.get("needs_you") else ""
            out.append(f"- {flag}{r['kind']}  {who}  {r['state']}  {r.get('age', '')}".rstrip())
            screen = r.get("screen")
            if screen and lines > 0:
                tail = screen.rstrip("\n").split("\n")[-lines:]
                out.extend("    " + l for l in tail)
    if want and len(out) == 0:
        return f"no cell named {want}; see cells()"
    return "\n".join(out)


def t_brief(args):
    cell, session, body = args.get("cell", ""), args.get("session", ""), args.get("body", "")
    agent, project = args.get("agent", "claude"), args.get("project", "")
    for value, what in ((cell, "cell"), (session, "session")):
        if not NAME.fullmatch(value or ""):
            return f"{what} must be letters, digits, - and _"
    if project and not NAME.fullmatch(project):
        return "project must be letters, digits, - and _"
    if agent not in AGENTS:
        return f"agent must be one of {', '.join(AGENTS)}"
    if not body.strip():
        return "the brief needs a body"
    rid = write_request("brief", {"cell": cell, "project": project, "session": session, "agent": agent}, body)
    return (f"queued {rid}\nIt waits for the person to read and dispatch it; "
            f"outcome(\"{rid}\") says what happened. Go on with something else.")


def t_outcome(args):
    rid = args.get("id", "")
    if not re.fullmatch(r"[0-9a-f]{64}", rid or ""):
        return "id is the 64-hex request id a tool returned"
    status, body = reply(rid)
    return f"{status}\n\n{body}".rstrip()


def t_search(args):
    query = (args.get("query") or "").strip()
    if not query:
        return "query is empty"
    rid = write_request("search", {"query": query.replace("\n", " ")})
    status, body = await_reply(rid)
    return body if status == "done" else f"{status} {rid}\n{body}".rstrip()


def t_fetch(args):
    url = (args.get("url") or "").strip()
    problem = url_problem(url, resolve=None)
    if problem:
        return problem
    rid = write_request("fetch", {"url": url})
    status, body = await_reply(rid)
    if status == "done":
        return body
    if status == "waiting":
        return (f"waiting {rid}\nThis URL didn't come from a search here, so it waits for the person. "
                f"outcome(\"{rid}\") later, or ask them to run: kuggis url {url}")
    return f"{status} {rid}\n{body}".rstrip()


TOOLS = {
    "cells": ("The kuggis cells: each with its state and projects.", {}, [], t_cells),
    "sessions": ("Every kuggis session grouped by cell, with its state, whether it needs the person, "
                 "and the last lines of its screen. Open PRs and waiting requests appear as rows too.",
                 {"cell": {"type": "string", "description": "one cell only"},
                  "lines": {"type": "integer", "description": "screen lines per session (default 40; 0 for none)"}},
                 [], t_sessions),
    "brief": ("Start a kuggis session, or send a message to a running one, with a brief. The person "
              "reads every brief before it goes; the VMs have no vault, so summarize, never paste notes.",
              {"cell": {"type": "string", "description": "the cell (or project) to run in"},
               "session": {"type": "string", "description": "a new name starts a session; an existing one receives the brief"},
               "body": {"type": "string", "description": "what to do, why, what done looks like, what to ask the person first"},
               "agent": {"type": "string", "enum": list(AGENTS), "description": "the worker (default claude)"},
               "project": {"type": "string", "description": "when the cell has several projects"}},
              ["cell", "session", "body"], t_brief),
    "outcome": ("What happened to a request: waiting, sent, edited, rejected, done or refused, with any answer.",
                {"id": {"type": "string"}}, ["id"], t_outcome),
    "search": ("Search the web. Results come back as title, URL and snippet; those URLs can then be fetched.",
               {"query": {"type": "string"}}, ["query"], t_search),
    "fetch": ("A web page as text. Goes straight through for a URL a search here returned or the person "
              "gave kuggis; any other URL waits for the person.",
              {"url": {"type": "string"}}, ["url"], t_fetch),
}


def tool_list():
    return [{"name": n, "description": d,
             "inputSchema": {"type": "object", "properties": p, "required": r}}
            for n, (d, p, r, _) in TOOLS.items()]


def handle(msg):
    """The JSON-RPC result for msg, or None for a notification."""
    method, params, mid = msg.get("method"), msg.get("params") or {}, msg.get("id")
    if method == "initialize":
        version = params.get("protocolVersion", "2024-11-05")
        return {"protocolVersion": version if version in ("2024-11-05", "2025-03-26", "2025-06-18") else "2024-11-05",
                "capabilities": {"tools": {}}, "serverInfo": {"name": "kuggis", "version": "1"}}
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": tool_list()}
    if method == "tools/call":
        name, args = params.get("name"), params.get("arguments") or {}
        if name not in TOOLS:
            return {"content": [{"type": "text", "text": f"no tool {name}"}], "isError": True}
        try:
            text = TOOLS[name][3](args if isinstance(args, dict) else {})
        except Exception as e:  # the room must never see a traceback with paths in it
            text, err = f"{type(e).__name__}", True
        else:
            err = False
        return {"content": [{"type": "text", "text": text}], "isError": err}
    if mid is None:
        return None
    raise LookupError(method)


def serve(stdin=sys.stdin, stdout=sys.stdout):
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        mid = msg.get("id")
        try:
            result = handle(msg)
        except LookupError as e:
            out = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"method not found: {e}"}}
        else:
            if mid is None:
                continue
            out = {"jsonrpc": "2.0", "id": mid, "result": result}
        stdout.write(json.dumps(out) + "\n")
        stdout.flush()


# ── Outside the room ─────────────────────────────────────────────────────────

def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "serve":
        serve()
    elif cmd == "fetch" and len(argv) == 3:
        try:
            print(fetch(argv[2]))
        except ValueError as e:
            sys.exit(f"door: {e}")
    elif cmd == "search" and len(argv) in (3, 5):
        try:
            results = search(argv[2], os.environ.get("TAVILY_API_KEY", ""))
        except ValueError as e:
            sys.exit(f"door: {e}")
        for title, url, snippet in results:
            print(f"- {title}\n  {url}\n  {' '.join(snippet.split())[:400]}\n")
        if len(argv) == 5 and argv[3] == "--remember":
            with open(argv[4], "a", encoding="utf-8") as f:
                f.writelines(url + "\n" for _, url, _ in results)
        if not results:
            print("(no results)")
    else:
        sys.exit("usage: door.py serve | fetch URL | search QUERY [--remember FILE]")


if __name__ == "__main__":
    main(sys.argv)
