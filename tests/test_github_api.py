"""API authority tests, including forged guest requests and real host socket forwarding."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import socketserver
import ssl
import subprocess
import tempfile
import threading
import concurrent.futures
import unittest
from unittest.mock import patch
from test_github import FakeGitHub, b, c, g, h, o
from test_runtime import ROOT
from github_api import GitHubAPI, execute, plan
import github_collab


class FakeAPI:
    def __init__(self):
        self.calls = []
        self.ids = {'owner/a': 11, 'owner/b': 22, 'fork/a': 33}
        self.sessions = 0

    def status(self):
        return {'authenticated': True, 'login': 'operator'}

    def session(self):
        self.sessions += 1
        return self.call

    def call(self, path, method='GET', body=None):
        self.calls.append((path, method, copy.deepcopy(body)))
        pieces = path.split('?')[0].strip('/').split('/')
        repo = '/'.join(pieces[1:3])
        identity = self.ids[repo]
        if len(pieces) == 3:
            result = {'id': identity, 'full_name': repo}
            if repo == 'fork/a':
                result['source'] = {'id': self.ids['owner/a']}
            return result
        issue = {'number': 1, 'title': 'Issue', 'body': 'text', 'state': 'open', 'user': {'login': 'author'}}
        pr = {'number': 2, 'title': 'PR', 'pull_request': {'url': 'unused'},
              'base': {'repo': {'id': identity}}, 'head': {'sha': 'a' * 40}}
        if method != 'GET':
            return {'number': 2 if pieces[3] == 'pulls' else 1, 'body': (body or {}).get('body', ''),
                    'html_url': 'https://github.com/' + repo + '/issues/1'}
        if pieces[-1] in ('comments', 'reviews'):
            return [{'id': 123, 'body': 'comment', 'user': {'login': 'author'}}]
        if pieces[-1] == 'files':
            return [{'filename': 'file.py', 'patch': '+line', 'raw_url': 'https://untrusted.invalid'}]
        if len(pieces) == 4:
            return [issue, pr] if pieces[3] == 'issues' else [pr]
        return pr if pieces[-1] == '2' else issue


class APIGrants(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.backend = FakeGitHub()
        self.backend.transport = 'ssh-gateway'
        self.api = FakeAPI()
        self.broker = b.Broker(self.root / 'db', self.backend, 'a' * 64, api=self.api)
        self.fri = self.broker.enroll('fri')
        self.other = self.broker.enroll('other')
        for cell in (self.fri, self.other):
            self.broker.change(cell['cell'], 'owner/a', 'read')

    def grant(self, who=None, issues='write', pulls='write', merge=False):
        return self.broker.change_api((who or self.fri)['cell'], 'owner/a', issues, pulls, merge)

    def operation(self, kind='issue', action='list', params=None, who=None, repo='owner/a', **extra):
        return self.broker.handle('/api', (who or self.fri)['credential'],
            {'repo': repo, 'kind': kind, 'action': action, 'params': params or {}, **extra})['result']

    def test_roles_set_both_scopes_preserve_identity_and_do_not_affect_other_cells(self):
        def role(value):
            return self.broker.handle('/admin/repos/add', 'a' * 64,
                {'cell': self.fri['cell'], 'repo': 'owner/a', 'role': value})
        role('develop')
        self.operation(action='create', params={'title': 'new'})
        self.operation('pr', 'comment', {'number': 2, 'body': 'review'})
        self.grant(merge=True)
        role('read')
        grant = self.broker.listing(self.fri['cell'])['repositories'][0]
        self.assertEqual(grant['permissions']['contents'], 'read')
        self.assertEqual(grant['api_permissions'], {'issues': 'read', 'pull_requests': 'read', 'merge': False})
        self.assertEqual(grant['api_repository_id'], 11)
        self.operation('pr', 'view', {'number': 2})
        for kwargs in ({'action': 'create', 'params': {'title': 'denied'}},
                       {'kind': 'pr', 'action': 'comment', 'params': {'number': 2, 'body': 'denied'}},
                       {'who': self.other}):
            with self.assertRaises(c.Failure):
                self.operation(**kwargs)
        self.assertEqual(role('read'), self.broker.listing(self.fri['cell']))
        role('develop')
        with self.assertRaisesRegex(c.Failure, 'merging'):
            self.operation('pr', 'merge', {'number': 2, 'sha': 'a' * 40})
        # Explicit Git-only changes retain API scope; removal clears both.
        self.broker.change(self.fri['cell'], 'owner/a', 'read')
        self.operation(action='create', params={'title': 'still allowed'})
        self.broker.change(self.fri['cell'], 'owner/a')
        with self.assertRaises(c.Failure):
            self.operation()

    def test_invalid_roles_and_failed_resolution_do_not_partially_change_grants(self):
        before = self.broker.listing(self.fri['cell'])
        body = {'cell': self.fri['cell'], 'repo': 'owner/a', 'role': 'develop'}
        for bad in (None, '', 'admin', True, []):
            with self.assertRaises(c.Failure):
                self.broker.handle('/admin/repos/add', 'a' * 64, {**body, 'role': bad})
        with self.assertRaises(c.Failure):
            self.broker.handle('/admin/repos/add', 'a' * 64, {**body, 'access': 'write'})
        with self.assertRaises(c.Failure):
            self.broker.handle('/admin/repos/add', self.fri['credential'], body)
        with patch.object(self.backend, 'resolve', side_effect=c.Failure('upstream unavailable')):
            for repo in ('owner/a', 'owner/b'):
                with self.assertRaises(c.Failure):
                    self.broker.handle('/admin/repos/add', 'a' * 64, {**body, 'repo': repo})
        with patch.object(self.broker, 'transport', 'github'):
            with self.assertRaisesRegex(c.Failure, 'SSH gateway'):
                self.broker.handle('/admin/repos/add', 'a' * 64, body)
        self.assertEqual(self.broker.listing(self.fri['cell']), before)

    def test_old_broker_requires_upgrade_without_falling_back_to_partial_grants(self):
        config = self.root / 'workspace.json'
        url = 'https://broker.invalid'
        c.atomic_json(Path(str(config) + '.github.json'), {'url': url, 'cell': self.fri['cell']})
        operator = o.Operator({'config': str(config), 'settings': str(self.root / 'operator.json')})
        args = o.options(['repo', 'add', 'fri', 'owner/a', '--role', 'develop'])
        with patch.object(operator, 'configured', return_value=(url, 'a' * 64)), \
                patch.object(operator, 'api', side_effect=c.Failure('unknown operation or fields', 404)) as request:
            with self.assertRaisesRegex(c.Failure, 'upgrade the broker'):
                operator.run(args)
            request.assert_called_once_with('/admin/repos/add',
                {'cell': self.fri['cell'], 'repo': 'owner/a', 'role': 'develop'})

    def test_default_deny_and_separate_cell_repo_resource_levels(self):
        with self.assertRaises(c.Failure):
            self.operation()
        self.assertEqual(self.api.sessions, 0)
        self.grant(issues='write', pulls='read')
        self.operation(action='create', params={'title': 'new'})
        self.operation('pr', 'view', {'number': 2})
        for kwargs in ({'who': self.other}, {'repo': 'owner/b'},
                       {'kind': 'pr', 'action': 'comment', 'params': {'number': 2, 'body': 'x'}}):
            with self.assertRaises(c.Failure):
                self.operation(**kwargs)
        # API write does not upgrade Git read scope.
        self.assertEqual(self.broker.listing(self.fri['cell'])['repositories'][0]['permissions']['contents'], 'read')

    def test_forged_identity_paths_graphql_and_unsafe_fields_never_reach_upstream(self):
        self.grant()
        cases = [dict(cell=self.other['cell']), dict(action='api'), dict(kind='graphql'),
                 dict(repo='owner/a/../../other/b'), dict(action='view', params={'number': '../2'}),
                 dict(action='view', params={'number': True}),
                 dict(action='list', params={'url': 'https://evil.invalid'}),
                 dict(action='create', params={'title': 'x', 'repository': 'other/b'}),
                 dict(kind='pr', action='create', params={'title': 'x', 'head': 'other:branch', 'base': 'main'}),
                 dict(kind='pr', action='merge', params={'number': 2, 'sha': 'bad'}),
                 dict(action='list', params={'limit': 101})]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(c.Failure):
                self.operation(**value)
        self.assertEqual(self.api.calls, [])

    def test_issue_alias_cannot_bypass_pr_permission_and_list_hides_prs(self):
        self.grant(issues='write', pulls='none')
        result = self.operation()
        self.assertEqual([row['number'] for row in result], [1])
        for action, params in [('view', {'number': 2}), ('close', {'number': 2}),
                               ('comment', {'number': 2, 'body': 'bypass'}), ('comments', {'number': 2})]:
            with self.assertRaisesRegex(c.Failure, 'pull request'):
                self.operation(action=action, params=params)
        self.assertTrue(all(method == 'GET' for _, method, _ in self.api.calls))

    def test_routes_are_constructed_from_repo_and_local_numbers(self):
        self.grant()
        cases = [
            ('issue', 'create', {'title': 'x', 'body': 'text'}, '/issues', 'POST'),
            ('issue', 'edit', {'number': 1, 'title': 'new'}, '/issues/1', 'PATCH'),
            ('issue', 'close', {'number': 1}, '/issues/1', 'PATCH'),
            ('issue', 'reopen', {'number': 1}, '/issues/1', 'PATCH'),
            ('issue', 'comment', {'number': 1, 'body': 'x'}, '/issues/1/comments', 'POST'),
            ('pr', 'create', {'title': 'p', 'head': 'feature/x', 'base': 'main', 'draft': True}, '/pulls', 'POST'),
            ('pr', 'comment', {'number': 2, 'body': 'x'}, '/issues/2/comments', 'POST'),
            ('pr', 'review', {'number': 2, 'body': 'looks good', 'event': 'APPROVE'}, '/pulls/2/reviews', 'POST')]
        for kind, action, params, suffix, method in cases:
            with self.subTest(action=action):
                self.operation(kind, action, params)
                self.assertEqual(self.api.calls[-1][:2], ('/repos/owner/a' + suffix, method))
        result = self.operation('pr', 'diff', {'number': 2})
        self.assertNotIn('raw_url', result[0])

    def test_merge_is_explicit_and_includes_expected_head(self):
        self.grant()
        with self.assertRaisesRegex(c.Failure, 'merging'):
            self.operation('pr', 'merge', {'number': 2, 'sha': 'a' * 40})
        self.grant(merge=True)
        self.operation('pr', 'merge', {'number': 2, 'sha': 'a' * 40, 'method': 'squash'})
        self.assertEqual(self.api.calls[-1], ('/repos/owner/a/pulls/2/merge', 'PUT',
                                            {'sha': 'a' * 40, 'merge_method': 'squash'}))

    def test_fork_requires_grant_and_same_network(self):
        self.grant()
        params = {'title': 'p', 'head': 'feature', 'base': 'main', 'head_repo': 'fork/a'}
        with self.assertRaisesRegex(c.Failure, 'not granted'):
            self.operation('pr', 'create', params)
        self.assertNotIn('/repos/fork/a', [p for p, _, _ in self.api.calls])
        self.broker.change(self.fri['cell'], 'fork/a', 'read')
        self.operation('pr', 'create', params)
        self.assertEqual(self.api.calls[-1][2]['head'], 'fork:feature')
        self.broker.change(self.fri['cell'], 'owner/b', 'read')
        with self.assertRaisesRegex(c.Failure, 'fork network'):
            self.operation('pr', 'create', {**params, 'head_repo': 'owner/b'})

    def test_identity_pinned_and_downgrade_removal_persist(self):
        self.grant()
        self.operation()
        self.broker.change(self.fri['cell'], 'owner/a', 'write')
        grant = self.broker.listing(self.fri['cell'])['repositories'][0]
        self.assertEqual(grant['api_repository_id'], 11)
        self.assertEqual(grant['api_permissions']['issues'], 'write')
        self.api.ids['owner/a'] = 999
        with self.assertRaisesRegex(c.Failure, 'identity changed'):
            self.operation(action='create', params={'title': 'x'})
        self.api.ids['owner/a'] = 11
        self.grant(issues='read', pulls='none')
        self.broker = b.Broker(self.root / 'db', self.backend, 'a' * 64, api=self.api)
        with self.assertRaises(c.Failure):
            self.operation(action='create', params={'title': 'x'})
        self.operation()
        self.broker.change(self.fri['cell'], 'owner/a')
        with self.assertRaises(c.Failure):
            self.operation()

    def test_missing_api_login_does_not_change_git_or_grants(self):
        self.grant()
        self.broker.api = None
        with self.assertRaisesRegex(c.Failure, 'not configured'):
            self.operation()
        self.assertEqual(len(self.broker.listing(self.fri['cell'])['repositories']), 1)

    def test_revocation_waits_for_inflight_operation_then_denies_new_requests(self):
        self.grant()
        entered, release = threading.Event(), threading.Event()
        original = self.api.call
        def call(path, method='GET', body=None):
            if method == 'POST':
                entered.set()
                if not release.wait(5):
                    raise RuntimeError('test did not release request')
            return original(path, method, body)
        self.api.call = call
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            operation = pool.submit(self.operation, action='create', params={'title': 'x'})
            self.assertTrue(entered.wait(5))
            removal = pool.submit(self.broker.change, self.fri['cell'], 'owner/a')
            self.assertFalse(removal.done())
            release.set()
            operation.result(timeout=5)
            removal.result(timeout=5)
        with self.assertRaises(c.Failure):
            self.operation()

    def test_real_cli_grants_and_guest_relay_over_https(self):
        certificate, key = self.root / 'cert.pem', self.root / 'key.pem'
        config = self.root / 'openssl.cnf'
        config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3\n'
            '[dn]\nCN=localhost\n[v3]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n'
            'basicConstraints=critical,CA:TRUE\nkeyUsage=digitalSignature,keyEncipherment,keyCertSign\n')
        subprocess.run(['/usr/bin/openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-x509', '-days', '1',
                        '-config', str(config), '-keyout', str(key), '-out', str(certificate)],
                       check=True, capture_output=True)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        server = b.Server(('127.0.0.1', 0), context, self.broker)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        relay = socketserver.UnixStreamServer(str(self.root / 'tls-relay'), h.Relay)
        relay_thread = threading.Thread(target=relay.serve_forever, daemon=True)
        relay_thread.start()
        url = 'https://localhost:%d' % server.server_address[1]
        admin = self.root / 'admin'
        admin.write_text('a' * 64)
        admin.chmod(0o600)
        workspace = self.root / 'workspace.json'
        c.atomic_json(workspace, {'version': 2, 'name': 'fri', 'ssh': 'fixture', 'launch': ['bash']})
        c.atomic_json(Path(str(workspace) + '.github.json'), {'url': url, 'cell': self.fri['cell']})
        env = {**os.environ, 'SSL_CERT_FILE': str(certificate), 'KUGGIS_STATE_DIR': str(self.root / 'operator')}
        binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
        def cli(*args):
            result = subprocess.run([binary, *map(str, args)], capture_output=True, env=env, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertNotIn(admin.read_bytes(), result.stdout)
            return json.loads(result.stdout)
        try:
            cli('github', 'connect', '--url', url, '--admin-token-file', admin)
            self.assertEqual(cli('github', 'api-status')['login'], 'operator')
            added = cli('repo', 'add', workspace, 'owner/b', '--role', 'develop')
            grant = next(row for row in added['repositories'] if row['repo'] == 'owner/b')
            self.assertEqual(grant['permissions']['contents'], 'write')
            self.assertEqual(grant['api_permissions'], {'issues': 'write', 'pull_requests': 'write', 'merge': False})
            cli('repo', 'add', workspace, 'owner/b', '--role', 'read')
            self.operation('pr', 'list', repo='owner/b')
            with self.assertRaises(c.Failure):
                self.operation('pr', 'comment', {'number': 2, 'body': 'denied'}, repo='owner/b')
            cli('repo', 'api', workspace, 'owner/a', '--issues', 'write', '--pull-requests', 'read')
            with patch.dict(os.environ, {'SSL_CERT_FILE': str(certificate)}), \
                    patch.object(h, 'connection', return_value={'url': url, **self.fri}), \
                    patch.object(g, 'SOCKET', str(self.root / 'tls-relay')):
                body = {'repo': 'owner/a', 'kind': 'issue', 'action': 'create',
                        'params': {'title': 'new', 'body': 'x' * 9000}}
                self.assertEqual(g.api_request('/api', body)['body'], 'x' * 9000)
                with self.assertRaisesRegex(c.Failure, 'API permission'):
                    g.api_request('/api', {**body, 'kind': 'pr',
                                          'params': {'title': 'new', 'base': 'main', 'head': 'feature'}})
                cli('repo', 'api', workspace, 'owner/a', '--issues', 'none', '--pull-requests', 'none')
                with self.assertRaisesRegex(c.Failure, 'API permission'):
                    g.api_request('/api', body)
        finally:
            relay.shutdown()
            relay.server_close()
            relay_thread.join()
            server.shutdown()
            server.server_close()
            thread.join()

    def test_guest_cli_through_unix_relay_and_forged_direct_socket_request(self):
        self.grant()
        server = socketserver.UnixStreamServer(str(self.root / 'socket'), h.Relay)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            def forward(url, credential, body, **kwargs):
                self.assertEqual(credential, self.fri['credential'])
                return self.broker.handle('/api', credential, body)
            with patch.object(h, 'connection', return_value={'url': 'https://broker.invalid', **self.fri}), \
                    patch.object(h, 'request', side_effect=forward), patch.object(g, 'SOCKET', str(self.root / 'socket')), \
                    patch.object(g, 'transport', return_value='ssh-gateway'):
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    g.gh(['--repo', 'owner/a', '--', 'issue', 'create', '--title', 'new', '--body', 'content'])
                self.assertEqual(json.loads(output.getvalue())['body'], 'content')
                self.assertNotIn(self.fri['credential'], output.getvalue())
                with self.assertRaises(c.Failure):
                    g.api_request('/api', {'repo': 'owner/a', 'kind': 'issue', 'action': 'list', 'params': {},
                                           'cell': self.other['cell']})
                self.grant(issues='none', pulls='none')
                with self.assertRaisesRegex(c.Failure, 'API permission'):
                    g.api_request('/api', {'repo': 'owner/a', 'kind': 'issue', 'action': 'list', 'params': {}})
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


class APICredentialsAndCLI(unittest.TestCase):
    def test_provider_uses_protected_gh_login_and_ignores_inherited_token(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            provider = GitHubAPI(root)
            with self.assertRaisesRegex(c.Failure, 'login is missing'):
                provider.token()
            (root / 'hosts.yml').write_text('fixture')
            (root / 'hosts.yml').chmod(0o600)
            with patch.dict(os.environ, {'GH_TOKEN': 'wrong', 'GH_HOST': 'evil.invalid'}), \
                    patch('github_api.subprocess.run', return_value=subprocess.CompletedProcess([], 0, b'upstream-secret\n', b'')) as run:
                provider.gh = '/usr/bin/gh'
                self.assertEqual(provider.token(), 'upstream-secret')
                self.assertNotIn('GH_TOKEN', run.call_args.kwargs['env'])
                self.assertEqual(run.call_args.args[0], ['/usr/bin/gh', 'auth', 'token', '--hostname', 'github.com'])
            (root / 'hosts.yml').chmod(0o644)
            with self.assertRaises(c.Failure):
                provider.token()

    def test_cli_body_files_and_rejection_of_unsupported_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'body'
            path.write_text('multiline\nbody')
            result = github_collab.parse(['pr', 'create', '--title', 'new', '--base', 'main', '--head', 'feature',
                                         '--head-repo', 'fork/a', '--body-file', str(path)])
            self.assertEqual(result['params']['body'], 'multiline\nbody')
            plan(**result)
        for args in (['api', 'graphql'], ['pr', 'list', '--hostname', 'evil.invalid'], ['issue', 'delete', '1'],
                     ['pr', 'list', '--repo', 'other/b']):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                github_collab.parse(args)
        args = o.options(['repo', 'api', 'fri', 'owner/a', '--issues', 'write', '--pull-requests', 'read'])
        self.assertFalse(args.merge)
        for flags in ([], ['--role', 'admin'], ['--role', 'read', '--access', 'write'],
                      ['--role', 'develop', '--workflows']):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                o.options(['repo', 'add', 'fri', 'owner/a', *flags])


if __name__ == '__main__':
    unittest.main()
