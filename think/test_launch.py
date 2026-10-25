"""The launcher: one wall, three agents, one list of doors, no other exit."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
MCP = {"mcpServers": {
    "kuggis": {"command": "/usr/bin/python3", "args": ["-I", "/opt/kuggis-thinker/door.py", "serve"]},
    "bigbrain": {"type": "http", "url": "http://127.0.0.1:4748/mcp",
                 "headers": {"Authorization": "Bearer ${BIGBRAIN_TOKEN}"}},
}}


class Launch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.room = Path(cls.tmp.name)
        (cls.room / "mcp.json").write_text(json.dumps(MCP))
        (cls.room / "prompt.md").write_text("You are the Thinker.\n")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def launch(self, *args):
        return subprocess.run(["/bin/zsh", "-f", str(HERE / "launch"), *args], capture_output=True, text=True,
                              env={"KUGGIS_LAUNCH_DRY": "1", "KUGGIS_ROOM": str(self.room), "PATH": "/usr/bin:/bin"})

    def test_claude_is_the_default_and_has_no_shell(self):
        r = self.launch()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith(f"claude: {self.room}/claude --restricted"), r.stdout)
        flags = r.stdout.split("--append-system-prompt")[0]  # the prompt text is data, not flags
        self.assertIn("--tools Read,Edit,Write,Glob,Grep,WebSearch", flags)
        self.assertNotIn("WebFetch", flags)
        self.assertIn(f"--strict-mcp-config --mcp-config {self.room}/mcp.json", flags)

    def test_codex_gets_both_doors_and_its_exits_turned_off(self):
        r = self.launch("--agent", "codex", "--model", "gpt-5")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("codex: codex -C /Users/thinker/workspace -s danger-full-access"), r.stdout)
        for want in ('mcp_servers.kuggis.command=\\"/usr/bin/python3\\"', "door.py",
                     'mcp_servers.bigbrain.url=\\"http://127.0.0.1:4748/mcp\\"',
                     'mcp_servers.bigbrain.bearer_token_env_var=\\"BIGBRAIN_TOKEN\\"',
                     'web_search=\\"disabled\\"', "--disable browser_use", "--disable apps", "--model gpt-5"):
            self.assertIn(want, r.stdout, want)
        self.assertNotIn("Bearer", r.stdout)  # the token itself never appears on a command line

    def test_pi_gets_the_prompt_and_no_discovery(self):
        r = self.launch("--agent", "pi")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("pi: pi --no-extensions --no-skills --no-prompt-templates --no-context-files"), r.stdout)
        self.assertIn(f"--append-system-prompt {self.room}/prompt.md", r.stdout)

    def test_unknown_agent_is_refused(self):
        r = self.launch("--agent", "gpt")
        self.assertEqual(r.returncode, 2)
        self.assertIn("unknown agent", r.stderr)


if __name__ == "__main__":
    unittest.main()
