"""Shared login migration, native session identity, callback isolation and CLI."""
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import socket
import select
import signal
import sqlite3
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import urlopen, build_opener, ProxyHandler
import uuid
from test_runtime import ROOT, module
import test_management as management_tests

spec = importlib.util.spec_from_file_location('shared_codex', ROOT/'recipes/codex/launch.py')
recipe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recipe)
login = module('login')
guest = module('login_guest')


class ChatMigration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.template = self.home/'.local/share/kuggis/codex/config.toml'
        self.template.parent.mkdir(parents=True)
        self.template.write_text('model="gpt-6.1-sol"\nmodel_provider="exe"\nmodel_catalog_json="/catalog.json"\n')

    def legacy(self, name, source='vscode'):
        state = recipe.chat_home(self.home, name)
        state.mkdir(parents=True)
        (state/'config.toml').write_text('model="gpt-6.1-terra"\nmodel_provider="exe"\n')
        session = str(uuid.uuid4())
        with closing(sqlite3.connect(state/'state_5.sqlite')) as db, db:
            db.execute('CREATE TABLE threads(id TEXT, source TEXT, updated_at INTEGER, created_at INTEGER, archived INTEGER)')
            db.execute('INSERT INTO threads VALUES (?,?,2,1,0)', (session, source))
            db.execute('INSERT INTO threads VALUES (?,?,100,1,0)', (str(uuid.uuid4()), 'subagent'))
        (state/'thread_history_1.sqlite').write_bytes(b'paginated history remains untouched')
        return state, session

    def test_legacy_databases_history_and_configs_remain_byte_identical(self):
        state, session = self.legacy('research')
        original = {p:p.read_bytes() for p in state.iterdir()}
        recipe.prepare(self.home, 'research')
        self.assertEqual(recipe.command(self.home, state)[-2:], ['resume', session])
        self.assertEqual(original, {p:p.read_bytes() for p in original})
        profile = tomllib.loads((self.home/'.codex/kuggis-research.config.toml').read_text())
        self.assertEqual(profile['model'], 'gpt-6.1-terra')
        self.assertNotIn('model_provider', profile)
        self.assertIn('SessionStart', profile['hooks'])

    def test_chats_do_not_resume_each_other_and_new_chats_inherit_login(self):
        one, first = self.legacy('one')
        two, second = self.legacy('two')
        for name in ('one', 'two', 'new'):
            recipe.prepare(self.home, name)
        common = self.home/'.codex'
        (common/'auth.json').write_text('{"private":"shared-native-credentials"}')
        recipe.activate(self.home)
        self.assertEqual(recipe.command(self.home, one)[-1], first)
        self.assertEqual(recipe.command(self.home, two)[-1], second)
        self.assertNotIn('resume', recipe.command(self.home, recipe.chat_home(self.home, 'new')))
        for state in (one, two):
            self.assertIn('model_provider="openai"', recipe.command(self.home, state))
            self.assertFalse((state/'auth.json').exists())
        self.assertEqual((common/'auth.json').read_text(), '{"private":"shared-native-credentials"}')
        self.assertNotIn('model_catalog_json', tomllib.loads((common/'config.toml').read_text()))

    def test_native_start_hook_tracks_new_sessions_without_scanning_other_chats(self):
        state, old = self.legacy('one')
        recipe.prepare(self.home, 'one')
        new = str(uuid.uuid4())
        recipe.bind(self.home, 'one', {'hook_event_name':'SessionStart', 'session_id':new, 'source':'clear'})
        self.assertEqual(recipe.command(self.home, state)[-1], new)
        with self.assertRaises(ValueError):
            recipe.bind(self.home, 'one', {'hook_event_name':'SubagentStart', 'session_id':str(uuid.uuid4())})
        self.assertEqual(recipe.command(self.home, state)[-1], new)

    def test_legacy_chat_can_keep_working_until_handover(self):
        state, old = self.legacy('one')
        recipe.prepare(self.home, 'one')
        new = str(uuid.uuid4())
        with closing(sqlite3.connect(state/'state_5.sqlite')) as db, db:
            db.execute('INSERT INTO threads VALUES (?,?,200,200,0)', (new, 'cli'))
        self.assertEqual(recipe.command(self.home, state)[-1], new)

    def test_config_edits_preserve_unrelated_settings_and_are_idempotent(self):
        common = recipe.shared_home(self.home)
        (common/'config.toml').write_text('model="custom"\nmodel_provider="exe"\n[features]\nfoo=true\n')
        recipe.activate(self.home)
        first = (common/'config.toml').read_text()
        backup = (common/'config.before-kuggis-login.toml').read_bytes()
        recipe.activate(self.home)
        self.assertEqual(tomllib.loads(first), tomllib.loads((common/'config.toml').read_text()))
        self.assertEqual(tomllib.loads(first)['features'], {'foo':True})
        self.assertEqual(tomllib.loads(first)['model'], 'custom')
        self.assertEqual((common/'config.before-kuggis-login.toml').read_bytes(), backup)

    def test_concurrent_migration_preserves_one_binding_and_profile(self):
        state, session = self.legacy('one')
        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda _: recipe.prepare(self.home, 'one'), range(8)))
        self.assertEqual(recipe.command(self.home, state)[-1], session)
        self.assertEqual(len(tomllib.loads((self.home/'.codex/kuggis-one.config.toml').read_text())['hooks']['SessionStart']),1)

    def test_old_jsonl_handoffs_are_bound_without_rewriting_transcripts(self):
        state = recipe.chat_home(self.home, 'imported')
        (state/'sessions').mkdir(parents=True)
        session = str(uuid.uuid4())
        transcript = state/'sessions/rollout-import.jsonl'
        content = json.dumps({'type':'session_meta','payload':{'id':session,'source':'vscode'}})+'\n'
        transcript.write_text(content)
        recipe.prepare(self.home, 'imported')
        self.assertEqual(recipe.command(self.home, state)[-1], session)
        self.assertEqual(transcript.read_text(), content)


class CallbackIsolation(unittest.TestCase):
    def test_relay_targets_only_guest_loopback_and_never_an_arbitrary_url(self):
        for request in [{'port':443,'path':'/auth/callback'},
                        {'port':1455,'path':'http://evil.example/auth/callback'},
                        {'port':1455,'path':'//evil.example/success'},
                        {'port':1455,'path':'/cancel'},
                        {'port':1455,'path':'/auth/callback\r\nInjected: yes'}]:
            with patch.object(guest.http.client, 'HTTPConnection') as connection:
                with self.assertRaises(ValueError):
                    guest.callback(request)
                connection.assert_not_called()

    def test_callback_codes_travel_in_stdin_and_response_is_not_logged(self):
        auth_url='https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback'
        try:
            server, thread = login.callback_server({}, auth_url)
        except login.Failure:
            self.skipTest('local Codex callback port is occupied')
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        body = b'login complete'
        with patch.object(login, 'guest_operation', return_value={'status':200,'headers':[('Content-Type','text/plain')],
                            'body':base64.b64encode(body).decode()}) as operation:
            with build_opener(ProxyHandler({})).open('http://127.0.0.1:1455/auth/callback?code=private&state=expected') as response:
                self.assertEqual(response.read(), body)
                self.assertEqual(response.headers['Cache-Control'], 'no-store')
            self.assertEqual(operation.call_args.args[2], {'port':1455,'path':'/auth/callback?code=private&state=expected'})
            with self.assertRaises(HTTPError):
                build_opener(ProxyHandler({})).open('http://127.0.0.1:1455/arbitrary')
            self.assertEqual(operation.call_count,1)

    def test_native_authorization_origin_and_callback_are_validated(self):
        for url in ['http://auth.openai.com/oauth/authorize', 'https://evil.example/oauth/authorize',
                    'https://auth.openai.com/oauth/authorize?redirect_uri=http://evil.example:1455/auth/callback',
                    'https://auth.openai.com/oauth/authorize?redirect_uri=http://127.0.0.1:22/auth/callback']:
            with self.assertRaises(login.Failure):
                login.callback_server({}, url)


class LoginCLI(unittest.TestCase):
    # Reuse the real binary/SSH/host-policy fixture, not production SSH or accounts.
    cli = management_tests.ManagementCLI.cli
    calls = management_tests.ManagementCLI.calls

    def setUp(self):
        management_tests.ManagementCLI.setUp(self)
        self.workspace['agents']={'codex':{'launch':['/home/agent/.local/bin/kuggis-codex','default'],
                                        'chat_launch':['/home/agent/.local/bin/kuggis-codex','{chat}']}}
        self.workspace['default_agent']='codex'
        self.config.write_text(json.dumps(self.workspace))
        (self.remote/'workspace.json').write_text(json.dumps(self.workspace))
        self.home=self.remote/'agent'
        binary=self.home/'.local/bin/codex'
        binary.parent.mkdir(parents=True)
        shutil.copyfile(ROOT/'tests/fake_codex_login.py',binary)
        binary.chmod(0o755)
        template=self.home/'.local/share/kuggis/codex/config.toml'
        template.parent.mkdir(parents=True)
        template.write_text('model="gpt-6.1-sol"\nmodel_provider="exe"\n')

    def test_login_plan_reports_exact_grants_without_mutating_host_or_launcher(self):
        original=self.config.read_bytes()
        report=self.cli('login','fri','--agent','codex','--plan')
        self.assertEqual(report['network_grants'],['auth.openai.com:443','chatgpt.com:443'])
        self.assertFalse(report['cell_restart'])
        self.assertEqual(report['restart'],'proxy')
        self.assertEqual(self.calls(),[])
        self.assertEqual(self.config.read_bytes(),original)

    def test_invalid_agent_flags_and_missing_cell_fail_without_remote_effects(self):
        for args in [('login','fri','--agent','pi'),('login','fri'),
                     ('login','fri','--agent','codex','--unknown'),
                     ('login','fri','--agent','codex','--status','--force')]:
            self.cli(*args,ok=False)
        self.assertEqual(self.calls(),[])

    def test_existing_native_login_applies_to_shell_and_all_chats_without_browser(self):
        common=self.home/'.codex'
        common.mkdir()
        (common/'auth.json').write_text('{"test_credential":"never output"}')
        chat=self.home/'.kuggis/agents/codex/research'
        chat.mkdir(parents=True)
        (chat/'config.toml').write_text('model="gpt-6.1-terra"\nmodel_provider="exe"\n')
        report=self.cli('login','fri','--agent','codex','--no-open')
        self.assertTrue(report['authenticated'])
        self.assertTrue(report['reused'])
        self.assertEqual(self.calls(),['restart-proxy'])
        self.assertEqual(tomllib.loads((common/'config.toml').read_text())['model_provider'],'openai')
        self.assertEqual(tomllib.loads((common/'kuggis-research.config.toml').read_text())['model'],'gpt-6.1-terra')
        self.assertFalse((chat/'auth.json').exists())
        self.assertEqual((common/'auth.json').read_text(),'{"test_credential":"never output"}')
        self.assertEqual(self.cli('login','fri','--agent','codex','--status')['authenticated'],True)
        self.assertEqual(self.cli('login','fri','--agent','codex','--no-open')['reused'],True)
        self.assertEqual(self.calls(),['restart-proxy'])

    def test_fresh_browser_login_crosses_the_guest_relay_and_closes_both_listeners(self):
        available=socket.socket()
        available.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        try:
            available.bind(('127.0.0.1',1455))
        except OSError:
            self.skipTest('local Codex callback port is occupied')
        finally:
            available.close()
        child=subprocess.Popen([self.binary,'login','fri','--agent','codex','--no-open','--json'],
                               env=self.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            self.assertTrue(select.select([child.stdout],[],[],15)[0],'login never offered browser authorization')
            authorization=json.loads(child.stdout.readline())
            self.assertEqual(authorization['event'],'authorize')
            with build_opener(ProxyHandler({})).open('http://127.0.0.1:1455/auth/callback?code=private-authorization-code&state=fixture-state',timeout=20) as response:
                self.assertEqual(response.read(),b'You are signed in')
            stdout,stderr=child.communicate(timeout=20)
            self.assertEqual(child.returncode,0,stderr.decode()+stdout.decode())
            self.assertTrue(json.loads(stdout)['authenticated'])
            self.assertNotIn(b'private-native-token',stdout+stderr)
            self.assertNotIn(b'private-authorization-code',stdout+stderr)
            self.assertTrue((self.home/'.codex/.fixture-callback-closed').exists())
            self.assertEqual(self.calls(),['restart-proxy'])
        finally:
            if child.poll() is None:
                child.kill();child.communicate()

    def test_occupied_callback_port_cancels_native_login_without_writing_credentials(self):
        occupied=socket.socket()
        occupied.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        try:
            occupied.bind(('127.0.0.1',1455));occupied.listen()
        except OSError:
            occupied.close();self.skipTest('local Codex callback port is occupied')
        self.addCleanup(occupied.close)
        report=self.cli('login','fri','--agent','codex','--no-open',ok=False)
        self.assertIn('occupied',report['error']['message'])
        self.assertFalse((self.home/'.codex/auth.json').exists())
        self.assertTrue((self.home/'.codex/.fixture-callback-closed').exists())

    def test_ctrl_c_cancels_waiting_login_and_closes_native_listener(self):
        available=socket.socket()
        available.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        try:
            available.bind(('127.0.0.1',1455))
        except OSError:
            self.skipTest('local Codex callback port is occupied')
        finally:
            available.close()
        child=subprocess.Popen([self.binary,'login','fri','--agent','codex','--no-open','--json'],
            env=self.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
        try:
            self.assertTrue(select.select([child.stdout],[],[],15)[0])
            self.assertEqual(json.loads(child.stdout.readline())['event'],'authorize')
            os.killpg(child.pid,signal.SIGINT)
            stdout,stderr=child.communicate(timeout=20)
            self.assertNotEqual(child.returncode,0)
            self.assertFalse((self.home/'.codex/auth.json').exists())
            self.assertTrue((self.home/'.codex/.fixture-callback-closed').exists())
        finally:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGKILL);child.communicate()


if __name__=='__main__':
    unittest.main()
