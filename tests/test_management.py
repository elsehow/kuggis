"""Grouped CLI and live-policy transactions, using a fake SSH transport."""
import copy
import fcntl
import hashlib
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

sys.path.insert(0, str(ROOT / 'crates/kuggis/runtime'))
m = module('manage')
host = module('manage_host')
installer = module('host')


class ManagementCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.remote = self.root / 'remote'
        self.remote.mkdir(mode=0o700)
        self.state = self.root / 'state'
        (self.state / 'fri').mkdir(parents=True)
        self.config = self.state / 'fri/workspace.json'
        self.workspace = {'version': 2, 'name': 'fri', 'ssh': 'fixture', 'launch': ['bash'],
                          'packages': [], 'resources': [{'name': 'existing', 'endpoints': [
                              {'host': 'example.com', 'port': 443}], 'credentials': []}]}
        self.config.write_text(json.dumps(self.workspace))
        (self.remote / 'workspace.json').write_bytes(self.config.read_bytes())
        (self.remote / 'workspace.json').chmod(0o600)
        (self.remote / 'setup.lock').touch(mode=0o600)
        (self.root / 'bin').mkdir()
        ssh = self.root / 'bin/ssh'
        shutil.copyfile(ROOT / 'tests/fake_management_ssh.py', ssh)
        ssh.chmod(0o755)
        self.binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
        self.env = {**os.environ, 'KUGGIS_STATE_DIR': str(self.state), 'KUGGIS_BINARY': self.binary,
                    'KUGGIS_RUNTIME_ROOT': str(ROOT / 'crates/kuggis/runtime'),
                    'KUGGIS_MANAGEMENT_FIXTURE': str(self.remote),
                    'PATH': str(self.root / 'bin') + ':' + str(Path(sys.executable).parent) + ':' + os.environ['PATH']}

    def cli(self, *args, ok=True):
        result = subprocess.run([self.binary, *args, '--json'], env=self.env, capture_output=True, timeout=30)
        self.assertEqual(result.returncode == 0, ok, result.stderr.decode() + result.stdout.decode())
        return json.loads(result.stdout)

    def calls(self):
        path = self.remote / 'calls'
        return path.read_text().splitlines() if path.exists() else []

    def test_network_plan_apply_idempotence_revoke_and_local_publication(self):
        # Preserve an intervening host-side resource, even when the local cache is stale.
        live = copy.deepcopy(self.workspace)
        live['resources'].append({'name': 'another', 'endpoints': [{'host': 'other.example', 'port': 443}], 'credentials': []})
        (self.remote / 'workspace.json').write_text(json.dumps(live))
        original = self.config.read_bytes()
        report = self.cli('access', 'grant', 'fri', 'network', 'api.anthropic.com:443', '--plan')
        self.assertEqual(report['restart'], 'proxy')
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.config.read_bytes(), original)
        report = self.cli('access', 'grant', 'fri', 'network', 'api.anthropic.com:443')
        self.assertTrue(report['verification']['reachable'])
        self.assertEqual(json.loads(self.config.read_text()), json.loads((self.remote / 'workspace.json').read_text()))
        self.assertEqual(json.loads(self.config.read_text())['resources'][:2], live['resources'])
        report = self.cli('access', 'grant', 'fri', 'network', 'api.anthropic.com:443')
        self.assertFalse(report['changed'])
        self.assertEqual(self.calls(), ['restart-proxy'])
        self.assertTrue(self.cli('access', 'check', 'fri', 'network', 'api.anthropic.com:443')['granted'])
        report = self.cli('access', 'revoke', 'fri', 'network', 'api.anthropic.com:443')
        self.assertEqual(report['verification']['proxy_status'], 403)
        self.assertEqual(self.cli('access', 'check', 'fri', 'network', 'api.anthropic.com:443', ok=False)['reason'], 'not_granted')
        self.assertEqual(self.calls(), ['restart-proxy', 'restart-proxy'])

    def test_failed_proxy_restart_restores_host_and_leaves_operator_unchanged(self):
        original = self.config.read_bytes()
        (self.remote / 'fail-restart').touch()
        self.cli('access', 'grant', 'fri', 'network', 'api.anthropic.com:443', ok=False)
        self.assertEqual(self.config.read_bytes(), original)
        self.assertEqual((self.remote / 'workspace.json').read_bytes(), original)
        self.assertEqual(self.calls(), ['restart-proxy', 'restart-proxy'])

    def test_concurrent_host_edit_rejects_apply_without_lost_update(self):
        original = self.config.read_bytes()
        (self.remote / 'concurrent-edit').touch()
        report = self.cli('access', 'grant', 'fri', 'network', 'api.anthropic.com:443', ok=False)
        self.assertIn('concurrently', report['error']['message'])
        self.assertEqual(self.config.read_bytes(), original)
        self.assertEqual(json.loads((self.remote / 'workspace.json').read_text())['packages'], ['make'])
        self.assertEqual(self.calls(), [])

    def test_configuration_plans_restart_guard_and_explicit_upgrade(self):
        patchfile = self.root / 'patch.json'
        patchfile.write_text(json.dumps({'packages': ['ripgrep']}))
        plan = self.cli('cell', 'configure', 'fri', '--file', str(patchfile), '--plan')
        self.assertEqual(plan['restart'], 'cell')
        self.assertEqual(self.calls(), [])
        error = self.cli('cell', 'configure', 'fri', '--file', str(patchfile), ok=False)
        self.assertIn('--restart', error['error']['message'])
        self.assertEqual(self.calls(), [])
        linux = self.root / 'linux'
        linux.write_bytes(b'\x7fELF\x02\x01' + b'\0' * 32)
        self.cli('cell', 'configure', 'fri', '--file', str(patchfile), '--restart', '--bin', str(linux))
        self.assertEqual(self.calls(), ['install'])
        self.assertEqual(json.loads(self.config.read_text())['packages'], ['ripgrep'])
        self.assertEqual(json.loads(self.config.read_text())['resources'], [
            {'name': 'existing', 'endpoints': [{'host': 'example.com', 'port': 443, 'destination': {'kind': 'public'}}], 'credentials': []}])

    def test_network_only_configure_does_not_restart_cell_and_identity_cannot_change(self):
        patchfile = self.root / 'patch.json'
        resources = copy.deepcopy(self.workspace['resources'])
        resources[0]['endpoints'].append({'host': 'another.example', 'port': 443})
        patchfile.write_text(json.dumps({'resources': resources}))
        self.assertEqual(self.cli('cell', 'configure', 'fri', '--file', str(patchfile))['restart'], 'proxy')
        self.assertEqual(self.calls(), ['restart-proxy'])
        patchfile.write_text('{"ssh":"different-host"}')
        self.cli('cell', 'configure', 'fri', '--file', str(patchfile), '--plan', ok=False)
        self.assertEqual(self.calls(), ['restart-proxy'])

    def test_internet_mode_revoke_is_not_a_false_deny_rule(self):
        live = {**self.workspace, 'egress': 'internet'}
        (self.remote / 'workspace.json').write_text(json.dumps(live))
        report = self.cli('access', 'revoke', 'fri', 'network', 'example.com:443', ok=False)
        self.assertIn('internet', report['error']['message'])
        self.assertEqual(self.calls(), [])

    def test_invalid_arguments_and_local_listing_never_mutate(self):
        for words in [('access', 'grant', 'fri', 'network', 'https://example.com'),
                      ('access', 'grant', 'fri', 'network', 'example.com:0'),
                      ('access', 'grant', 'fri', 'repo', 'owner/project'),
                      ('access', 'grant', 'fri', 'network', 'example.com:443', '--role', 'develop')]:
            self.cli(*words, ok=False)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.cli('cell', 'list')['cells'][0]['name'], 'fri')
        self.assertTrue(self.cli('cell', 'status', 'fri')['ok'])


class PolicyGuards(unittest.TestCase):
    def test_read_only_host_operations_share_lock_and_mutations_require_exclusivity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'workspace.json'
            path.write_text('{"name":"fri","resources":[]}')
            path.chmod(0o600)
            with (root / 'setup.lock').open('w') as lock, patch.object(host, 'CONFIG', path), \
                    patch.object(host, 'services', return_value={}):
                fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
                self.assertEqual(host.handle({'action': 'inspect', 'cell': 'fri'})['workspace']['name'], 'fri')
                with self.assertRaises(BlockingIOError):
                    host.handle({'action': 'network', 'cell': 'fri', 'revision': host.revision(path.read_bytes()),
                                 'workspace': {'name': 'fri', 'resources': []}})

    def test_installer_requires_explicit_boolean_restart_and_matching_revision(self):
        current = {'name': 'fri'}
        for request in ({}, {'allow_restart': 'true'}, {'allow_restart': 1}):
            with self.assertRaisesRegex(RuntimeError, '--restart'):
                installer.check_update(request, current, current)
        installer.check_update({}, None, current)
        installer.check_update({'allow_restart': True}, current, current)
        with patch.object(installer, 'trusted_bytes', return_value=b'current'):
            with self.assertRaisesRegex(RuntimeError, 'concurrently'):
                installer.check_update({'allow_restart': True, 'expected_revision': '0' * 64}, current, current)
            installer.check_update({'allow_restart': True, 'expected_revision': hashlib.sha256(b'current').hexdigest()}, current, current)

    def test_network_apply_cannot_change_credentials_or_other_settings(self):
        original = {'name': 'fri', 'resources': [{'name': 'secret', 'credentials': [{'source': '/safe', 'target': 'key'}], 'endpoints': []}]}
        candidate = copy.deepcopy(original)
        candidate['resources'][0]['endpoints'].append({'host': 'example.com', 'port': 443})
        self.assertEqual(host.without_network(original), host.without_network(candidate))
        candidate['resources'][0]['credentials'][0]['source'] = '/different'
        self.assertNotEqual(host.without_network(original), host.without_network(candidate))
        self.assertEqual(m.non_network(original), host.without_network(original))


if __name__ == '__main__':
    unittest.main()
