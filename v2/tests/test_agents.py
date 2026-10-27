"""Saved bindings are explicit, stable, and scoped to the guest."""
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch
from test_runtime import g, h, ROOT


def configuration(default='pi'):
    return {'default_agent':default, 'agents':{
        agent:{'launch':['/bin/sh'], 'chat_launch':['/bin/sh','{chat}']}
        for agent in ('pi','codex')}}


class Agents(unittest.TestCase):
    def test_legacy_adoption_requires_one_exact_match(self):
        pair = {'launch':['pi','--continue'], 'chat_launch':['pi','{chat}']}
        self.assertEqual(g.legacy_agent({'pi':pair}, pair['launch'], pair['chat_launch']), 'pi')
        for recipes in ({}, {'pi':pair,'copy':pair}, {'pi':{'launch':['pi'],'chat_launch':['pi','{chat}']}}):
            with self.assertRaisesRegex(ValueError, 'matching'):
                g.legacy_agent(recipes, pair['launch'], pair['chat_launch'])

    def test_saved_choice_survives_default_change_and_rejects_switch(self):
        saved = {'version':1,'agent':'pi'}
        self.assertEqual(g.choose_agent(configuration('codex'), saved, None), 'pi')
        self.assertEqual(g.choose_agent(configuration('codex'), None, None), 'codex')
        self.assertEqual(g.choose_agent(configuration(), None, 'codex'), 'codex')
        with self.assertRaisesRegex(ValueError, 'explicitly migrate'):
            g.choose_agent(configuration(), saved, 'codex')
        for saved in ({'version':2,'agent':'pi'}, {'version':1,'agent':'missing'}):
            with self.assertRaises(ValueError):
                g.choose_agent(configuration(), saved, None)

    def test_concurrent_selection_creates_one_binding_and_no_history(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home/'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'HOME', home), patch.object(g, 'AGENTS', config):
                with ThreadPoolExecutor(max_workers=4) as pool:
                    self.assertEqual(list(pool.map(lambda _:g.select_chat('research','codex'), range(4))), ['codex']*4)
                self.assertEqual(g.select_chat('research'), 'codex')
                with self.assertRaisesRegex(ValueError, 'explicitly migrate'):
                    g.select_chat('research','pi')
                self.assertFalse((home/'.kuggis/chats').exists())
                binding = home/'.kuggis/chat-bindings/research.json'
                self.assertEqual(json.loads(binding.read_text()), {'version':1,'agent':'codex'})
                self.assertEqual(binding.stat().st_mode & 0o777, 0o600)

    def test_missing_executable_does_not_claim_chat(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home/'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'HOME', home), patch.object(g, 'AGENTS', config), patch.object(g.shutil, 'which', return_value=None):
                with self.assertRaisesRegex(ValueError, 'not installed'):
                    g.select_chat('research', 'codex')
            self.assertFalse((home/'.kuggis/chat-bindings/research.json').exists())

    def test_agent_option_parses_in_either_order_without_extra_arguments(self):
        with self.assertRaisesRegex(ValueError, 'requires --chat'):
            h.connect_options(['--agent','codex'])
        for args in (['--chat','vt','--agent','codex'], ['--agent','codex','--chat','vt']):
            self.assertEqual(h.connect_options(args), ('vt','codex'))
        for args in (['--agent'], ['--agent','../bad'], ['--agent','pi','--agent','codex'], ['--chat','vt','--bad','x']):
            with self.assertRaises((ValueError, RuntimeError)):
                h.connect_options(args)

    def test_listing_is_read_only_and_reports_saved_agents(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home/'agents.json'
            config.write_text(json.dumps(configuration()))
            with patch.object(g, 'HOME', home), patch.object(g, 'AGENTS', config), \
                 patch.object(g.subprocess, 'run') as run:
                self.assertEqual(g.list_chats()['chats'], [])
                run.assert_not_called()
                self.assertFalse((home/'.kuggis').exists())
                bindings = home/'.kuggis/chat-bindings'
                bindings.mkdir(parents=True)
                for name, agent in [('default','pi'),('vt','codex')]:
                    (bindings/(name+'.json')).write_text(json.dumps({'version':1,'agent':agent}))
                before = {p:p.read_bytes() for p in bindings.iterdir()}
                run.side_effect = [subprocess.CompletedProcess([],0,b'0\n'),subprocess.CompletedProcess([],1,b'')]
                self.assertEqual(g.list_chats()['chats'], [
                    {'name':'default','agent':'pi','running':True}, {'name':'vt','agent':'codex','running':False}])
                self.assertEqual(before, {p:p.read_bytes() for p in bindings.iterdir()})
                self.assertTrue(all('list-panes' in call.args[0] for call in run.call_args_list))

    def test_bare_host_connect_only_reads_the_guest_index(self):
        for verb in ('connect','chats'):
            with patch.object(h.sys, 'platform', 'linux'), patch.object(h.os, 'geteuid', return_value=0), \
                 patch.object(h, 'load', return_value={'name':'demo'}), patch.object(h, 'guard'), \
                 patch.object(h, 'guest', return_value=subprocess.CompletedProcess([],0,b'{}')) as guest, \
                 patch.object(h.sys, 'stdout') as output, patch.object(h.os, 'execv') as execv:
                h.dispatch([verb,'demo'])
                guest.assert_called_once_with('/usr/bin/python3','/run/kuggis-runtime/guest.py','list-chats')
                output.buffer.write.assert_called_once_with(b'{}')
                execv.assert_not_called()


class CodexRecipe(unittest.TestCase):
    def test_native_sessions_are_isolated_and_model_changes_preserved(self):
        spec = importlib.util.spec_from_file_location('codex_launcher', ROOT/'recipes/codex/launch.py')
        launcher = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(launcher)
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            shared = home/'.local/share/kuggis/codex'
            shared.mkdir(parents=True)
            (shared/'config.toml').write_text('model = "gpt-6.1-sol"\n')
            default = launcher.prepare(home, 'default', home/'workspace')
            vt = launcher.prepare(home, 'vt', home/'workspace')
            self.assertNotEqual(default, vt)
            self.assertEqual(len(launcher.command(home, vt)), 1)
            self.assertIn('[projects.'+json.dumps(str(home/'workspace'))+']', (vt/'config.toml').read_text())
            self.assertIn('trust_level = "trusted"', (vt/'config.toml').read_text())
            (vt/'sessions').mkdir()
            (vt/'sessions/rollout-test.jsonl').write_text('{}\n')
            self.assertEqual(launcher.command(home, vt)[1:], ['resume','--last','--all','--include-non-interactive'])
            self.assertEqual(len(launcher.command(home, default)), 1)
            (vt/'config.toml').write_text('model = "gpt-6.1-terra"\n')
            launcher.prepare(home, 'vt', home/'workspace')
            self.assertIn('terra', (vt/'config.toml').read_text())
