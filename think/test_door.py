"""The door: requests and replies, the MCP server, the URL rule."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


def load(outbox, inbox):
    os.environ["KUGGIS_OUTBOX"], os.environ["KUGGIS_ROOM_INBOX"] = str(outbox), str(inbox)
    spec = importlib.util.spec_from_file_location("door", HERE / "door.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def rpc(door, *msgs):
    out = io.StringIO()
    door.serve(io.StringIO("".join(json.dumps(m) + "\n" for m in msgs)), out)
    return [json.loads(l) for l in out.getvalue().splitlines()]


class Door(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.outbox, self.inbox = self.tmp / "outbox", self.tmp / "inbox"
        self.inbox.mkdir()
        self.door = load(self.outbox, self.inbox)
        self.door.WAIT = 0

    def answer(self, rid, status, body=""):
        (self.inbox / "replies").mkdir(exist_ok=True)
        (self.inbox / "replies" / f"{rid}.md").write_text(self.door.render({"status": status}, body))

    def test_front_matter_round_trip(self):
        text = self.door.render({"kind": "brief", "cell": "forecasting", "empty": ""}, "do the thing\n")
        fields, body = self.door.front_matter(text)
        self.assertEqual(fields, {"kind": "brief", "cell": "forecasting"})
        self.assertEqual(body, "do the thing\n")
        self.assertEqual(self.door.front_matter("no front matter"), ({}, "no front matter"))

    def test_handshake_and_tool_list(self):
        init, listed = rpc(self.door,
                           {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                           {"jsonrpc": "2.0", "method": "notifications/initialized"},
                           {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertEqual(init["result"]["protocolVersion"], "2025-06-18")
        self.assertEqual(init["result"]["serverInfo"]["name"], "kuggis")
        self.assertEqual({t["name"] for t in listed["result"]["tools"]},
                         {"cells", "sessions", "brief", "outcome", "search", "fetch"})
        self.assertNotIn(None, [t["inputSchema"]["type"] for t in listed["result"]["tools"]])

    def test_unknown_method_and_bad_json(self):
        out = io.StringIO()
        self.door.serve(io.StringIO('not json\n{"jsonrpc":"2.0","id":7,"method":"nope"}\n'), out)
        (err,) = [json.loads(l) for l in out.getvalue().splitlines()]
        self.assertEqual(err["error"]["code"], -32601)

    def call(self, name, **args):
        (r,) = rpc(self.door, {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                               "params": {"name": name, "arguments": args}})
        return r["result"]["content"][0]["text"], r["result"].get("isError")

    def test_brief_is_a_request_that_waits(self):
        text, err = self.call("brief", cell="forecasting", session="instrument-v10", body="Do X.\nThen Y.")
        self.assertFalse(err)
        rid = text.split()[1]
        (f,) = list(self.outbox.glob("brief-*.md"))
        self.assertEqual(self.door.sha(f), rid)
        fields, body = self.door.front_matter(f.read_text())
        self.assertEqual(fields, {"kind": "brief", "cell": "forecasting", "session": "instrument-v10", "agent": "claude"})
        self.assertEqual(body, "Do X.\nThen Y.\n")
        self.assertEqual(self.call("outcome", id=rid)[0], "waiting")
        self.answer(rid, "sent", "attach with: kuggis forecasting instrument-v10")
        self.assertEqual(self.call("outcome", id=rid)[0], "sent\n\nattach with: kuggis forecasting instrument-v10")

    def test_brief_refuses_bad_names(self):
        self.assertIn("session must be", self.call("brief", cell="forecasting", session="../x", body="b")[0])
        self.assertIn("agent must be", self.call("brief", cell="c", session="s", body="b", agent="gpt")[0])
        self.assertIn("needs a body", self.call("brief", cell="c", session="s", body="  ")[0])
        self.assertEqual(list(self.outbox.glob("*")), [])

    def test_search_and_fetch_hand_back_when_unanswered(self):
        text, _ = self.call("search", query="example domains")
        self.assertTrue(text.startswith("waiting "))
        rid = text.split()[1]
        fields, _ = self.door.front_matter(next(self.outbox.glob("search-*.md")).read_text())
        self.assertEqual(fields, {"kind": "search", "query": "example domains"})
        self.answer(rid, "done", "- Example\n  https://example.com/\n  reserved")
        self.assertIn("https://example.com/", self.call("outcome", id=rid)[0])
        text, _ = self.call("fetch", url="https://example.com/page")
        self.assertIn("waits for the person", text)
        self.assertIn("kuggis url https://example.com/page", text)

    def test_fetch_tool_refuses_non_public_shapes_without_dns(self):
        for url in ("http://localhost/", "http://127.0.0.1:4748/mcp", "ftp://example.com/",
                    "http://user:pw@example.com/", "http://box.internal/", "http://nodots/"):
            self.assertTrue(self.call("fetch", url=url)[0].startswith("only"), url)
        self.assertEqual(list(self.outbox.glob("fetch-*")), [])

    def test_url_rule_outside_resolves(self):
        d = self.door
        self.assertIsNone(d.url_problem("https://example.com/", resolve=lambda h: {"93.184.216.34"}))
        self.assertIn("public hosts", d.url_problem("https://example.com/", resolve=lambda h: {"10.0.0.1"}))
        self.assertIn("public hosts", d.url_problem("https://example.com/", resolve=lambda h: {"93.184.216.34", "127.0.0.1"}))
        self.assertIn("public hosts", d.url_problem("https://example.com/", resolve=lambda h: (_ for _ in ()).throw(OSError())))
        self.assertIn("public hosts", d.url_problem("http://169.254.169.254/", resolve=lambda h: set()))

    def test_html_to_text(self):
        p = self.door.Text()
        p.feed("<html><head><title>T</title><style>b{}</style></head><body><h1>Hi</h1>"
               "<script>x()</script><p>one   two</p><ul><li>a</li><li>b</li></ul></body></html>")
        self.assertEqual(p.text(), "T\nHi\n\none two\n\na\n\nb")

    def test_tools_without_state(self):
        self.assertIn("kuggis watch", self.call("cells")[0])
        (self.inbox / "state.json").write_text(json.dumps({"cells": [
            {"name": "forecasting", "status": "1 working", "projects": ["airo"],
             "rows": [{"kind": "session", "project": "airo", "name": "v10", "state": "working", "age": "3m",
                       "needs_you": False, "screen": "line1\nline2\nline3"},
                      {"kind": "door", "project": "airo", "name": "brief / v11", "state": "awaiting dispatch",
                       "age": "", "needs_you": True}]}]}))
        self.assertEqual(self.call("cells")[0], "forecasting  ·  1 working  ·  projects: airo")
        text = self.call("sessions", lines=2)[0]
        self.assertIn("- session  airo/v10  working  3m", text)
        self.assertIn("    line2\n    line3", text)
        self.assertNotIn("line1", text)
        self.assertIn("- NEEDS YOU  door  airo/brief / v11  awaiting dispatch", text)
        self.assertIn("no cell named hiring", self.call("sessions", cell="hiring")[0])

    def test_cli_search_without_key(self):
        r = subprocess.run([sys.executable, "-I", str(HERE / "door.py"), "search", "q"],
                           capture_output=True, text=True, env={**os.environ, "TAVILY_API_KEY": ""})
        self.assertEqual(r.returncode, 1)
        self.assertIn("no search backend", r.stderr)

    def test_cli_fetch_refuses_loopback(self):
        r = subprocess.run([sys.executable, "-I", str(HERE / "door.py"), "fetch", "http://127.0.0.1:1/"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("public hosts", r.stderr)


if __name__ == "__main__":
    unittest.main()
