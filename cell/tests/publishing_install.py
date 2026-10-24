"""Host provisioning tests with temporary paths and a simulated service manager."""
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SOURCE = (Path(__file__).parents[1] / 'src/publish/install.py').read_text()

class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = SOURCE.replace('/etc/cell/', f'{self.root}/etc/').replace('/run/cell-publish/', f'{self.root}/shared/').replace('/usr/local/lib/cell/', f'{self.root}/runtime/')
        (self.root / 'etc').mkdir()
        (self.root / 'shared').mkdir()
        (self.root / 'etc/key.pub').write_text('verified-key')
        self.config = {'socket': f'{self.root}/shared/bridge.sock', 'namespace': 'namespace', 'audience': 'audience', 'endpoint': 'https://publisher.example'}
        self.context = {'key': 'verified-key', 'publishing': {'config': self.config, 'sites': [], 'error': None}}
        self.calls = []
        self.active = False
        self.fail = False
    def tearDown(self): self.temp.cleanup()
    def service(self, args, **kwargs):
        self.calls.append(args[1])
        status = 0
        if args[1] == 'is-active': status = 0 if self.active else 3
        elif args[1] == 'restart':
            if self.fail: raise subprocess.CalledProcessError(1, args)
            self.active = True
            (self.root / 'shared/publisher.json').write_bytes((self.root / 'etc/publisher.json').read_bytes())
        elif args[1] == 'stop': self.active = False
        else: raise AssertionError(args)
        if kwargs.get('check') and status: raise subprocess.CalledProcessError(status, args)
        return subprocess.CompletedProcess(args, status)
    def install(self):
        with patch('sys.stdin', io.StringIO(json.dumps(self.context))), patch('subprocess.run', self.service):
            exec(compile(self.source, 'host-publisher-install', 'exec'), {})
        return json.loads((self.root / 'runtime/context.json').read_text())
    def test_install_is_idempotent_and_revocation_stops_only_the_managed_bridge(self):
        self.assertEqual(self.install()['publishing']['config'], self.config)
        self.install()
        self.assertEqual(self.calls.count('restart'), 1)
        self.assertEqual((self.root / 'etc/publisher.json').stat().st_mode & 0o777, 0o644)
        self.context['publishing'] = {'config': None, 'sites': [], 'error': 'Revoked'}
        self.install()
        self.assertFalse(self.active)
        self.assertFalse((self.root / 'etc/publisher.json').exists())
        self.assertFalse((self.root / 'shared/publisher.json').exists())
        # No chat/environment restart is part of this script.
        self.assertEqual(set(self.calls), {'is-active', 'restart', 'stop'})
    def test_failed_service_reports_unavailable_and_can_be_retried(self):
        self.fail = True
        result = self.install()
        self.assertIsNone(result['publishing']['config'])
        self.assertIn('runtime', result['publishing']['error'])
        self.fail = False
        self.assertEqual(self.install()['publishing']['config'], self.config)
    def test_wrong_pinned_key_cannot_write_config(self):
        self.context['key'] = 'another-cell'
        with self.assertRaisesRegex(RuntimeError, 'key changed'): self.install()
        self.assertFalse((self.root / 'etc/publisher.json').exists())

if __name__ == '__main__': unittest.main()
