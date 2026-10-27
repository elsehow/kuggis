"""Preview guidance preservation and client transport lifecycle."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from test_runtime import g, ROOT


class Guidance(unittest.TestCase):
    def test_reapply_preserves_user_instructions_and_updates_only_managed_block(self):
        original = 'User instructions.\n'
        first = g.merge_web_guidance(original, [{'name':'app','guest_port':8787,'host_port':8000}])
        self.assertTrue(first.startswith(original))
        self.assertIn('8787', first)
        self.assertEqual(g.merge_web_guidance(first, [{'name':'app','guest_port':8787,'host_port':8000}]), first)
        updated = g.merge_web_guidance(first + '\nMore user instructions.\n', [])
        self.assertTrue(updated.startswith(original))
        self.assertTrue(updated.endswith('\nMore user instructions.\n'))
        self.assertNotIn('8787', updated)
        self.assertIn('No private preview ports', updated)

    def test_corrupt_markers_fail_without_discarding_text(self):
        for text in [g.WEB_START, g.WEB_END, g.WEB_END + g.WEB_START,
                     g.WEB_START + g.WEB_START + g.WEB_END]:
            with self.assertRaises(ValueError):
                g.merge_web_guidance(text, [])


class WebClient(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cli = ROOT/'target/debug/kuggis'
        self.config = self.root/'workspace.json'
        self.config.write_text(json.dumps({'version':2,'name':'demo','ssh':'demo-host','launch':['bash'],
            'private_web':[{'name':'preview','guest_port':8787,'host_port':8000}]}))
        self.env = {**os.environ, 'PATH':str(self.root)}

    def script(self, name, body):
        path = self.root/name
        path.write_text(f'#!{sys.executable}\n' + body)
        path.chmod(0o700)

    def test_failed_transport_never_opens_browser_or_prints_url(self):
        for opener in ['open', 'xdg-open']:
            self.script(opener, f'from pathlib import Path\nPath({str(self.root/"opened")!r}).touch()\n')
        self.script('ssh', 'import sys\nsys.exit(255)\n')
        result = subprocess.run([self.cli,'web',self.config], env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b'')
        self.assertFalse((self.root/'opened').exists())
        self.assertIn(b'tunnel did not start', result.stderr)

    def test_no_open_holds_tunnel_even_with_closed_caller_stdin_and_releases_on_exit(self):
        marker = self.root/'closed'
        self.script('ssh', f'import sys\nfrom pathlib import Path\nprint("kuggis-web-ready", flush=True)\nsys.stdin.read()\nPath({str(marker)!r}).touch()\n')
        process = subprocess.Popen([self.cli,'web',self.config,'--no-open'], env=self.env,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(), b'http://127.0.0.1:8000/\n')
            with self.assertRaises(subprocess.TimeoutExpired):
                process.wait(timeout=.2)
            self.assertIsNone(process.poll())
            process.terminate()
            process.communicate(timeout=5)
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(marker.exists(), 'SSH must see EOF when client exits')
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)
