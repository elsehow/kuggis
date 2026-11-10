"""Native-only agent login: no exe model gateway; Claude Code as a built-in agent."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
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


if __name__ == '__main__':
    unittest.main()
