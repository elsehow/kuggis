"""The compiled positional CLI enters the native terminal over a fake SSH transport."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from test_runtime import ROOT


class PositionalCLI(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
        self.state = self.root / 'state'
        cell = self.state / 'fri'
        cell.mkdir(parents=True)
        self.config = cell / 'workspace.json'
        self.config.write_text(json.dumps({
            'version': 2, 'name': 'fri', 'ssh': 'fixture', 'launch': ['sh'],
            'agents': {'codex': {'launch': ['sh'], 'chat_launch': ['sh', '{chat}']}},
            'default_agent': 'codex',
        }))
        self.log = self.root / 'ssh.json'
        ssh = self.root / 'ssh'
        ssh.write_text(f'#!{sys.executable}\n' + '''import json, os, shlex, sys
from pathlib import Path
Path(os.environ['SSH_LOG']).write_text(json.dumps(sys.argv[1:]))
if shlex.split(sys.argv[-1])[4] == 'chats':
    print(os.environ.get('SSH_CHATS', json.dumps({'chats': [], 'agents': ['codex'], 'default_agent': 'codex'})))
elif shlex.split(sys.argv[-1])[4] == 'archive' and os.environ.get('SSH_ARCHIVE_ID'):
    print(json.dumps({'archive_id': os.environ['SSH_ARCHIVE_ID']}))
sys.exit(int(os.environ.get('SSH_STATUS', '0')))
''')
        ssh.chmod(0o755)
        self.env = {**os.environ, 'KUGGIS_STATE_DIR': str(self.state), 'SSH_LOG': str(self.log),
                    'PATH': str(self.root) + ':' + os.environ['PATH']}

    def cli(self, *args, status=0):
        result = subprocess.run([self.binary, *args], env=self.env, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        return result

    def test_named_chat_opens_native_terminal_and_preserves_agent_selection(self):
        self.cli('fri', 'research', '--agent', 'codex')
        args = json.loads(self.log.read_text())
        self.assertEqual(args[0], '-tt')
        self.assertTrue(args[-1].endswith("'connect' 'fri' '--chat' 'research' '--agent' 'codex'"))
        self.cli(str(self.config), 'research')
        self.assertTrue(json.loads(self.log.read_text())[-1].endswith("'connect' 'fri' '--chat' 'research'"))

    def test_bare_cell_and_list_only_read_inventory(self):
        for args in [('fri',), ('fri', '--list'), ('fri', '--list', '--json')]:
            result = self.cli(*args)
            invocation = json.loads(self.log.read_text())
            self.assertEqual(invocation[0], '-T')
            self.assertTrue(invocation[-1].endswith("'chats' 'fri'"))
            if '--json' in args:
                self.assertEqual(json.loads(result.stdout)['chats'], [])
            else:
                self.assertIn(b'kuggis fri NAME', result.stdout)

    def test_reserved_cell_name_keeps_usable_open_instructions(self):
        cell = self.state / 'chat'
        cell.mkdir()
        config = json.loads(self.config.read_text())
        config['name'] = 'chat'
        (cell / 'workspace.json').write_text(json.dumps(config))
        result = self.cli('chat', 'list', 'chat')
        self.assertIn(b'kuggis chat open chat NAME', result.stdout)

    def test_exec_preserves_literal_arguments_and_transport_errors_propagate(self):
        self.cli('fri', '--exec', '--', 'printf', '--help', '$(literal)')
        self.assertTrue(json.loads(self.log.read_text())[-1].endswith(
            "'exec' 'fri' '--' 'printf' '--help' '$(literal)'"))
        self.env['SSH_STATUS'] = '7'
        self.cli('fri', 'research', status=2)

    def test_list_cells_does_not_connect(self):
        result = self.cli('--list', '--json')
        self.assertIn(b'fri', result.stdout)
        self.assertFalse(self.log.exists())

    def test_archive_is_nonterminal_and_reports_the_selected_chat(self):
        result = self.cli('fri', 'research', 'archive', '--json')
        self.assertEqual(json.loads(result.stdout), {'ok': True, 'cell': 'fri', 'archived': 'research'})
        invocation = json.loads(self.log.read_text())
        self.assertEqual(invocation[0], '-T')
        self.assertTrue(invocation[-1].endswith("'archive' 'fri' '--chat' 'research'"))

    def test_archive_identity_reaches_operator_and_chat_inventory(self):
        self.env['SSH_ARCHIVE_ID'] = 'research@123'
        result = self.cli('fri', 'research', 'archive', '--json')
        self.assertEqual(json.loads(result.stdout)['archive_id'], 'research@123')
        self.assertIn(b'as research@123', self.cli('fri', 'research', 'archive').stdout)
        self.env['SSH_CHATS'] = json.dumps({
            'chats': [{'name': 'research', 'agent': 'claude', 'running': True}],
            'agents': ['codex', 'claude'], 'default_agent': 'codex',
            'archives': [{'id': 'research@123', 'name': 'research', 'agent': 'codex',
                          'archived_at': 123, 'state_name': 'research'}],
        })
        report = json.loads(self.cli('fri', '--json').stdout)
        self.assertEqual(report['chats'][0]['agent'], 'claude')
        self.assertEqual(report['archives'][0]['id'], 'research@123')
        self.assertIn(b'research@123', self.cli('fri').stdout)
        self.assertEqual(json.loads(self.log.read_text())[0], '-T')

    def test_login_help_and_create_plan_do_not_connect(self):
        result = self.cli('fri', '--login', '--help')
        self.assertIn(b'--agent codex', result.stdout)
        result = self.cli('--create', 'new-cell', '--repo', 'owner/repo', '--plan', '--json')
        self.assertIn(b'new-cell', result.stdout)
        self.assertFalse(self.log.exists())

    def test_unknown_actions_do_not_start_ssh(self):
        for args in [('fri', 'research', 'delete'), ('fri', '--lst'),
                     ('fri', 'research', 'archive', '--agent', 'codex')]:
            self.cli(*args, status=2)
            self.assertFalse(self.log.exists())
