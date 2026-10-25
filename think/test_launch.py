"""The launcher: one wall, three agents, each with the door and no other exit."""
from pathlib import Path
import subprocess
import unittest

HERE = Path(__file__).resolve().parent


def launch(*args):
    return subprocess.run(["/bin/zsh", "-f", str(HERE / "launch"), *args], capture_output=True, text=True,
                          env={"KUGGIS_LAUNCH_DRY": "1", "PATH": "/usr/bin:/bin"})


class Launch(unittest.TestCase):
    def test_claude_is_the_default_and_has_no_shell(self):
        r = launch()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("claude: /opt/kuggis-thinker/claude --restricted"), r.stdout)
        flags = r.stdout.split("--append-system-prompt")[0]  # the prompt text is data, not flags
        self.assertIn("--tools Read,Edit,Write,Glob,Grep,WebSearch", flags)
        self.assertNotIn("WebFetch", flags)
        self.assertIn("--strict-mcp-config --mcp-config /opt/kuggis-thinker/mcp.json", r.stdout)

    def test_codex_gets_the_door_and_its_exits_turned_off(self):
        r = launch("--agent", "codex", "--model", "gpt-5")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("codex: codex -C /Users/thinker/workspace -s danger-full-access"), r.stdout)
        for want in ("mcp_servers.kuggis.command", "door.py", "web_search=\\\"disabled\\\"",
                     "--disable browser_use", "--disable apps", "--model gpt-5"):
            self.assertIn(want, r.stdout, want)

    def test_pi_gets_the_prompt_and_no_discovery(self):
        r = launch("--agent", "pi")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("pi: pi --no-extensions --no-skills --no-prompt-templates --no-context-files"), r.stdout)
        self.assertIn("--append-system-prompt /opt/kuggis-thinker/prompt.md", r.stdout)

    def test_unknown_agent_is_refused(self):
        r = launch("--agent", "gpt")
        self.assertEqual(r.returncode, 2)
        self.assertIn("unknown agent", r.stderr)


if __name__ == "__main__":
    unittest.main()
