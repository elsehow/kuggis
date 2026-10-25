"""Cell dashboard projection and real HTTP/CLI boundary tests. No live services."""
import copy
import http.client
import json
import os
from pathlib import Path
import runpy
import subprocess
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from unittest.mock import patch

from web.cells import project

ROOT = Path(__file__).resolve().parent
SNAPSHOT = {
    "now": 2000, "thinker_installed": True,
    "projects": [{"name": "airo", "profile": "airo"}, {"name": "kuggis", "profile": "-"},
                 {"name": "other", "profile": "-"}, {"name": "bigbrain", "profile": "bigbrain"}],
    "sessions": [
        {"project": "airo", "name": "zulu", "state": "working", "age": "2m"},
        {"project": "airo", "name": "alpha", "state": "waiting on you (done)", "age": "4m"},
        {"project": "bigbrain", "name": "-", "state": "unreachable", "age": "-"},
        {"project": "kuggis", "name": "dashboard", "state": "idle", "age": "3m"},
        {"project": "other", "name": "-", "state": "no sessions", "age": "-"},
    ],
    "requests": [{"id": "a" * 64, "kind": "brief", "cell": "", "project": "airo", "session": "task",
                  "url": "", "since": 1000, "held": True}],
}


class Cells(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.config = Path(self.tmp.name)
        (self.config / "cells").mkdir()

    def definition(self, name, projects):
        p = self.config / "cells" / f"{name}.json"
        p.write_text(json.dumps({"version": 1, "cell": name, "projects": projects}))
        return p

    def test_stable_order_attention_and_real_commands(self):
        result = project(SNAPSHOT, self.config)
        self.assertEqual([c["name"] for c in result["cells"]],
                         ["trusted", "forecasting", "bigbrain-dev", "operator", "unassigned"])
        forecasting = result["cells"][1]
        self.assertEqual([r["name"] for r in forecasting["rows"]], ["alpha", "zulu", "brief / task"])
        self.assertEqual(forecasting["status"], "1 working · 2 need you")
        self.assertEqual(forecasting["rows"][0]["command"], "kuggis forecasting alpha")
        self.assertEqual(forecasting["rows"][-1]["command"], "kuggis dispatch")
        self.assertEqual(forecasting["rows"][-1]["since"], 1000)
        self.assertTrue(result["cells"][0]["rows"][0]["needs_you"])
        self.assertTrue(result["cells"][2]["rows"][0]["needs_you"])
        changed = copy.deepcopy(SNAPSHOT)
        changed["sessions"][0]["state"] = "waiting on you"
        changed["sessions"][1]["state"] = "working"
        self.assertEqual([r["id"] for r in project(changed, self.config)["cells"][1]["rows"]],
                         [r["id"] for r in forecasting["rows"]])

    def test_definitions_override_inference_and_are_not_enforcement(self):
        p = self.definition("research", ["airo"])
        result = project(SNAPSHOT, self.config)
        cell = next(c for c in result["cells"] if c["name"] == "research")
        self.assertEqual(cell["projects"], ["airo"])
        self.assertEqual(cell["definition"]["text"], p.read_text())
        self.assertFalse(cell["definition"]["enforced"])
        self.assertEqual(len(cell["definition"]["sha256"]), 64)
        self.assertNotIn("forecasting", [c["name"] for c in result["cells"]])

    def test_explicit_membership_excludes_default_members(self):
        self.definition("forecasting", [])
        result = project(SNAPSHOT, self.config)
        self.assertEqual(next(c for c in result["cells"] if c["name"] == "forecasting")["projects"], [])
        self.assertIn("airo", next(c for c in result["cells"] if c["name"] == "unassigned")["projects"])

    def test_duplicate_membership_and_operator_are_rejected(self):
        self.definition("one", ["airo"])
        other = self.definition("two", ["airo"])
        with self.assertRaisesRegex(ValueError, "more than one"):
            project(SNAPSHOT, self.config)
        other.unlink()
        self.definition("one", ["kuggis"])
        with self.assertRaisesRegex(ValueError, "outside"):
            project(SNAPSHOT, self.config)

    def test_bad_config_does_not_silently_fall_back(self):
        p = self.definition("forecasting", [])
        p.write_text('{"version": 1, "cell": "forecasting", "projects": "airo"}')
        with self.assertRaises(ValueError):
            project(SNAPSHOT, self.config)

    def test_hostile_session_names_never_become_commands(self):
        data = copy.deepcopy(SNAPSHOT)
        data["sessions"][0]["name"] = "$(touch /tmp/unwanted)"
        result = project(data, self.config)
        row = next(r for c in result["cells"] for r in c["rows"] if r["name"].startswith("$("))
        self.assertIsNone(row["command"])

    def test_stopped_needs_help_but_idle_and_running_do_not(self):
        data = copy.deepcopy(SNAPSHOT)
        data["requests"] = []
        data["sessions"][0]["state"] = "stopped (codex)"
        data["sessions"][1]["state"] = "running (codex)"
        cell = project(data, self.config)["cells"][1]
        self.assertEqual(cell["needs_you"], 1)
        self.assertEqual(cell["status"], "1 working · 1 needs you")

    def test_all_runtime_suffixes_preserve_attention_and_attach_command(self):
        for agent in ("claude", "codex", "pi"):
            data = copy.deepcopy(SNAPSHOT)
            data["requests"] = []
            data["sessions"][0]["state"] = f"waiting on you (blocked) ({agent})"
            data["sessions"][1]["state"] = f"idle ({agent})"
            cell = project(data, self.config)["cells"][1]
            self.assertEqual(cell["needs_you"], 1)
            self.assertTrue(cell["rows"][0]["command"].startswith("kuggis forecasting "))


class HTTP(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.config = Path(self.tmp.name)
        self.env = patch.dict(os.environ, {"KUGGIS_CONFIG": str(self.config)})
        self.env.start()
        self.addCleanup(self.env.stop)
        stub = self.config / "kuggis"
        stub.write_text("#!/usr/bin/env python3\nimport json, sys\nassert sys.argv[1:] == ['_web', 'cells']\nprint(" + repr(json.dumps(SNAPSHOT)) + ")\n")
        stub.chmod(0o700)
        handler = runpy.run_path(str(ROOT / "web/server.py"))["Handler"]
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.server.token = "a" * 48
        self.server.kuggis = str(stub)
        self.server.hosts = {f"127.0.0.1:{self.server.server_port}"}
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop)

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def get(self, path, token=True, host=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        headers = {"X-Kuggis-Token": self.server.token} if token else {}
        if host:
            headers["Host"] = host
        conn.request("GET", path, headers=headers)
        response = conn.getresponse()
        body = response.read()
        conn.close()
        return response.status, body

    def test_cells_api_requires_token_and_local_host(self):
        self.assertEqual(self.get("/api/cells", token=False)[0], 403)
        self.assertEqual(self.get("/api/cells", host="attacker.example")[0], 403)
        status, body = self.get("/api/cells")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["cells"][1]["name"], "forecasting")

    def test_both_pages_and_shared_assets_are_served(self):
        for path, marker in [("/", b"/cells.js"), ("/reviews", b"/app.js"),
                             ("/common.js", b"function visible"), ("/cells.css", b".needs-you")]:
            status, body = self.get(path, token=False)
            self.assertEqual(status, 200, path)
            self.assertIn(marker, body)

    def test_invalid_definition_returns_error_not_empty_success(self):
        (self.config / "cells").mkdir()
        (self.config / "cells/forecasting.json").write_text("{")
        status, body = self.get("/api/cells")
        self.assertEqual(status, 409)
        self.assertIn("Cell definitions", json.loads(body)["error"])


class CLI(unittest.TestCase):
    def test_overview_never_queries_github_or_starts_a_review(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "bin").mkdir()
            (root / "vms").write_text("airo airo-dev example/airo airo\n")
            (root / "outbox").mkdir()
            (root / "outbox/task.md").write_text("---\nproject: airo\nsession: sample\n---\nBODY_NOT_FOR_DASHBOARD\n")
            (root / "outbox/review.md").write_text("---\nreview: example/airo#1\n---\nREVIEW_NOT_FOR_DASHBOARD\n")
            for name, text in {
                "ssh": "printf 'sample\\tworking\\t1m\\n'",
                "gh": 'echo unexpected-GitHub >&2; exit 99',
                "claude": 'echo unexpected-model >&2; exit 99',
            }.items():
                f = root / "bin" / name
                f.write_text("#!/bin/sh\n" + text + "\n")
                f.chmod(0o700)
            env = dict(os.environ, KUGGIS_CONFIG=tmp, KUGGIS_OUTBOX=str(root / "outbox"),
                       PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
            result = subprocess.run([str(ROOT / "kuggis"), "_web", "cells"], env=env,
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("unexpected", result.stderr)
            data = json.loads(result.stdout)
            self.assertEqual(data["sessions"][0]["name"], "sample")
            self.assertEqual(data["projects"], [{"name": "airo", "profile": "airo"}])
            self.assertEqual(len(data["requests"]), 1)
            self.assertEqual(data["requests"][0]["session"], "sample")
            self.assertIsInstance(data["requests"][0]["since"], int)
            self.assertNotIn("NOT_FOR_DASHBOARD", result.stdout)


if __name__ == "__main__":
    unittest.main()
