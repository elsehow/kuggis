"""Chat selection, legacy continuity, and concurrent reconnect behavior."""
import subprocess
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from test_runtime import g, h


class Chats(unittest.TestCase):
    def test_old_markers_and_interrupted_archives_release_the_name(self):
        from test_agents import configuration
        for interrupted in (False, True):
            with self.subTest(interrupted=interrupted), tempfile.TemporaryDirectory() as directory, \
                 patch.object(g, 'HOME', Path(directory)):
                home = Path(directory)
                config = home / 'agents.json'
                config.write_text(json.dumps(configuration()))
                with patch.object(g, 'AGENTS', config), patch.object(g.shutil, 'which', return_value='/bin/sh'), \
                     patch.object(g.subprocess, 'run'):
                    g.select_chat('research', 'codex')
                    if interrupted:
                        with patch.object(g, 'finish_archive', side_effect=[None, OSError('interrupted')]):
                            with self.assertRaises(OSError):
                                g.archive_chat('research')
                    else:
                        marker = home / '.kuggis/archived-chats/research'
                        marker.parent.mkdir(parents=True)
                        marker.touch()
                    self.assertEqual(g.select_chat('research', 'pi'), 'pi')
                    self.assertNotEqual(g.state_name('research'), 'research')
                    self.assertEqual(g.list_chats()['archives'][0]['agent'], 'codex')
                    self.assertFalse((home / '.kuggis/archived-chats/research').exists())

    def test_default_name_gets_fresh_history_and_archive_ids_do_not_collide(self):
        from test_agents import configuration
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
            home = Path(directory)
            config = home / 'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'AGENTS', config), patch.object(g.shutil, 'which', return_value='/bin/sh'), \
                 patch.object(g.subprocess, 'run'), patch.object(g.time, 'time_ns', return_value=123):
                g.select_chat('default', 'codex')
                self.assertEqual(g.archive_chat('default'), 'default@123')
                g.select_chat('default', 'codex')
                self.assertNotEqual(g.state_name('default'), 'default')
                self.assertEqual(g.archive_chat('default'), 'default@124')

    def test_launch_uses_replacement_identity_for_recipe_history(self):
        from test_agents import configuration
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
            home = Path(directory)
            config = home / 'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'AGENTS', config), patch.object(g.shutil, 'which', return_value='/bin/sh'), \
                 patch.object(g.subprocess, 'run'), patch.object(g.os, 'chdir'), patch.object(g.os, 'execvpe') as execute:
                g.select_chat('research', 'codex')
                g.archive_chat('research')
                g.select_chat('research', 'codex')
                identity = g.state_name('research')
                g.launch('research')
                self.assertEqual(execute.call_args.args[1], ['/bin/sh', identity])
                self.assertEqual(execute.call_args.args[2]['KUGGIS_CHAT'], 'research')

    def test_legacy_replacements_do_not_expose_internal_history_ids_as_chat_names(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
            home = Path(directory)
            config = home / 'agents.json'
            config.write_text('{}')
            history = home / '.kuggis/chats/research'
            history.mkdir(parents=True)
            recipe = home / 'chat-launch.json'
            recipe.write_text('["pi", "--session-dir", "{chat}"]')
            original_path = g.Path
            def path(value):
                return recipe if value == '/etc/kuggis/chat-launch.json' else original_path(value)
            with patch.object(g, 'AGENTS', config), patch.object(g.subprocess, 'run'), patch.object(g, 'Path', side_effect=path):
                g.archive_chat('research')
                g.prepare_legacy_chat('research')
                identity = g.state_name('research')
                self.assertNotEqual(identity, 'research')
                (history.parent / identity).mkdir()
                self.assertEqual(g.chat_names({}), {'research', 'default'})
                g.archive_chat('research')
                self.assertEqual(g.chat_names({}), {'default'})

    def test_archived_chat_releases_name_and_preserves_original_history(self):
        import json
        from test_agents import configuration
        with tempfile.TemporaryDirectory() as directory, patch.object(g, 'HOME', Path(directory)):
            home = Path(directory)
            config = home / 'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'AGENTS', config), patch.object(g.shutil, 'which', return_value='/bin/sh'), \
                 patch.object(g.subprocess, 'run'):
                g.select_chat('research', 'codex')
                binding = home / '.kuggis/chat-bindings/research.json'
                before = binding.read_bytes()
                history = home / '.kuggis/agents/codex/research/kuggis-session.json'
                history.parent.mkdir(parents=True)
                history.write_text('original session')
                first = g.archive_chat('research')
                self.assertFalse(binding.exists())
                record = json.loads((home / '.kuggis/archives' / (first + '.json')).read_text())
                self.assertEqual(record['binding'], json.loads(before))
                self.assertEqual(record['state_name'], 'research')
                self.assertEqual(g.select_chat('research', 'pi'), 'pi')
                identity = g.state_name('research')
                self.assertNotEqual(identity, 'research')
                with self.assertRaisesRegex(ValueError, 'archive it first'):
                    g.select_chat('research', 'codex')
                second = g.archive_chat('research')
                self.assertNotEqual(first, second)
                self.assertEqual(g.select_chat('research', 'codex'), 'codex')
                self.assertNotIn(g.state_name('research'), ('research', identity))
                self.assertEqual(history.read_text(), 'original session')
                self.assertEqual({r['id'] for r in g.list_chats()['archives']}, {first, second})

    def test_guest_failure_preserves_diagnostic_and_handles_empty_stderr(self):
        for stderr, expected in [
            (b'kuggis guest: ValueError: chat belongs to codex, not claude\n',
             'chat belongs to codex, not claude'),
            (b'', 'guest operation failed with exit status 1'),
        ]:
            with self.subTest(stderr=stderr), patch.object(h, 'run', return_value=
                    subprocess.CompletedProcess([], 1, b'', stderr)):
                with self.assertRaisesRegex(RuntimeError, expected):
                    h.guest('/usr/bin/python3', '/run/kuggis-runtime/guest.py',
                            'select-chat', 'research', 'claude')

    def test_archive_preserves_legacy_binding_and_history(self):
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
                    identity = g.archive_chat('research')
                    self.assertEqual(run.call_args.args[0], ['/usr/bin/sudo', '-n', '/usr/bin/systemctl', 'stop', 'kuggis-chat@research.service'])
                    self.assertNotIn('research', [c['name'] for c in g.list_chats()['chats']])
                    self.assertFalse(binding.exists())
                    record = json.loads((home / '.kuggis/archives' / (identity + '.json')).read_text())
                    self.assertEqual(record['state_name'], 'research')
                    self.assertEqual(record['binding'], {'version':1, 'agent':'codex'} if configured else None)
                    self.assertEqual(history.read_text(), 'history to retain')
                    with self.assertRaisesRegex(ValueError, 'unknown chat'):
                        g.archive_chat('research')

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
             patch.object(h, 'guest', return_value=subprocess.CompletedProcess([], 0, b'')) as guest, patch.object(h.os, 'execv') as execv:
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
