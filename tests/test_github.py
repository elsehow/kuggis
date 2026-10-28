"""Broker, relay and credential integration; no live GitHub credentials required."""
import concurrent.futures
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import secrets
import socket
import socketserver
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from test_runtime import ROOT, module

sys.path.insert(0, str(ROOT / 'crates/kuggis/runtime'))
import github_common as c
import github_broker as b
import github_guest as g
import github_host as h
import github_operator as o


class FakeGitHub:
    def __init__(self):
        self.issued = {}
        self.revoked = set()
        self.unavailable = False
        self.resolutions = 0

    def resolve(self, repo, scope):
        self.resolutions += 1
        return {'repository_id': {'owner/a': 11, 'owner/b': 22}.get(repo, 33), 'installation_id': 7}

    def mint(self, grant):
        token = 'ghs_' + secrets.token_hex(32)
        self.issued[token] = copy.deepcopy(grant)
        return token, time.time() + 3600

    def revoke(self, token):
        if self.unavailable:
            raise c.Failure('offline', 503)
        self.revoked.add(token)


class Grants(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.db = Path(self.temp.name) / 'broker.db'
        self.github = FakeGitHub()
        self.admin = 'operator-' + 'a' * 40
        self.broker = b.Broker(self.db, self.github, self.admin)
        self.fri = self.broker.enroll('fri')
        self.merri = self.broker.enroll('merrillfront')

    def tearDown(self):
        self.temp.cleanup()

    def add(self, who, repo='owner/a', access='read', workflows=False):
        return self.broker.change(who['cell'], repo, access, workflows)

    def token(self, who, repo='owner/a'):
        return self.broker.issue(who['credential'], repo)['token']

    def test_same_repo_different_cells_and_mixed_permissions(self):
        self.add(self.fri)
        self.add(self.merri, access='write')
        self.add(self.fri, 'owner/b', 'write')
        first, second, third = self.token(self.fri), self.token(self.merri), self.token(self.fri, 'owner/b')
        self.assertEqual(len({first, second, third}), 3)
        self.assertEqual(self.github.issued[first]['permissions']['contents'], 'read')
        self.assertEqual(self.github.issued[second]['permissions']['contents'], 'write')
        self.assertEqual(self.github.issued[third]['repository_id'], 22)
        self.assertEqual(self.token(self.fri), first)
        with self.assertRaises(c.Failure):
            self.token(self.merri, 'owner/b')
        # No schema limit of one or two repositories.
        for i in range(20):
            self.add(self.fri, 'owner/repo%d' % i)
        self.assertEqual(len(self.broker.listing(self.fri['cell'])['repositories']), 22)

    def test_cell_identity_and_permissions_cannot_be_supplied_by_guest(self):
        self.add(self.fri)
        for extra in ({'cell': self.merri['cell']}, {'permissions': {'contents': 'write'}}, {'access': 'write'}):
            with self.assertRaises(c.Failure):
                self.broker.handle('/token', self.fri['credential'], {'repo': 'owner/a', **extra})
        for token in (self.fri['credential'], 'wrong'):
            with self.assertRaises(c.Failure):
                self.broker.handle('/admin/repos/remove', token, {'cell': self.merri['cell'], 'repo': 'owner/a'})
        with self.assertRaises(c.Failure):
            self.broker.issue(self.admin, 'owner/a')

    def test_removal_and_downgrade_revoke_only_affected_tokens(self):
        self.add(self.fri, access='write')
        self.add(self.merri, access='write')
        self.add(self.fri, 'owner/b', 'write')
        old, other, sibling = self.token(self.fri), self.token(self.merri), self.token(self.fri, 'owner/b')
        self.add(self.fri, access='read')
        self.assertEqual(self.github.revoked, {old})
        fresh = self.token(self.fri)
        self.assertEqual(self.github.issued[fresh]['permissions']['contents'], 'read')
        self.assertEqual(self.token(self.merri), other)
        self.assertEqual(self.token(self.fri, 'owner/b'), sibling)
        self.broker.change(self.fri['cell'], 'owner/a')
        self.assertIn(fresh, self.github.revoked)
        with self.assertRaises(c.Failure):
            self.token(self.fri)

    def test_failed_revocation_survives_restart_and_blocks_issuance(self):
        self.add(self.fri, access='write')
        old = self.token(self.fri)
        self.github.unavailable = True
        with self.assertRaisesRegex(c.Failure, 'grant saved; revocation incomplete'):
            self.add(self.fri, access='read')
        self.broker = b.Broker(self.db, self.github, self.admin)
        report = self.broker.listing(self.fri['cell'])
        self.assertEqual(report['repositories'][0]['permissions']['contents'], 'read')
        self.assertEqual(report['pending_revocations'], 1)
        with self.assertRaisesRegex(c.Failure, 'revocation pending'):
            self.token(self.fri)
        self.github.unavailable = False
        self.add(self.fri, access='read')
        self.assertIn(old, self.github.revoked)
        self.assertEqual(self.broker.listing(self.fri['cell'])['pending_revocations'], 0)

    def test_remove_while_offline_never_restores_grant_and_expiry_unblocks(self):
        self.add(self.fri)
        old = self.token(self.fri)
        self.github.unavailable = True
        with self.assertRaises(c.Failure):
            self.broker.change(self.fri['cell'], 'owner/a')
        self.assertEqual(self.broker.listing(self.fri['cell'])['repositories'], [])
        with self.broker.db() as db:
            db.execute('UPDATE tokens SET expires=0 WHERE token=?', (old,))
        self.broker.change(self.fri['cell'], 'owner/a')
        self.assertEqual(self.broker.listing(self.fri['cell'])['pending_revocations'], 0)

    def test_concurrent_issuance_and_removal_leave_no_live_record(self):
        self.add(self.fri)
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            issued = list(pool.map(lambda _: self.token(self.fri), range(16)))
        self.assertEqual(len(set(issued)), 1)
        self.broker.change(self.fri['cell'], 'owner/a')
        self.assertTrue(set(issued) <= self.github.revoked)

    def test_removal_waits_for_inflight_mint_then_revokes_it(self):
        self.add(self.fri)
        entered, release = threading.Event(), threading.Event()
        mint = self.github.mint
        def paused(grant):
            entered.set()
            if not release.wait(5):
                raise RuntimeError('test mint timed out')
            return mint(grant)
        with patch.object(self.github, 'mint', side_effect=paused), concurrent.futures.ThreadPoolExecutor() as pool:
            issuance = pool.submit(self.token, self.fri)
            self.assertTrue(entered.wait(5))
            removal = pool.submit(self.broker.change, self.fri['cell'], 'owner/a')
            release.set()
            token = issuance.result(timeout=5)
            removal.result(timeout=5)
        self.assertIn(token, self.github.revoked)
        with self.assertRaises(c.Failure):
            self.token(self.fri)

    def test_idempotence_and_workflow_permission_are_explicit(self):
        self.add(self.fri, access='write')
        old = self.token(self.fri)
        self.add(self.fri, access='write')
        self.assertEqual(self.token(self.fri), old)
        self.assertNotIn('workflows', self.github.issued[old]['permissions'])
        self.add(self.fri, access='write', workflows=True)
        self.assertIn(old, self.github.revoked)
        self.assertEqual(self.github.issued[self.token(self.fri)]['permissions']['workflows'], 'write')
        with self.assertRaises(c.Failure):
            self.add(self.fri, workflows=True)

    def test_refresh_tracks_both_tokens_for_later_revocation(self):
        self.add(self.fri)
        old = self.token(self.fri)
        with self.broker.db() as db:
            db.execute('UPDATE tokens SET expires=? WHERE token=?', (time.time() + 60, old))
        fresh = self.token(self.fri)
        self.assertNotEqual(old, fresh)
        self.broker.change(self.fri['cell'], 'owner/a')
        self.assertTrue({old, fresh} <= self.github.revoked)


class GitHubRequests(unittest.TestCase):
    def test_every_minted_token_has_explicit_repo_and_permission_scope(self):
        api = b.GitHub('1', 'unused')
        grant = {'repository_id': 123, 'installation_id': 456, 'permissions': c.permissions('read')}
        response = {'token': 'ghs_test', 'expires_at': '2099-01-01T00:00:00Z',
                    'repositories': [{'id': 123}], 'permissions': {**grant['permissions'], 'metadata': 'read'}}
        with patch.object(api, 'jwt', return_value='jwt'), patch.object(api, 'api', return_value=response) as call, \
                patch.object(b, 'expiry', return_value=time.time() + 3500):
            self.assertEqual(api.mint(grant)[0], 'ghs_test')
        self.assertEqual(call.call_args.args[2], {'repository_ids': [123], 'permissions': c.permissions('read')})

    def test_scope_mismatch_is_revoked_and_never_returned(self):
        api = b.GitHub('1', 'unused')
        grant = {'repository_id': 123, 'installation_id': 456, 'permissions': c.permissions('read')}
        response = {'token': 'secret', 'repositories': [{'id': 123}, {'id': 999}], 'permissions': c.permissions('write')}
        with patch.object(api, 'jwt', return_value='jwt'), patch.object(api, 'api', return_value=response), \
                patch.object(api, 'revoke') as revoke:
            with self.assertRaises(c.Failure):
                api.mint(grant)
            revoke.assert_called_once_with('secret')

    def test_redirects_and_malformed_repository_names_are_rejected(self):
        with self.assertRaises(c.Failure):
            c.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://elsewhere.invalid')
        for repo in ('../repo', 'owner/repo.git', 'owner/repo/other', 'https://github.com/o/r', 'o/r\n', 'o/%2Fr'):
            with self.assertRaises(c.Failure):
                c.repository(repo)
        for url in ('http://broker', 'https://user:pass@broker', 'https://broker/token', 'https://broker?q=1'):
            with self.assertRaises(c.Failure):
                c.broker_url(url)


class GuestIntegration(unittest.TestCase):
    def test_git_credential_protocol_is_repository_specific(self):
        out = io.StringIO()
        with patch.object(g, 'token', return_value='short-lived') as mint:
            g.credential('get', io.StringIO('protocol=https\nhost=github.com\npath=Owner/Repo.git\n\n'), out)
            mint.assert_called_once_with('owner/repo')
        self.assertIn('password=short-lived', out.getvalue())
        with patch.object(g, 'token') as mint:
            g.credential('get', io.StringIO('protocol=https\nhost=evil.invalid\npath=Owner/Repo.git\n\n'), io.StringIO())
            g.credential('store', io.StringIO('password=secret'), io.StringIO())
            mint.assert_not_called()
            with self.assertRaises(c.Failure):
                g.credential('get', io.StringIO('protocol=https\nhost=github.com\n\n'), io.StringIO())

    def test_gh_uses_only_selected_repo_and_no_credential_in_argv(self):
        with patch.object(g, 'token', return_value='scoped'), patch.object(g.os, 'execve') as execute:
            g.gh(['--repo', 'owner/repo', '--', 'pr', 'list'])
            command, argv, env = execute.call_args.args
            self.assertEqual(command, '/usr/bin/gh')
            self.assertNotIn('scoped', argv)
            self.assertEqual(env['GH_TOKEN'], 'scoped')
            self.assertEqual(env['GH_REPO'], 'owner/repo')
        for args in (['api', 'https://evil.invalid'], ['api', '--hostname=evil.invalid', 'repos/o/r'], ['auth', 'login']):
            with self.assertRaises(c.Failure):
                g.gh(['--repo', 'owner/repo', '--', *args])

    def test_socket_relay_forwards_only_repo_with_host_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / 'token.sock')
            server = socketserver.UnixStreamServer(path, h.Relay)
            server_thread = threading.Thread(target=server.serve_forever, daemon=True)
            server_thread.start()
            try:
                with patch.object(h, 'connection', return_value={'url': 'https://broker.invalid', 'credential': 'host-secret'}), \
                        patch.object(h, 'request', return_value={'token': 'guest-token'}) as forward, patch.object(g, 'SOCKET', path):
                    self.assertEqual(g.token('owner/repo'), 'guest-token')
                    forward.assert_called_once_with('https://broker.invalid/token', 'host-secret', {'repo': 'owner/repo'})
                    conn = g.Connection('localhost', timeout=2)
                    conn.request('POST', '/token', json.dumps({'repo': 'owner/repo', 'cell': 'other'}))
                    response = conn.getresponse()
                    self.assertEqual(response.status, 400)
                    response.read()
                    conn.close()
                    self.assertEqual(forward.call_count, 1)
            finally:
                server.shutdown()
                server.server_close()
                server_thread.join()


class OperatorIntegration(unittest.TestCase):
    def test_real_cli_https_enrollment_multiple_repos_and_offline_removal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            certificate, key = root / 'cert.pem', root / 'key.pem'
            openssl_config = root / 'openssl.cnf'
            openssl_config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3\n'
                '[dn]\nCN=localhost\n[v3]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n'
                'basicConstraints=critical,CA:TRUE\nkeyUsage=digitalSignature,keyEncipherment,keyCertSign\n')
            subprocess.run(['/usr/bin/openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-x509', '-days', '1',
                '-config', str(openssl_config), '-keyout', str(key), '-out', str(certificate)],
                check=True, capture_output=True)
            github = FakeGitHub()
            admin = 'a' * 64
            broker = b.Broker(root / 'broker.db', github, admin)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, key)
            server = b.Server(('127.0.0.1', 0), context, broker)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                url = 'https://localhost:%d' % server.server_address[1]
                admin_file = root / 'admin-token'
                admin_file.write_text(admin)
                admin_file.chmod(0o600)
                config = root / 'workspace.json'
                config.write_text(json.dumps({'version': 2, 'name': 'fri', 'ssh': 'fixture', 'launch': ['bash']}))
                host_state = root / 'host.json'
                binaries = root / 'bin'
                binaries.mkdir()
                fake = binaries / 'ssh'
                fake.write_text('#!' + sys.executable + '\n' + '''import json, os, shlex, sys
from pathlib import Path
state = Path(os.environ['TEST_GITHUB_HOST'])
action = shlex.split(sys.argv[-1])[-1]
if action == 'attach':
    state.write_text(sys.stdin.read())
value = json.loads(state.read_text()) if state.exists() else None
print(json.dumps({'connected': True, 'url': value['url'], 'cell': value['cell']} if value else {'connected': False}))
''')
                fake.chmod(0o755)
                env = {**os.environ, 'KUGGIS_STATE_DIR': str(root / 'operator'), 'SSL_CERT_FILE': str(certificate),
                       'TEST_GITHUB_HOST': str(host_state),
                       'PATH': str(binaries) + os.pathsep + str(Path(sys.executable).parent) + os.pathsep + os.environ['PATH']}
                binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
                def cli(*args):
                    result = subprocess.run([binary, *map(str, args)], env=env, capture_output=True, timeout=20)
                    self.assertEqual(result.returncode, 0, result.stderr.decode())
                    self.assertNotIn(admin, result.stdout.decode())
                    return json.loads(result.stdout)
                cli('github', 'connect', '--url', url, '--admin-token-file', admin_file)
                cli('github', 'attach', config)
                cli('github', 'attach', config)  # Idempotent enrollment.
                identity = json.loads(host_state.read_text())
                self.assertNotEqual(identity['credential'], admin)
                self.assertNotIn('credential', json.loads(Path(str(config) + '.github.json').read_text()))
                cli('repo', 'add', config, 'owner/a', '--access', 'read')
                cli('repo', 'add', config, 'owner/b', '--access', 'write')
                first = broker.issue(identity['credential'], 'owner/a')['token']
                second = broker.issue(identity['credential'], 'owner/b')['token']
                self.assertEqual(github.issued[first]['permissions']['contents'], 'read')
                self.assertEqual(github.issued[second]['permissions']['contents'], 'write')
                fake.write_text('#!/bin/sh\nexit 99\n')  # Host unavailable; removal still works.
                cli('repo', 'remove', config, 'owner/a')
                self.assertIn(first, github.revoked)
                self.assertNotIn(second, github.revoked)
                result = cli('repo', 'list', config, '--json')
                self.assertEqual([r['repo'] for r in result['repositories']], ['owner/b'])
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

    def test_revocation_needs_no_live_host(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / 'workspace.json'
            c.atomic_json(config.with_name(config.name + '.github.json'), {'url': 'https://broker.invalid', 'cell': '123'})
            operator = o.Operator({'settings': str(root / 'settings'), 'config': str(config)})
            with patch.object(operator, 'configured', return_value=('https://broker.invalid', 'admin')), \
                    patch.object(operator, 'api', return_value={'ok': True}) as api, patch.object(operator, 'remote') as remote:
                operator.run(o.options(['repo', 'remove', 'merri', 'owner/repo']))
                api.assert_called_once_with('/admin/repos/remove', {'cell': '123', 'repo': 'owner/repo'})
                remote.assert_not_called()

    def test_legacy_exe_authority_requires_explicit_migration(self):
        with self.assertRaises(c.Failure):
            c.enable_network({'resources': [{'name': 'old', 'endpoints': [{'host': 'github.int.exe.xyz', 'port': 443}]}]})
        config = c.enable_network({'resources': []})
        self.assertEqual(c.enable_network(copy.deepcopy(config)), config)


if __name__ == '__main__':
    unittest.main()
