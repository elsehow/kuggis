"""Chat selection, legacy continuity, and concurrent reconnect behavior."""
import subprocess
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from test_runtime import g, h


class Chats(unittest.TestCase):
    def test_archive_preserves_binding_and_history_and_can_be_restored(self):
        for configured in (False, True):
            with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
                home = Path(directory)
                config = home / 'agents.json'
                config.write_text('{"agents":{"codex":{}}}' if configured else '{}')
                bindings = home / '.kuggis/chat-bindings'
                bindings.mkdir(parents=True)
                binding = bindings / 'research.json'
                if configured:
                    binding.write_text('{"version":1,"agent":"codex"}')
                history = home / '.kuggis/chats/research/session.json'
                history.parent.mkdir(parents=True)
                history.write_text('history to retain')
                with patch.object(g, 'AGENTS', config), patch.object(g.subprocess, 'run') as run:
                    for _ in range(2):
                        g.archive_chat('research')
                    self.assertEqual(run.call_args.args[0], ['/usr/bin/sudo', '-n', '/usr/bin/systemctl', 'stop', 'kuggis-chat@research.service'])
                    self.assertNotIn('research', [c['name'] for c in g.list_chats()['chats']])
                    if configured:
                        self.assertEqual(binding.read_text(), '{"version":1,"agent":"codex"}')
                    else:
                        self.assertFalse(binding.exists())
                    self.assertEqual(history.read_text(), 'history to retain')
                    g.restore_chat('research')
                    self.assertFalse((home / '.kuggis/archived-chats/research').exists())
                    restored = next(c for c in g.list_chats()['chats'] if c['name'] == 'research')
                    self.assertEqual(restored['agent'], 'codex' if configured else None)

    def test_archive_unknown_invalid_and_stop_failure_leave_no_marker(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)), \
             patch.object(g, 'AGENTS', Path(directory) / 'absent'), \
             patch.object(g.subprocess, 'run') as run:
            for name in ('missing', '../escape'):
                with self.assertRaises(ValueError):
                    g.archive_chat(name)
            run.assert_not_called()
            run.side_effect = subprocess.CalledProcessError(1, 'systemctl')
            with self.assertRaises(subprocess.CalledProcessError):
                g.archive_chat('default')
            self.assertEqual(run.call_args.args[0][-1], 'kuggis-terminal.service')
            self.assertFalse((Path(directory) / '.kuggis/archived-chats').exists())

    def test_host_archive_validates_and_delegates_without_attaching(self):
        with patch.object(h.sys, 'platform', 'linux'), patch.object(h.os, 'geteuid', return_value=0), \
             patch.object(h, 'load', return_value={'name':'demo'}), patch.object(h, 'guard'), \
             patch.object(h, 'guest') as guest, patch.object(h.os, 'execv') as execv:
            for rest in ([], ['--chat', '../bad'], ['--chat', 'vt', '--agent', 'pi']):
                with self.assertRaises(ValueError):
                    h.dispatch(['archive', 'demo', *rest])
            guest.assert_not_called()
            h.dispatch(['archive', 'demo', '--chat', 'vt'])
            guest.assert_called_once_with('/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'archive-chat', 'vt')
            execv.assert_not_called()

    def test_history_is_selected_by_recipe_and_default_is_unchanged(self):
        default = ['pi', '--session-dir', '/home/agent/.kuggis/chat/pi', '--continue']
        recipe = ['pi', '--session-dir', '/home/agent/.kuggis/chats/{chat}', '--continue']
        self.assertEqual(g.launch_argv(default, None, 'default'), default)
        self.assertEqual(g.launch_argv(default, recipe, 'default'), default)
        research = g.launch_argv(default, recipe, 'research')
        writing = g.launch_argv(default, recipe, 'writing')
        self.assertIn('/home/agent/.kuggis/chats/research', research)
        self.assertNotEqual(research, writing)
        self.assertNotIn(default[2], research)
        for bad in [None, [], ['pi','--continue'], ['{chat}'], ['pi','{chat}\0']]:
            with self.assertRaises(ValueError):
                g.launch_argv(default, bad, 'research')

    def test_both_sides_reject_names_that_are_paths_or_service_arguments(self):
        for name in ['../escape','x/y','-x','a;id','a b','UPPER','a'*49,'end-','']:
            with self.assertRaises(ValueError):
                g.chat_name(name)
            with self.assertRaises(ValueError):
                h.connect_chat(['--chat', name])
        with self.assertRaisesRegex(ValueError, 'lists chats'):
            h.connect_chat([])
        self.assertEqual(h.connect_chat(['--chat','default']), 'default')
        self.assertEqual(h.connect_chat(['--chat','research']), 'research')

    def test_named_service_has_its_own_socket_and_is_not_started_at_boot(self):
        default = g.terminal_unit('kuggis')
        named = g.terminal_unit('kuggis-chat-%i', ' %i')
        self.assertIn('ExecStop=/usr/bin/tmux -L kuggis-chat-%i kill-server', named)
        self.assertIn('/usr/local/bin/kuggis-launch %i', named)
        self.assertNotIn('[Install]', named)
        self.assertNotIn('kuggis-chat-', default)
        self.assertIn('Restart=no', named)

    def test_live_pane_is_never_respawned_and_dead_pane_is_revived_once(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
            # Serializes the result of two reconnects after an exited agent.
            states = iter([b'1', b'0'])
            calls = []
            def run(argv, **kwargs):
                calls.append(argv)
                return subprocess.CompletedProcess(argv, 0, next(states) if 'display-message' in argv else b'')
            with patch.object(g.subprocess, 'run', side_effect=run):
                g.revive_chat('research')
                g.revive_chat('research')
            respawns = [c for c in calls if 'respawn-pane' in c]
            self.assertEqual(len(respawns), 1)
            self.assertEqual(respawns[0][-2:], ['/usr/local/bin/kuggis-launch','research'])
            self.assertTrue(all(c[2] == 'kuggis-chat-research' for c in calls))

    def test_connect_starts_only_selected_guest_service(self):
        class Attached(Exception): pass
        def attach(*args): raise Attached()
        for name in ['default','research']:
            with patch.object(h.sys, 'platform', 'linux'), patch.object(h.os, 'geteuid', return_value=0), \
                 patch.object(h, 'load', return_value={'name':'demo','chat_launch':['pi','{chat}']}), \
                 patch.object(h, 'guard'), patch.object(h, 'guest') as guest, \
                 patch.object(h.os, 'execv', side_effect=attach) as execv:
                with self.assertRaises(Attached):
                    h.dispatch(['connect','demo','--chat',name])
                calls = guest.call_args_list
                expected = 'kuggis-terminal.service' if name == 'default' else 'kuggis-chat@research.service'
                self.assertEqual(calls[0].args, ('systemctl','start',expected))
                self.assertEqual(calls[0].kwargs, {'user':'root'})
                self.assertEqual(calls[-1].args, ('/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'restore-chat', name))
                argv = execv.call_args.args[1]
                self.assertIn('--uid=agent', argv)
                self.assertIn('kuggis' if name == 'default' else 'kuggis-chat-research', argv)

    def test_missing_recipe_does_not_start_any_terminal(self):
        with patch.object(h.sys, 'platform', 'linux'), patch.object(h.os, 'geteuid', return_value=0), \
             patch.object(h, 'load', return_value={'name':'demo'}), patch.object(h, 'guard'), \
             patch.object(h, 'guest') as guest:
            with self.assertRaisesRegex(RuntimeError, 'chat_launch'):
                h.dispatch(['connect','demo','--chat','research'])
            guest.assert_not_called()
