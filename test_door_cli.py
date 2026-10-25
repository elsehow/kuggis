"""The carrier (kuggis watch) and the gate (kuggis dispatch) through the real
CLI, with fake ssh, a scratch outbox and inbox, and no GitHub or model."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "think"))
import door  # noqa: E402

SCREEN = "$ claude\n> working on the instrument\n"


class DoorCLI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.conf, self.outbox, self.inbox = root / "conf", root / "outbox", root / "inbox"
        for d in (self.conf, self.conf / "cells", self.outbox, self.inbox, root / "bin"):
            d.mkdir()
        (self.conf / "vms").write_text("airo airo-vm example/airo airo\n")
        self.log = root / "log"
        self.log.touch()
        import base64
        screen = base64.b64encode(SCREEN.encode()).decode()
        stubs = {
            "ssh": f'''printf 'ssh %s\\n' "$*" >>"$LOG"
case "$*" in
  *'session.sh snapshot'*) printf 'v10\\tworking\\t3m\\n--\\nv10\\t{screen}\\n' ;;
  *'echo $HOME'*) echo /home/box ;;
  *deliver*) echo "started" ;;
esac
cat >/dev/null''',
            "scp": 'printf "scp %s\\n" "$*" >>"$LOG"',
            "gh": 'echo forbidden-gh >>"$LOG"; exit 99',
            "claude": 'echo forbidden-model >>"$LOG"; exit 99',
        }
        for name, code in stubs.items():
            f = root / "bin" / name
            f.write_text("#!/bin/sh\n" + code + "\n")
            f.chmod(0o700)
        self.env = dict(os.environ, HOME=str(root), KUGGIS_CONFIG=str(self.conf), LOG=str(self.log),
                        KUGGIS_OUTBOX=str(self.outbox), KUGGIS_ROOM_INBOX=str(self.inbox),
                        PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
        door.OUTBOX, door.INBOX = str(self.outbox), str(self.inbox)
        door.REPLIES, door.STATE = str(self.inbox / "replies"), str(self.inbox / "state.json")

    def kuggis(self, *args, stdin="", env=None):
        return subprocess.run([str(ROOT / "kuggis"), *args], env=env or self.env, input=stdin,
                              text=True, capture_output=True, timeout=60)

    def test_watch_writes_state_and_leaves_briefs_for_the_person(self):
        rid = door.write_request("brief", {"cell": "forecasting", "session": "v11", "agent": "claude"}, "Do the next thing.")
        r = self.kuggis("watch", "--once")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "")  # nothing carried: a brief waits
        state = json.loads((self.inbox / "state.json").read_text())
        (cell,) = [c for c in state["cells"] if c["name"] == "forecasting"]
        kinds = {(row["kind"], row["name"]): row for row in cell["rows"]}
        self.assertEqual(kinds[("session", "v10")]["screen"], SCREEN)
        self.assertEqual(kinds[("session", "v10")]["state"], "working")
        self.assertEqual(kinds[("door", "brief / v11")]["state"], "awaiting dispatch")
        self.assertTrue(kinds[("door", "brief / v11")]["needs_you"])
        self.assertEqual(door.reply(rid), ("waiting", ""))
        self.assertIn("forbidden-gh", self.log.read_text())  # the carrier asks GitHub for PRs; the home never does
        self.assertNotIn("forbidden-model", self.log.read_text())
        self.assertEqual(state["cells"][0]["rows"][0]["kind"], "thinker")
        self.assertIn("1 waiting", self.kuggis("_waiting").stdout)

    def test_dispatch_sends_a_brief_and_replies(self):
        rid = door.write_request("brief", {"cell": "forecasting", "session": "v11", "agent": "codex"}, "Do it.")
        r = self.kuggis("dispatch", stdin="d\n", env=dict(self.env, KUGGIS_PAGER="cat"))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("goes to  airo / v11 (codex)", r.stdout)
        self.assertIn("attach with: kuggis airo v11", r.stdout)
        self.assertIn("session.sh deliver v11 codex", self.log.read_text())
        status, body = door.reply(rid)
        self.assertEqual((status, body.strip()), ("sent", "attach with: kuggis airo v11"))
        self.assertIn(f"\tsent\t{rid}\t", (self.conf / "dispatch.log").read_text())
        self.assertEqual(self.kuggis("dispatch", stdin="").stdout.strip().splitlines()[0],
                         f"kuggis: nothing waiting for you in {self.outbox}")

    def test_a_bad_brief_cannot_be_dispatched_and_can_be_rejected(self):
        rid = door.write_request("brief", {"cell": "hiring", "session": "x", "agent": "claude"}, "Nope.")
        r = self.kuggis("dispatch", stdin="d\nr\n", env=dict(self.env, KUGGIS_PAGER="cat"))
        self.assertIn("PROBLEM  kuggis: cell 'hiring' has no configured VM projects", r.stdout)
        self.assertIn("can't dispatch", r.stdout)
        self.assertEqual(door.reply(rid)[0], "rejected")
        self.assertNotIn("deliver", self.log.read_text())

    def test_hand_written_brief_without_kind_still_works(self):
        (self.outbox / "task.md").write_text("---\nproject: airo\nsession: task\n---\nOld style.\n")
        r = self.kuggis("dispatch", stdin="d\n", env=dict(self.env, KUGGIS_PAGER="cat"))
        self.assertIn("brief    task.md", r.stdout)
        self.assertIn("session.sh deliver task claude", self.log.read_text())

    def test_search_is_carried_and_refused_without_a_backend(self):
        rid = door.write_request("search", {"query": "example domains"})
        r = self.kuggis("watch", "--once", env=dict(self.env, TAVILY_API_KEY=""))
        self.assertIn("search search-", r.stdout)
        status, body = door.reply(rid)
        self.assertEqual(status, "refused")
        self.assertIn("no search backend", body)

    def test_fetch_waits_unless_vouched_for(self):
        rid = door.write_request("fetch", {"url": "http://127.0.0.1:1/private"})
        self.kuggis("watch", "--once")
        self.assertEqual(door.reply(rid)[0], "waiting")
        r = self.kuggis("url", "http://127.0.0.1:1/private")
        self.assertEqual(r.stdout.strip(), "known: http://127.0.0.1:1/private")
        self.kuggis("watch", "--once")
        status, body = door.reply(rid)  # vouched for, carried, but the door's URL rule still holds
        self.assertEqual(status, "refused")
        self.assertIn("public hosts", body)
        self.assertEqual(self.kuggis("url", "ftp://x").returncode, 1)

    def test_held_fetch_can_be_done_from_the_gate(self):
        rid = door.write_request("fetch", {"url": "http://localhost:1/x"})
        r = self.kuggis("dispatch", stdin="d\n", env=dict(self.env, KUGGIS_PAGER="cat"))
        self.assertIn("fetches  http://localhost:1/x", r.stdout)
        self.assertIn("[d]o it", r.stdout)
        self.assertEqual(door.reply(rid)[0], "refused")

    def test_home_items_show_the_waiting_request_without_github(self):
        door.write_request("brief", {"cell": "forecasting", "session": "v11", "agent": "claude"}, "B")
        r = self.kuggis("_cell_items")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("brief / v11", r.stdout)
        self.assertIn("awaiting dispatch", r.stdout)
        self.assertNotIn("forbidden", self.log.read_text())


if __name__ == "__main__":
    unittest.main()
