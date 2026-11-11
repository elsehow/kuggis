"""Legacy software migration preserves state and refuses uncertain deletion."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from test_runtime import ROOT, module

c = module('cleanup_codex')
LAUNCHER = ROOT / 'recipes/codex/launch.py'


class Cleanup(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.central = self.home / '.local/lib/codex/0.165.0'
        (self.central / 'bin').mkdir(parents=True)
        for name in ('codex', 'codex-code-mode-host'):
            (self.central / 'bin' / name).write_bytes(b'package contents')
        binary = self.home / '.local/bin'
        binary.mkdir()
        (binary / 'codex').symlink_to(self.central / 'bin/codex')
        (binary / 'kuggis-codex').write_bytes(LAUNCHER.read_bytes())
        self.states = [self.home / '.codex', self.home / '.kuggis/agents/codex/research']
        self.releases = []
        for state in self.states:
            release = state / 'packages/app-server-daemon/releases/0.165.0-x86_64-unknown-linux-musl'
            shutil.copytree(self.central, release)
            self.releases.append(release)
            (state / 'state_5.sqlite').write_bytes(b'precious database')
            (state / 'auth.json').write_bytes(b'private state')
            (state / 'packages/app-server-daemon/current').symlink_to('releases/' + release.name)
        (self.central / '.release-sha256').write_text('receipt')
        self.agents = self.home / 'agents.json'
        self.agents.write_text(json.dumps({'agents': {'codex': {
            'launch': [str(binary / 'kuggis-codex'), 'default'],
            'chat_launch': [str(binary / 'kuggis-codex'), '{chat}']}}}))
        self.proc = self.home / 'proc'
        self.proc.mkdir()

    def run_cleanup(self, apply=False):
        return c.cleanup(self.home, LAUNCHER, self.agents, apply, self.proc)

    def test_preview_apply_and_repeat_preserve_all_state(self):
        plan = self.run_cleanup()
        self.assertTrue(plan['ok'])
        self.assertEqual([r['status'] for r in plan['releases']], ['eligible'] * 2)
        self.assertTrue(all(p.exists() for p in self.releases))
        result = self.run_cleanup(True)
        self.assertEqual([r['status'] for r in result['releases']], ['removed'] * 2)
        self.assertTrue((self.central / 'bin/codex').exists())
        for state in self.states:
            self.assertEqual((state / 'state_5.sqlite').read_bytes(), b'precious database')
            self.assertEqual((state / 'auth.json').read_bytes(), b'private state')
        self.assertEqual(self.run_cleanup(True)['releases'], [])

    def test_unknown_modified_and_linked_packages_are_retained(self):
        (self.releases[0] / 'bin/codex').write_bytes(b'modified')
        linked = self.releases[1] / 'bin/codex'
        linked.unlink()
        linked.symlink_to(self.central / 'bin/codex')
        old = self.releases[0].with_name('0.164.0-x86_64-unknown-linux-musl')
        shutil.copytree(self.central, old)
        result = self.run_cleanup(True)
        self.assertTrue(all(row['status'] == 'skipped' for row in result['releases']))
        self.assertTrue(old.exists())
        self.assertTrue(all(p.exists() for p in self.releases))

    def test_symlinked_ancestor_is_rejected(self):
        state = self.states[1]
        state.rename(state.with_name('saved'))
        state.symlink_to('saved', target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'symlinked'):
            self.run_cleanup(True)
        self.assertTrue(all(p.exists() for p in self.releases))

    def test_old_launcher_or_custom_recipe_blocks_cleanup(self):
        installed = self.home / '.local/bin/kuggis-codex'
        installed.write_text('old launcher')
        with self.assertRaisesRegex(ValueError, 'upgrade'):
            self.run_cleanup(True)
        installed.write_bytes(LAUNCHER.read_bytes())
        self.agents.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'configured'):
            self.run_cleanup(True)

    def fake_process(self):
        process = self.proc / '123'
        process.mkdir()
        (process / 'maps').write_bytes(b'')
        (process / 'cmdline').write_bytes(b'codex\0--no-daemon')
        (process / 'fd').mkdir()
        (process / 'exe').symlink_to(self.central / 'bin/codex')
        return process

    def test_active_executable_mapping_descriptor_and_arguments_block_removal(self):
        process = self.fake_process()
        target = self.releases[0] / 'bin/codex'
        for kind in ('exe', 'maps', 'fd', 'cmdline'):
            with self.subTest(kind=kind):
                if kind == 'exe':
                    (process / 'exe').unlink()
                    (process / 'exe').symlink_to(target)
                elif kind == 'fd':
                    (process / 'fd/7').symlink_to(target)
                else:
                    (process / kind).write_text(str(target))
                self.assertFalse(self.run_cleanup(True)['ok'])
                self.assertTrue(all(p.exists() for p in self.releases))
                if kind == 'exe':
                    (process / 'exe').unlink()
                elif kind == 'fd':
                    (process / 'fd/7').unlink()
                else:
                    (process / kind).write_bytes(b'')

    def test_incomplete_process_visibility_fails_closed(self):
        process = self.fake_process()
        (process / 'maps').unlink()
        report = self.run_cleanup(True)
        self.assertFalse(report['ok'])
        self.assertIn('cannot fully inspect process 123', report['blockers'])
        self.assertTrue(all(p.exists() for p in self.releases))

    def test_active_shared_chat_does_not_block_cleanup(self):
        self.fake_process()
        self.assertTrue(self.run_cleanup(True)['ok'])
        self.assertTrue(all(not p.exists() for p in self.releases))

    def test_shared_codex_without_no_daemon_blocks_cleanup(self):
        process = self.fake_process()
        (process / 'cmdline').write_bytes(b'codex\0app-server')
        report = self.run_cleanup(True)
        self.assertFalse(report['ok'])
        self.assertIn('without --no-daemon', report['blockers'][0])
        self.assertTrue(all(p.exists() for p in self.releases))

    def test_hardlinked_files_are_retained(self):
        binary = self.releases[0] / 'bin/codex'
        binary.unlink()
        os.link(self.central / 'bin/codex', binary)
        result = self.run_cleanup(True)
        self.assertEqual(result['releases'][0]['status'], 'skipped')
        self.assertIn('hard-linked', result['releases'][0]['reason'])
        self.assertTrue(binary.exists())

    def test_inventory_change_aborts_before_any_removal(self):
        def change(*args):
            (self.releases[1] / 'bin/codex').write_bytes(b'changed')
            return []
        with patch.object(c, 'process_blockers', side_effect=change):
            with self.assertRaisesRegex(ValueError, 'inventory changed'):
                self.run_cleanup(True)
        self.assertTrue(all(p.exists() for p in self.releases))


class SharedHarness(unittest.TestCase):
    def test_six_real_launcher_invocations_share_executable_and_home(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            shared = home / '.local/share/kuggis/codex'
            shared.mkdir(parents=True)
            (shared / 'config.toml').write_text('model = "test"\n')
            binary = home / '.local/lib/codex/test/bin/codex'
            binary.parent.mkdir(parents=True)
            # A recording executable observes the actual exec boundary, including
            # CODEX_HOME. No network or native Codex credentials are required.
            binary.write_text('#!' + sys.executable + '\n' +
                'import json, os, pathlib, sys\n'
                'state = next(v.split("=", 1)[1] for v in sys.argv if v.startswith("sqlite_home="))\n'
                'pathlib.Path(json.loads(state), "invocation.json").write_text(json.dumps({\n'
                '"args": sys.argv, "home": os.environ["CODEX_HOME"],\n'
                '"exe": str(pathlib.Path(sys.argv[0]).resolve())}))\n')
            binary.chmod(0o755)
            link = home / '.local/bin/codex'
            link.parent.mkdir()
            link.symlink_to(binary)
            before = binary.read_bytes()
            for name in ('worldview', 'fb', 'fbsim', 'airo', 'leap', 'dwh'):
                subprocess.run([sys.executable, str(LAUNCHER), name],
                               env={**os.environ, 'HOME': str(home)}, check=True, capture_output=True)
                state = home / '.kuggis/agents/codex' / name
                observed = json.loads((state / 'invocation.json').read_text())
                self.assertEqual(observed['exe'], str(binary))
                self.assertEqual(observed['home'], str(home / '.codex'))
                self.assertIn('--no-daemon', observed['args'])
                self.assertFalse((state / 'packages').exists())
            self.assertEqual(binary.read_bytes(), before)
            self.assertEqual(list(home.rglob('packages')), [])
