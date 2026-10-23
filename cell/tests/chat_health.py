import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('chat_health', Path(__file__).parents[1] / 'src/operator/chat_health.py')
health = importlib.util.module_from_spec(spec)
spec.loader.exec_module(health)


class ChatHealth(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / 'state'
        self.state.mkdir()
        self.work = self.root / 'work'
        self.work.mkdir()
        self.unit = self.root / 'cell-chat.service'
        self.command = f'ExecStart=/usr/local/bin/cell chat --workspace "{self.work}" --state "{self.state}"'
        self.props = dict(LoadState='loaded', ActiveState='inactive', SubState='dead', Result='success', ExecMainStatus='0')
        self.calls = []
        self.up = False
        for name, value in [('STATE', self.state), ('UNIT', self.unit)]:
            context = patch.object(health, name, value)
            context.start()
            self.addCleanup(context.stop)
        for name, value in [('service', lambda: self.props), ('responding', lambda: self.up), ('run', self.run_command)]:
            context = patch.object(health, name, value)
            context.start()
            self.addCleanup(context.stop)
        context = patch.object(health.time, 'sleep', lambda _: None)
        context.start()
        self.addCleanup(context.stop)

    def run_command(self, *args):
        self.calls.append(args)
        if args[:2] == ('systemctl', 'cat'):
            if not self.unit.exists():
                self.unit.write_text('[Service]\n' + self.command + '\nRestart=always\n')
            return self.command
        if args[:2] == ('systemctl', 'start'):
            self.up = True
        return ''

    def bind(self, path=None):
        (self.state / 'workspace.json').write_text(json.dumps(str(path or self.work)))

    def test_stopped_service_starts_once_and_duplicate_does_not_interrupt_run(self):
        self.bind()
        self.assertEqual(health.health()['state'], 'stopped')
        self.assertEqual(health.recover('start')['state'], 'ready')
        self.assertEqual(health.recover('start')['state'], 'ready')
        self.assertEqual(self.calls.count(('systemctl', 'start', 'cell-chat.service')), 1)
        self.assertFalse(any('restart' in call or 'stop' in call for call in self.calls))

    def test_crash_loop_does_not_autostart(self):
        self.bind()
        self.props.update(ActiveState='activating', SubState='auto-restart', Result='exit-code', ExecMainStatus='2')
        before = health.recover('start')
        self.assertEqual(before['state'], 'failed')
        self.assertEqual(before['recovery'], 'restart')
        self.assertIn('exit: 2', before['message'])
        self.assertFalse(any(call[1] in ('start', 'reset-failed') for call in self.calls))

    def test_workspace_repair_preserves_state_and_only_changes_chat_unit(self):
        self.bind()
        transcript = self.state / 'conversation.jsonl'
        transcript.write_bytes(b'preserve every byte\n')
        original = {p.name: p.read_bytes() for p in self.state.iterdir()}
        self.command = self.command.replace(str(self.work), str(self.work / 'new-repository'))
        self.props.update(ActiveState='failed', Result='exit-code', ExecMainStatus='2')
        self.assertEqual(health.health()['recovery'], 'repair')
        self.assertEqual(health.recover('start')['recovery'], 'repair')
        before_unit = self.unit.read_text()
        self.assertEqual(health.recover('repair')['state'], 'ready')
        self.assertIn(f'--workspace "{self.work}"', self.unit.read_text())
        self.assertEqual(self.unit.with_suffix('.service.before-workspace-repair').read_text(), before_unit)
        self.assertIn('Restart=always', self.unit.read_text())
        self.assertEqual(original, {p.name: p.read_bytes() for p in self.state.iterdir()})
        self.assertFalse(any('restart' in call or 'stop' in call for call in self.calls))

    def test_missing_workspace_requires_manual_restoration(self):
        self.bind(self.work / 'missing')
        self.assertIsNone(health.recover('repair')['recovery'])
        self.assertFalse(self.unit.with_suffix('.service.before-workspace-repair').exists())

    def test_active_service_without_socket_is_not_restarted(self):
        self.bind()
        self.props.update(ActiveState='active', SubState='running')
        self.assertEqual(health.recover('restart')['state'], 'starting')
        self.assertEqual(self.calls, [])

    def test_custom_startup_command_is_not_overwritten(self):
        self.bind()
        self.command = 'ExecStart=/custom/chat-server'
        result = health.recover('repair')
        self.assertIsNone(result['recovery'])
        self.assertIn('custom startup', result['message'])
        self.assertFalse(self.unit.with_suffix('.service.before-workspace-repair').exists())

    def test_readonly_health_does_not_mutate_crashed_service(self):
        self.bind()
        self.props.update(ActiveState='failed', Result='start-limit-hit')
        self.assertEqual(health.health()['recovery'], 'restart')
        self.assertTrue(all(call[:2] == ('systemctl', 'cat') for call in self.calls))

    def test_inactive_service_with_failure_history_requires_explicit_retry(self):
        self.bind()
        self.props.update(Result='exit-code', ExecMainStatus='2')
        self.assertEqual(health.recover('start')['recovery'], 'restart')
        self.assertFalse(any(call[1] == 'start' for call in self.calls))

    def test_new_cell_can_start_without_saved_binding(self):
        self.assertEqual(health.recover('start')['state'], 'ready')

    def test_workspace_systemd_expansion_is_literal(self):
        workspace = self.work / 'dollar$percent%quote"'
        workspace.mkdir()
        self.bind(workspace)
        original, command = health.workspace_repair()
        self.assertIn('dollar$$percent%%quote\\"', command)
        self.command = next(line for line in command.splitlines() if line.startswith('ExecStart='))
        self.assertIsNone(health.workspace_repair())

    def test_custom_execstart_override_is_not_repaired(self):
        self.bind()
        self.command = '# /etc/systemd/system/cell-chat.service.d/custom.conf\n' + self.command.replace(str(self.work), str(self.work / 'other'))
        self.assertIsNone(health.recover('repair')['recovery'])
        self.assertFalse(self.unit.with_suffix('.service.before-workspace-repair').exists())


if __name__ == '__main__':
    unittest.main()
