"""Chat selection, legacy continuity, and concurrent reconnect behavior."""
import subprocess
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from test_runtime import g, h


class Chats(unittest.TestCase):
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
