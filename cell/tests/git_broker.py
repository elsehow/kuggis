"""Real Git clients against the broker and disposable git-http-backend repos.

No GitHub account, network outside loopback, or real credentials are used.
"""
import base64
import gzip
import http.client
import http.server
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('broker', Path(__file__).parents[1] / 'src/git_broker.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


def declaration(*grants):
    return {'name': 'git-canary', 'grants': [{'code': g} for g in grants]}


class Policy(unittest.TestCase):
    def test_scopes_and_forks_are_operation_specific(self):
        cell = declaration({'repo': 'org/read', 'write': False}, {'repo': 'org/upstream', 'via': 'fork/project', 'write': True})
        for method, path in [('GET', '/org/read.git/info/refs?service=git-upload-pack'),
                             ('GET', '/ORG/Read.git/info/refs?service=git-upload-pack'),
                             ('POST', '/org/read.git/git-upload-pack'),
                             ('GET', '/fork/project.git/info/refs?service=git-receive-pack'),
                             ('POST', '/fork/project.git/git-receive-pack')]:
            b.authorize(cell, method, path)
        for method, path in [('GET', '/org/read.git/info/refs?service=git-receive-pack'),
                             ('POST', '/org/read.git/git-receive-pack'),
                             ('GET', '/org/upstream.git/info/refs?service=git-upload-pack'),
                             ('POST', '/org/upstream.git/git-receive-pack'),
                             ('GET', '/org/other.git/info/refs?service=git-upload-pack')]:
            with self.subTest(path=path), self.assertRaises(PermissionError):
                b.authorize(cell, method, path)

    def test_url_method_and_service_ambiguities_fail_closed(self):
        cell = declaration({'repo': 'org/read', 'write': True})
        for path in ['/org/read.git/info/refs', '/org/read.git/objects/123',
                     '/org/read.git/info/refs?service=git-upload-pack&service=git-receive-pack',
                     '/org/read.git/info/refs?service=git-upload-pack&x=y',
                     '/org/read.git/git-upload-archive',
                     '/org%2fread.git/git-upload-pack', '/org/read.git/../other.git/git-upload-pack',
                     '/org/read.git/git-upload-pack?service=git-receive-pack',
                     'https://github.com/org/read.git/git-upload-pack', '//org/read.git/git-upload-pack',
                     '/org/read.git/git-upload-pack#fragment', '/api/repos/org/read']:
            with self.subTest(path=path), self.assertRaises(PermissionError):
                b.authorize(cell, 'POST', path)
        with self.assertRaises(PermissionError):
            b.authorize(cell, 'POST', '/org/read.git/info/refs?service=git-upload-pack')

    def test_credentials_reject_symlinks_permissions_and_header_injection(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'tokens'; path.write_text('{"org/read":"synthetic_token"}'); path.chmod(0o600)
            self.assertEqual(b.credentials(path), {'org/read': 'synthetic_token'})
            alias = Path(root) / 'alias'; alias.symlink_to(path)
            with self.assertRaises(OSError): b.credentials(alias)
            path.chmod(0o644)
            with self.assertRaises(ValueError): b.credentials(path)
            path.chmod(0o600)
            path.write_text(json.dumps({'org/read': 'token\r\nInjected: yes'}))
            with self.assertRaises(ValueError): b.credentials(path)

    def test_dns_cannot_send_host_credentials_to_private_network(self):
        with patch.object(b.socket, 'getaddrinfo', return_value=[(socket.AF_INET, socket.SOCK_STREAM, 6, '', ('127.0.0.1', 443))]), \
             patch.object(b.socket, 'socket') as connect:
            with self.assertRaises(OSError): b.GitHub().connect()
            connect.assert_not_called()

    def test_systemd_acl_copy_is_distinct_from_original_store(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'credential'; path.write_text('{}')
            for mode, allowed in [(0o440, True), (0o400, True), (0o444, False), (0o460, False), (0o640, False)]:
                info = SimpleNamespace(st_mode=0o100000 | mode, st_uid=0, st_size=2)
                with patch.object(b.os, 'fstat', return_value=info):
                    if allowed: self.assertEqual(b.credentials(path, systemd_copy=True), {})
                    else:
                        with self.assertRaises(ValueError): b.credentials(path, systemd_copy=True)
            info = SimpleNamespace(st_mode=0o100440, st_uid=os.getuid(), st_size=2)
            with patch.object(b.os, 'fstat', return_value=info), self.assertRaises(ValueError):
                b.credentials(path)


class Backend(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass

    def serve(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        self.server.requests.append((self.command, self.path, dict(self.headers), body))
        if self.server.redirect:
            self.send_response(302)
            self.send_header('Location', 'http://169.254.169.254/stolen')
            self.send_header('Set-Cookie', 'secret=cookie')
            self.end_headers(); self.wfile.write(b'synthetic_secret'); return
        path, _, query = self.path.partition('?')
        env = {**self.server.env, 'GIT_PROJECT_ROOT': str(self.server.root), 'GIT_HTTP_EXPORT_ALL': '1',
               'PATH_INFO': path, 'QUERY_STRING': query, 'REQUEST_METHOD': self.command,
               'CONTENT_TYPE': self.headers.get('Content-Type', ''), 'CONTENT_LENGTH': str(len(body)),
               'REMOTE_USER': 'fixture', 'HTTP_GIT_PROTOCOL': self.headers.get('Git-Protocol', ''),
               'HTTP_CONTENT_ENCODING': self.headers.get('Content-Encoding', '')}
        result = subprocess.run(['git', 'http-backend'], env=env, input=body, capture_output=True, timeout=30)
        head, payload = result.stdout.split(b'\r\n\r\n', 1)
        headers = dict(line.decode().split(': ', 1) for line in head.split(b'\r\n'))
        self.send_response(int(headers.pop('Status', '200 OK').split()[0]))
        for name, value in headers.items(): self.send_header(name, value)
        self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)

    do_GET = do_POST = serve


class RealGit(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_') and 'proxy' not in k.lower()}
        self.env.update(HOME=str(self.root), GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null', GIT_TERMINAL_PROMPT='0')
        seed = self.root / 'seed'; self.git('init', '-q', '-b', 'main', str(seed))
        self.git('-C', str(seed), 'config', 'user.name', 'Fixture')
        self.git('-C', str(seed), 'config', 'user.email', 'fixture@example.invalid')
        (seed / 'file').write_text('original\n')
        self.git('-C', str(seed), 'add', 'file'); self.git('-C', str(seed), 'commit', '-qm', 'seed')
        for repo in ('org/read', 'org/write', 'org/upstream', 'fork/project', 'org/denied'):
            target = self.root / (repo + '.git'); target.parent.mkdir(parents=True, exist_ok=True)
            self.git('clone', '-q', '--bare', str(seed), str(target))
            self.git('-C', str(target), 'config', 'http.receivepack', 'true')
        self.cell = self.root / 'cell.json'
        self.cell.write_text(json.dumps(declaration({'repo': 'org/read', 'write': False},
            {'repo': 'org/write', 'write': True}, {'repo': 'org/upstream', 'via': 'fork/project', 'write': True})))
        self.backend = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Backend)
        self.backend.root, self.backend.env = self.root, self.env
        self.backend.requests, self.backend.redirect = [], False
        self.start(self.backend)
        tokens = {repo: 'synthetic_host_token' for repo in ('org/read', 'org/write', 'fork/project')}
        self.broker = b.Server(('127.0.0.1', 0), self.cell, tokens,
                               upstream=lambda: http.client.HTTPConnection(*self.backend.server_address, timeout=10))
        self.start(self.broker)
        self.base = 'http://127.0.0.1:' + str(self.broker.server_port)

    def start(self, server):
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        def stop(): server.shutdown(); server.server_close(); thread.join()
        self.addCleanup(stop)

    def git(self, *args, ok=True):
        result = subprocess.run(['git', *args], env=self.env, capture_output=True, timeout=30)
        if ok: self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result

    def request(self, method, path, body=None, headers=None):
        c = http.client.HTTPConnection(*self.broker.server_address, timeout=10)
        c.request(method, path, body=body, headers=headers or {})
        r = c.getresponse(); result = (r.status, dict(r.getheaders()), r.read()); c.close(); return result

    def clone(self, repo):
        path = self.root / ('clone-' + repo.replace('/', '-'))
        self.git('clone', '-q', self.base + '/' + repo + '.git', str(path))
        self.git('-C', str(path), 'config', 'user.name', 'Fixture')
        self.git('-C', str(path), 'config', 'user.email', 'fixture@example.invalid')
        return path

    def commit(self, path):
        (path / 'file').write_bytes(os.urandom(256 * 1024))
        self.git('-C', str(path), 'add', 'file'); self.git('-C', str(path), 'commit', '-qm', 'change')

    def test_real_clone_fetch_and_readonly_push_refusal(self):
        path = self.clone('org/read'); self.git('-C', str(path), 'fetch')
        before = self.git('-C', str(self.root / 'org/read.git'), 'rev-parse', 'HEAD').stdout
        self.commit(path)
        result = self.git('-C', str(path), 'push', 'origin', 'main', ok=False)
        self.assertNotEqual(result.returncode, 0); self.assertIn(b'403', result.stderr)
        self.assertEqual(before, self.git('-C', str(self.root / 'org/read.git'), 'rev-parse', 'HEAD').stdout)
        self.assertFalse(any('receive-pack' in r[1] for r in self.backend.requests))

    def test_real_chunked_push_and_fork_scope(self):
        for repo in ('org/write', 'fork/project'):
            path = self.clone(repo); self.commit(path)
            self.git('-C', str(path), '-c', 'http.postBuffer=1024', 'push', 'origin', 'main')
            self.assertEqual(self.git('-C', str(path), 'rev-parse', 'HEAD').stdout,
                             self.git('-C', str(self.root / (repo + '.git')), 'rev-parse', 'HEAD').stdout)
        count = len(self.backend.requests)
        for repo in ('org/upstream', 'org/denied'):
            self.assertNotEqual(self.git('ls-remote', self.base + '/' + repo + '.git', ok=False).returncode, 0)
        self.assertEqual(len(self.backend.requests), count)

    def test_credentials_and_client_headers_never_cross_scope(self):
        status, _, body = self.request('GET', '/org/read.git/info/refs?service=git-upload-pack', headers={
            'Authorization': 'Bearer attacker', 'Cookie': 'untrusted', 'Host': 'attacker.invalid',
            'X-Forwarded-Host': 'attacker.invalid'})
        self.assertEqual(status, 200)
        headers = self.backend.requests[-1][2]
        self.assertEqual(headers['Authorization'], 'Basic ' + base64.b64encode(b'x-access-token:synthetic_host_token').decode())
        for name in ('Cookie', 'X-Forwarded-Host'): self.assertNotIn(name, headers)
        self.assertNotIn(b'synthetic_host_token', body)
        self.broker.tokens.clear()
        self.assertEqual(self.request('GET', '/org/read.git/info/refs?service=git-upload-pack')[0], 200)
        self.assertNotIn('Authorization', self.backend.requests[-1][2])
        self.assertEqual(self.request('GET', '/org/write.git/info/refs?service=git-receive-pack')[0], 503)

    def test_compressed_git_negotiation_keeps_scopes_and_protocol(self):
        self.clone('org/read')
        _, path, headers, body = next(r for r in self.backend.requests if r[0] == 'POST')
        request_headers = {'Content-Type': headers['Content-Type'], 'Content-Encoding': 'gzip'}
        if 'Git-Protocol' in headers: request_headers['Git-Protocol'] = headers['Git-Protocol']
        status, _, payload = self.request('POST', path, gzip.compress(body), request_headers)
        self.assertEqual(status, 200); self.assertTrue(payload)
        self.assertEqual(self.backend.requests[-1][2]['Content-Encoding'], 'gzip')
        count = len(self.backend.requests)
        self.assertEqual(self.request('POST', '/org/denied.git/git-upload-pack', gzip.compress(body), request_headers)[0], 403)
        self.assertEqual(count, len(self.backend.requests))

    def test_revocation_redirects_and_bad_framing(self):
        self.backend.redirect = True
        status, headers, body = self.request('GET', '/org/read.git/info/refs?service=git-upload-pack')
        self.assertEqual(status, 502); self.assertEqual(body, b'')
        self.assertNotIn('Location', headers); self.assertNotIn('Set-Cookie', headers)
        self.backend.redirect = False
        count = len(self.backend.requests)
        status, _, _ = self.request('POST', '/org/write.git/git-receive-pack', body=b'', headers={
            'Content-Type': 'application/x-git-receive-pack-request',
            'Content-Length': '0', 'Transfer-Encoding': 'chunked'})
        self.assertEqual(status, 400); self.assertEqual(count, len(self.backend.requests))
        self.cell.write_text(json.dumps(declaration()))
        self.assertEqual(self.request('GET', '/org/read.git/info/refs?service=git-upload-pack')[0], 403)
        self.assertEqual(count, len(self.backend.requests))


if __name__ == '__main__': unittest.main()
