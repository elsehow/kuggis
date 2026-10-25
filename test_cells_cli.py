"""Exercise the real CLI with fake SSH, upload, and model commands."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from cells.display import project, resolve

ROOT = Path(__file__).resolve().parent


class CellCLI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.conf = self.root / "conf"
        self.conf.mkdir()
        (self.conf / "cells").mkdir()
        self.map = self.conf / "vms"
        self.map.write_text("airo airo-vm example/airo airo\nkuggis tools-vm example/kuggis -\n")
        self.log = self.root / "log"
        self.log.touch()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, code in {
            "ssh": '''printf 'ssh %s\\n' "$*" >>"$LOG"
case "$*" in
  *'session.sh status'*) printf 'demo\\tworking\\t1m\\n' ;;
  *'echo $HOME'*) echo /home/box ;;
esac
cat >/dev/null''',
            "scp": 'printf "scp %s\\n" "$*" >>"$LOG"',
            "pbcopy": 'cat >/dev/null',
            "gh": 'echo forbidden-gh >>"$LOG"; exit 99',
            "claude": 'echo forbidden-model >>"$LOG"; exit 99',
            "security": 'echo forbidden-keychain >>"$LOG"; exit 99',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\n" + code + "\n")
            path.chmod(0o700)
        (self.root / "notes.txt").write_text("synthetic test file\n")
        self.env = dict(os.environ, HOME=str(self.root), KUGGIS_CONFIG=str(self.conf),
                        KUGGIS_OUTBOX=str(self.root / "outbox"), LOG=str(self.log),
                        PATH=str(self.bin) + os.pathsep + os.environ["PATH"])

    def run_cli(self, *args):
        return subprocess.run([str(ROOT / "kuggis"), *args], cwd=self.root, env=self.env,
                              input="", text=True, capture_output=True, timeout=10)

    def add_second_project(self):
        with self.map.open("a") as f:
            f.write("agent-box agents-vm example/agent-box -\n")

    def test_attach_cell_and_legacy_project_use_same_vm(self):
        for target in ("forecasting", "airo"):
            result = self.run_cli(target, "demo", "--codex")
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0], calls[1])
        self.assertIn("airo-vm.exe.xyz ~/.box/session.sh attach demo codex", calls[0])

    def test_all_agent_flags_route_without_model_or_github_calls(self):
        for agent in ("claude", "codex", "pi"):
            for flags in [("--agent", agent), ("--" + agent,)]:
                result = self.run_cli("forecasting", "demo", *flags)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("session.sh attach demo " + agent, self.log.read_text())
        self.assertNotIn("forbidden", self.log.read_text())

    def test_unknown_agent_fails_before_network(self):
        for flags in [("--agent", "oops"), ("--agent",)]:
            result = self.run_cli("forecasting", "demo", *flags)
            self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.log.read_text(), "")

    def test_done_preserves_work_checks_and_force_is_explicit(self):
        result = self.run_cli("done", "forecasting", "demo")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("session.sh done demo", self.log.read_text())
        self.assertNotIn("--force", self.log.read_text())
        result = self.run_cli("done", "forecasting", "demo", "--force")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("done demo --force", self.log.read_text())

    def test_put_routes_cell_without_guessing_file_positions(self):
        result = self.run_cli("put", "forecasting", "demo", "notes.txt")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("notes.txt airo-vm.exe.xyz:inbox/", self.log.read_text())

    def test_sh_routes_cell_and_keeps_command_arguments(self):
        result = self.run_cli("sh", "forecasting", "--", "printf", "hello")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("airo-vm.exe.xyz printf hello", self.log.read_text())

    def test_ambiguous_cell_never_guesses_or_contacts_a_vm(self):
        self.add_second_project()
        for args in [("forecasting", "demo"), ("done", "forecasting", "demo"),
                     ("put", "forecasting", "demo", "notes.txt"), ("sync", "forecasting"),
                     ("sh", "forecasting")]:
            result = self.run_cli(*args)
            self.assertNotEqual(result.returncode, 0, args)
            self.assertIn("choose --project", result.stderr)
        self.assertEqual(self.log.read_text(), "")

    def test_selector_routes_attach_done_put_and_shell(self):
        self.add_second_project()
        for args in [("forecasting", "demo", "--project", "agent-box"),
                     ("done", "forecasting", "demo", "--project", "agent-box"),
                     ("put", "forecasting", "demo", "notes.txt", "--project", "agent-box"),
                     ("sh", "forecasting", "--project", "agent-box", "hostname")]:
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("airo-vm", self.log.read_text())
        self.assertIn("agents-vm.exe.xyz", self.log.read_text())

    def test_wrong_member_missing_selector_and_extra_done_args_are_rejected(self):
        for args in [("forecasting", "demo", "--project", "kuggis"),
                     ("done", "forecasting", "demo", "--project"),
                     ("done", "forecasting", "demo", "--force", "extra")]:
            self.assertNotEqual(self.run_cli(*args).returncode, 0)
        self.assertEqual(self.log.read_text(), "")

    def test_default_and_ls_share_grouping_without_github_or_model_calls(self):
        for args in [(), ("ls",), ("ls", "forecasting")]:
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("forecasting · 1 working", result.stdout)
            self.assertIn("airo/demo", result.stdout)
        filtered = self.run_cli("ls", "forecasting")
        self.assertNotIn("operator", filtered.stdout)
        self.assertNotIn("forbidden", self.log.read_text())

    def test_cell_inventory_and_definition_are_local_reads(self):
        self.assertIn("forecasting", self.run_cli("cells").stdout)
        self.assertIn("not an enforced policy", self.run_cli("cell", "forecasting").stdout)
        self.assertEqual(self.log.read_text(), "")

    def test_private_membership_is_used_by_cli_and_web_commands(self):
        (self.conf / "cells/research.json").write_text(json.dumps({
            "version": 1, "cell": "research", "projects": ["airo"],
        }))
        self.assertEqual(resolve(self.conf, "research"), "airo")
        result = self.run_cli("research", "demo")
        self.assertEqual(result.returncode, 0, result.stderr)
        snapshot = {"projects": [{"name": "airo", "profile": "airo"}], "now": 0,
                    "sessions": [{"project": "airo", "name": "demo", "state": "working", "age": "1m"}]}
        self.assertEqual(project(snapshot, self.conf)["cells"][0]["rows"][0]["command"],
                         "kuggis research demo")

    def test_multi_project_web_command_round_trips_through_cli(self):
        self.add_second_project()
        snapshot = {"projects": [{"name": "airo", "profile": "airo"}, {"name": "agent-box", "profile": "-"}],
                    "sessions": [{"project": "airo", "name": "demo", "state": "working", "age": "1m"}], "now": 0}
        command = project(snapshot, self.conf)["cells"][0]["rows"][0]["command"]
        self.assertEqual(command, "kuggis forecasting demo --project airo")
        result = self.run_cli(*command.split()[1:])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("airo-vm.exe.xyz", self.log.read_text())

    def test_command_names_cannot_become_cell_aliases(self):
        (self.conf / "cells/sync.json").write_text(json.dumps({
            "version": 1, "cell": "sync", "projects": ["airo"],
        }))
        self.assertNotEqual(self.run_cli("cells").returncode, 0)
        self.assertEqual(self.log.read_text(), "")

    def test_terminal_items_use_cells_and_stable_session_payloads(self):
        result = self.run_cli("_cell_items")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cell\tforecasting", result.stdout)
        self.assertIn("\tairo\tdemo", result.stdout)
        self.assertNotIn("forbidden", self.log.read_text())


if __name__ == "__main__":
    unittest.main()
