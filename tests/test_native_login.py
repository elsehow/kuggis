"""Native-only agent login: no exe model gateway; Claude Code as a built-in agent."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import uuid
from test_runtime import ROOT, module

sys.path.insert(0, str(ROOT / 'crates/kuggis/runtime'))
manage = module('manage')
login = module('login')


def launcher(agent):
    spec = importlib.util.spec_from_file_location(agent + '_launcher', ROOT / 'recipes' / agent / 'launch.py')
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class FakeManager:
    def __init__(self, workspace):
        self.workspace, self.applied, self.probed = workspace, [], []

    def snapshot(self):
        return {'workspace': copy.deepcopy(self.workspace)}

    def apply_network(self, snapshot, candidate):
        self.applied.append(candidate)
        self.workspace = candidate

    def remote(self, action, **fields):
        self.probed.append(fields['host'])
        return {'reachable': True}


class GatewayRemoval(unittest.TestCase):
    def test_upgrade_drops_only_the_gateway_grant(self):
        github = {'name': 'github', 'endpoints': [{'host': 'github.com', 'port': 443}]}
        workspace = {'name': 'demo', 'resources': [github, {'name': 'exe-models', 'endpoints': [
            {'host': 'llm.int.exe.xyz', 'port': 443, 'destination': {'kind': 'pinned', 'addresses': ['169.254.169.254']}}],
            'credentials': [{'source': '/etc/kuggis/recipes/pi-models.json', 'target': '.pi/agent/models.json'}]}]}
        original = copy.deepcopy(workspace)
        candidate, removed = manage.without_gateway(workspace)
        self.assertEqual(removed, ['exe-models'])
        self.assertEqual(candidate['resources'], [github])
        self.assertEqual(workspace, original)
        self.assertEqual(manage.without_gateway(candidate), (candidate, []))

    def test_recipes_and_creation_no_longer_reference_the_gateway(self):
        sources = [path for agent in ('codex', 'claude') for path in (ROOT / 'recipes' / agent).iterdir()
                   if path.is_file()]
        for path in [*sources, ROOT / 'crates/kuggis/runtime/onboard.py', ROOT / 'crates/kuggis/src/onboarding.rs']:
            text = path.read_text()
            for retired in ('llm.int.exe.xyz', 'pi-models', "'integrations','attach'"):
                self.assertNotIn(retired, text, path)

    def test_codex_never_selects_the_retired_gateway_provider(self):
        codex = launcher('codex')
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            shared = home / '.local/share/kuggis/codex'
            shared.mkdir(parents=True)
            (shared / 'config.toml').write_bytes((ROOT / 'recipes/codex/config.toml').read_bytes())
            state = codex.prepare(home, 'vt', home / 'workspace')
            self.assertIn('model_provider="openai"', codex.command(home, state))
            # A cell created before native-only login still has the old shared config.
            (home / '.codex/config.toml').write_text('model = "gpt-6.1-sol"\nmodel_provider = "exe"\n')
            self.assertIn('model_provider="openai"', codex.command(home, state))


class ClaudeRecipe(unittest.TestCase):
    def test_each_chat_resumes_only_its_own_bound_session(self):
        claude = launcher('claude')
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            vt, other = claude.prepare(home, 'vt'), claude.prepare(home, 'other')
            first = claude.command(home, vt)
            self.assertEqual(first[-2], '--session-id')
            self.assertNotEqual(first[-1], claude.command(home, other)[-1])
            self.assertEqual(claude.command(home, claude.prepare(home, 'vt')), first)
            self.assertIn('--dangerously-skip-permissions', first)
            self.assertNotIn('--continue', first)
            # Only a saved transcript can be resumed.
            projects = home / '.claude/projects/-home-agent-workspace'
            projects.mkdir(parents=True)
            (projects / (first[-1] + '.jsonl')).write_text('{}\n')
            self.assertEqual(claude.command(home, vt)[-2:], ['--resume', first[-1]])
            session_id = str(uuid.uuid4())
            claude.bind(home, 'vt', {'hook_event_name': 'SessionStart', 'session_id': session_id})
            self.assertEqual(claude.command(home, vt)[-2:], ['--session-id', session_id])
            self.assertEqual(claude.command(home, other)[-2], '--session-id')
            hook = json.loads((vt / 'settings.json').read_text())['hooks']['SessionStart'][0]['hooks'][0]
            self.assertTrue(hook['command'].endswith('kuggis-claude --bind vt'))
            for payload in ({'hook_event_name': 'Stop', 'session_id': session_id},
                            {'hook_event_name': 'SessionStart', 'session_id': '../x'}):
                with self.assertRaises(ValueError):
                    claude.bind(home, 'vt', payload)
            for name in ('../bad', 'Bad', 'bad-'):
                with self.assertRaises(ValueError):
                    claude.prepare(home, name)

    def test_release_is_an_exact_checksum_pinned_binary(self):
        release = json.loads((ROOT / 'recipes/claude/release.json').read_text())
        spec = release['archives']['x86_64']
        self.assertRegex(release['version'], r'^\d+\.\d+\.\d+$')
        self.assertEqual(spec['url'], 'https://downloads.claude.ai/claude-code-releases/'
                         + release['version'] + '/linux-x64/claude')
        self.assertRegex(spec['sha256'], r'^[0-9a-f]{64}$')

    def test_login_grants_only_claude_endpoints_and_requires_the_recipe(self):
        workspace = {'name': 'demo', 'agents': {'codex': {}, 'claude': {}}, 'resources': [
            {'name': 'github', 'endpoints': [{'host': 'github.com', 'port': 443}], 'credentials': []}]}
        manager = FakeManager(workspace)
        plan = login.login_network(manager, plan=True, agent='claude')
        self.assertEqual(plan['network_grants'], ['api.anthropic.com:443', 'claude.ai:443', 'platform.claude.com:443'])
        self.assertEqual((plan['agent'], plan['restart'], manager.applied), ('claude', 'proxy', []))
        self.assertEqual(login.login_network(manager, agent='claude'), list(login.CLAUDE_LOGIN_HOSTS))
        resource = manager.workspace['resources'][-1]
        self.assertEqual(resource['name'], 'claude-login')
        self.assertEqual([e['host'] for e in resource['endpoints']], list(login.CLAUDE_LOGIN_HOSTS))
        self.assertFalse(any('openai' in e['host'] or 'chatgpt' in e['host']
                             for r in manager.workspace['resources'] for e in r['endpoints']))
        self.assertEqual(login.login_network(manager, agent='claude'), [])
        with self.assertRaisesRegex(login.Failure, 'Claude Code is not configured'):
            login.login_network(FakeManager({'name': 'old', 'agents': {'codex': {}}}), agent='claude')


class UpgradeAgents(unittest.TestCase):
    BUILTIN = {'codex': {'launch': ['kuggis-codex', 'default'], 'chat_launch': ['kuggis-codex', '{chat}']},
               'claude': {'launch': ['kuggis-claude', 'default'], 'chat_launch': ['kuggis-claude', '{chat}']}}

    def manager(self, root, recipes):
        m = manage.Management.__new__(manage.Management)
        m.config = root / 'workspace.json'
        m.p = {'recipes': recipes}
        m.calls = []
        def cli(*words, timeout=120, allow_failure=False):
            m.calls.append(tuple(map(str, words)))
            if getattr(m, 'fail_on', None) and any(m.fail_on in word for word in map(str, words)):
                raise manage.Failure('guest refused')
            return b'{}'
        m.cli = cli
        return m

    def test_registration_adds_missing_builtins_without_touching_existing_choices(self):
        custom = {'launch': ['custom-codex'], 'chat_launch': ['custom-codex', '{chat}']}
        workspace = {'name': 'demo', 'default_agent': 'pi', 'agents': {'pi': {'launch': ['pi']}, 'codex': custom}}
        candidate, added = manage.with_builtin_agents(workspace, self.BUILTIN)
        self.assertEqual(added, ['claude'])
        self.assertEqual(candidate['agents']['codex'], custom)
        self.assertEqual((candidate['default_agent'], sorted(candidate['agents'])), ('pi', ['claude', 'codex', 'pi']))
        self.assertNotIn('claude', workspace['agents'])
        # A legacy launch-only cell is never silently converted to per-chat agents.
        legacy = {'name': 'old', 'launch': ['pi', '--continue']}
        self.assertEqual(manage.with_builtin_agents(legacy, self.BUILTIN), (legacy, []))

    def test_plan_installs_only_when_release_or_launcher_differs(self):
        recipes = {'claude': {'release.json': json.dumps({'version': '2.0.0'}), 'launch.py': 'launcher'}}
        launcher = manage.hashlib.sha256(b'launcher').hexdigest()
        for installed, action in [({}, 'install'), ({'claude': {'version': '1.0.0', 'launcher': launcher}}, 'install'),
                                  ({'claude': {'version': '2.0.0', 'launcher': 'stale'}}, 'install'),
                                  ({'claude': {'version': '2.0.0', 'launcher': launcher}}, 'none')]:
            plan = manage.agent_plan(recipes, installed)['claude']
            self.assertEqual((plan['action'], plan['pinned']), (action, '2.0.0'), installed)
        self.assertEqual(manage.agent_plan(recipes, {'claude': {'version': '1.0.0', 'launcher': None}})['claude']['installed'], '1.0.0')

    def test_install_stages_runs_the_recipe_as_guest_and_always_removes_the_staging_link(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            m = self.manager(root, {'claude': {'install.py': 'i', 'release.json': '{}', 'launch.py': 'l'}})
            package = root / 'claude.pkg'
            package.write_bytes(b'binary')
            m.install_agents({'claude': package})
            verbs = [(call[0], call[-1] if call[0] == 'exec' else call[3]) for call in m.calls]
            self.assertEqual(verbs[0], ('exec', 'claude'))
            self.assertEqual([target for verb, target in verbs if verb == 'put'], [
                '.kuggis-setup/claude/install.py', '.kuggis-setup/claude/release.json',
                '.kuggis-setup/claude/launch.py', '.kuggis-setup/claude/agent.pkg'])
            self.assertEqual(m.calls[-2][-2:], (manage.AGENT_STAGE + '/claude/install.py', manage.AGENT_STAGE + '/claude/agent.pkg'))
            self.assertIn(manage.AGENT_CLEAN, m.calls[-1])
            # Uploads never run as host root: every guest program goes through exec.
            self.assertTrue(all(call[0] in ('exec', 'put') for call in m.calls))
            m.calls.clear()
            m.fail_on = 'install.py'
            with self.assertRaisesRegex(manage.Failure, 'guest refused'):
                m.install_agents({'claude': package})
            self.assertIn(manage.AGENT_CLEAN, m.calls[-1])

    def test_download_reuses_a_verified_package_and_rejects_a_wrong_checksum(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            m = self.manager(root, {})
            good = manage.hashlib.sha256(b'release').hexdigest()
            files = {'release.json': json.dumps({'version': '1.0.0', 'archives': {'x86_64': {'url': 'https://example.invalid/x', 'sha256': good}}})}
            (root / 'claude.pkg').write_bytes(b'release')
            with patch.object(manage.subprocess, 'run', side_effect=AssertionError('must not download')):
                self.assertEqual(m.download_agent('claude', files), root / 'claude.pkg')
            (root / 'claude.pkg').write_bytes(b'tampered')
            def fake_curl(argv, **kwargs):
                Path(argv[-1]).write_bytes(b'not the release')
                return manage.subprocess.CompletedProcess(argv, 0, b'', b'')
            with patch.object(manage.subprocess, 'run', side_effect=fake_curl):
                with self.assertRaisesRegex(manage.Failure, 'checksum'):
                    m.download_agent('claude', files)
            self.assertFalse((root / 'claude.pkg.partial').exists())

    def test_staging_scripts_link_private_state_and_refuse_a_project_path(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory) / 'agent'
            (home / 'workspace').mkdir(parents=True)
            stage = home / '.local/share/kuggis/setup/agents'
            run = lambda script, *args: subprocess.run(
                [sys.executable, '-c', script.replace('/home/agent', str(home)), *args], capture_output=True)
            self.assertEqual(run(manage.AGENT_PREPARE, str(stage), 'claude').returncode, 0)
            self.assertEqual((home / 'workspace/.kuggis-setup').resolve(), stage.resolve())
            self.assertTrue((stage / 'claude').is_dir())
            self.assertEqual(run(manage.AGENT_CLEAN, str(stage)).returncode, 0)
            self.assertFalse((home / 'workspace/.kuggis-setup').exists())
            (home / 'workspace/.kuggis-setup').mkdir()
            self.assertNotEqual(run(manage.AGENT_PREPARE, str(stage), 'claude').returncode, 0)
            self.assertEqual(run(manage.AGENT_CLEAN, str(stage)).returncode, 0)
            self.assertTrue((home / 'workspace/.kuggis-setup').is_dir())

    def test_probe_reports_the_linked_release_and_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            release = home / '.local/lib/claude/2.0.0'
            release.mkdir(parents=True)
            (release / 'claude').write_text('binary')
            (home / '.local/bin').mkdir()
            (home / '.local/bin/claude').symlink_to(release / 'claude')
            (home / '.local/bin/kuggis-claude').write_text('launcher')
            result = subprocess.run([sys.executable, '-c', manage.AGENT_PROBE, 'claude', 'codex'],
                                    env={**os.environ, 'HOME': str(home)}, capture_output=True, check=True)
            self.assertEqual(json.loads(result.stdout), {
                'claude': {'version': '2.0.0', 'launcher': manage.hashlib.sha256(b'launcher').hexdigest()},
                'codex': {'version': None, 'launcher': None}})


class ReleasePins(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('release_check', ROOT / 'scripts/check-agent-releases.py')
        self.check = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.check)
        self.upstream = {
            'https://downloads.claude.ai/claude-code-releases/latest': '2.0.1\n',
            'https://downloads.claude.ai/claude-code-releases/2.0.1/manifest.json': json.dumps(
                {'platforms': {'linux-x64': {'checksum': 'c' * 64}}}),
            'https://api.github.com/repos/openai/codex/releases/latest': json.dumps({'tag_name': 'rust-v0.2.0', 'assets': [
                {'name': 'codex.tar.gz', 'digest': 'sha256:' + 'd' * 64,
                 'browser_download_url': 'https://github.com/openai/codex/releases/download/rust-v0.2.0/codex.tar.gz'}]})}

    def recipes(self, root, claude='2.0.1', codex='0.1.0'):
        for agent, release in {
                'claude': {'version': claude, 'archives': {'x86_64': {
                    'url': 'https://downloads.claude.ai/claude-code-releases/' + claude + '/linux-x64/claude', 'sha256': 'a' * 64}}},
                'codex': {'version': codex, 'archives': {'x86_64': {
                    'url': 'https://github.com/openai/codex/releases/download/rust-v' + codex + '/codex.tar.gz', 'sha256': 'b' * 64}}}}.items():
            (root / 'recipes' / agent).mkdir(parents=True)
            (root / 'recipes' / agent / 'release.json').write_text(json.dumps(release))

    def test_reports_stale_pins_and_updates_only_on_request(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.recipes(root)
            report = self.check.check(root, self.upstream.__getitem__)
            self.assertEqual(report['claude'], {'pinned': '2.0.1', 'latest': '2.0.1', 'stale': False})
            self.assertEqual(report['codex'], {'pinned': '0.1.0', 'latest': '0.2.0', 'stale': True})
            pin = root / 'recipes/codex/release.json'
            self.assertEqual(json.loads(pin.read_text())['version'], '0.1.0')
            self.assertEqual(list(self.check.check(root, self.upstream.__getitem__, only=('claude',))), ['claude'])
            self.assertTrue(self.check.check(root, self.upstream.__getitem__, update=True)['codex']['updated'])
            self.assertEqual(json.loads(pin.read_text()), {'version': '0.2.0', 'archives': {'x86_64': {
                'url': 'https://github.com/openai/codex/releases/download/rust-v0.2.0/codex.tar.gz', 'sha256': 'd' * 64}}})
            self.assertFalse(self.check.check(root, self.upstream.__getitem__)['codex']['stale'])

    def test_unreachable_or_unverifiable_upstream_is_an_error_not_current(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.recipes(root)
            del self.upstream['https://downloads.claude.ai/claude-code-releases/latest']
            release = json.loads(self.upstream['https://api.github.com/repos/openai/codex/releases/latest'])
            del release['assets'][0]['digest']
            self.upstream['https://api.github.com/repos/openai/codex/releases/latest'] = json.dumps(release)
            report = self.check.check(root, self.upstream.__getitem__, update=True)
            self.assertIn('error', report['claude'])
            self.assertIn('SHA-256', report['codex']['error'])
            self.assertEqual(json.loads((root / 'recipes/codex/release.json').read_text())['version'], '0.1.0')


if __name__ == '__main__':
    unittest.main()
