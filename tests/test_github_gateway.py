"""Real Git pack transfers through TLS gateway and Unix host relay, without GitHub credentials."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import select
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
from test_runtime import ROOT

RUNTIME = ROOT / 'crates/kuggis/runtime'
sys.path.insert(0, str(RUNTIME))
import github_broker as b
import github_common as c
import github_guest as g
import github_git as git
import github_host as h
from github_ssh import SSHGit
from github_wire import Wire


class LocalGit(SSHGit):
    """Replace only upstream SSH with real local Git service processes."""
    def __init__(self, root):
        self.root = root
        self.started = []

    def resolve(self, repo, scope):
        if not (self.root / (repo + '.git')).is_dir():
            raise c.Failure('unknown local repository')
        return {'upstream_repo': repo}

    def open(self, repo, service):
        self.started.append((repo, service))
        return subprocess.Popen(['/usr/bin/git', service.removeprefix('git-'), str(self.root / (repo + '.git'))],
                                env=self.environment(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, start_new_session=True)


class Gateway(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'owner').mkdir()
        self.environment = {**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                            'GIT_CONFIG_SYSTEM': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
                            'NO_PROXY': 'localhost,127.0.0.1', 'no_proxy': 'localhost,127.0.0.1'}
        for name in ('a', 'b'):
            self.run_git('init', '--bare', str(self.root / 'owner' / (name + '.git')))
        self.backend = LocalGit(self.root)
        self.broker = b.Broker(self.root / 'broker.db', self.backend, 'a' * 64)
        self.fri = self.broker.enroll('fri')
        self.merri = self.broker.enroll('merri')
        self.broker.change(self.fri['cell'], 'owner/a', 'write')
        self.broker.change(self.fri['cell'], 'owner/b', 'read')
        self.broker.change(self.merri['cell'], 'owner/a', 'read')
        certificate, key = self.root / 'cert.pem', self.root / 'key.pem'
        config = self.root / 'openssl.cnf'
        config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3\n'
            '[dn]\nCN=localhost\n[v3]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n'
            'basicConstraints=critical,CA:TRUE\nkeyUsage=digitalSignature,keyEncipherment,keyCertSign\n')
        subprocess.run(['/usr/bin/openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-x509', '-days', '1',
                        '-config', str(config), '-keyout', str(key), '-out', str(certificate)],
                       check=True, capture_output=True)
        self.environment['SSL_CERT_FILE'] = str(certificate)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        self.server = b.Server(('127.0.0.1', 0), context, self.broker)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = 'https://localhost:%d' % self.server.server_address[1]
        self.binding = {**self.fri, 'url': self.url}
        connection_patch = patch.object(h, 'connection', side_effect=lambda: self.binding.copy())
        connection_patch.start()
        self.addCleanup(connection_patch.stop)
        env_patch = patch.dict(os.environ, {'SSL_CERT_FILE': str(certificate), 'NO_PROXY': 'localhost,127.0.0.1'})
        env_patch.start()
        self.addCleanup(env_patch.stop)
        self.socket = self.root / 'relay.sock'
        self.relay = socketserver.UnixStreamServer(str(self.socket), h.Relay)
        relay_thread = threading.Thread(target=self.relay.serve_forever, daemon=True)
        relay_thread.start()
        self.addCleanup(relay_thread.join)
        self.addCleanup(self.relay.server_close)
        self.addCleanup(self.relay.shutdown)
        binaries = self.root / 'bin'
        binaries.mkdir()
        helper = binaries / 'git-remote-kuggis'
        helper.write_text('#!' + sys.executable + '\nimport sys\nsys.path.insert(0, ' + repr(str(RUNTIME)) + ')\n'
                          'import github_guest, github_git\ngithub_guest.SOCKET = ' + repr(str(self.socket)) + '\n'
                          'github_git.main(sys.argv[1:])\n')
        helper.chmod(0o755)
        self.environment['PATH'] = str(binaries) + ':/usr/bin:/bin'

    def run_git(self, *args, cwd=None, ok=True):
        result = subprocess.run(['/usr/bin/git', *args], cwd=cwd or self.root, env=self.environment,
                                capture_output=True, timeout=20)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr.decode())
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_clone_push_fetch_and_cell_scope(self):
        checkout = self.root / 'clone'
        self.run_git('clone', 'kuggis::owner/a.git', str(checkout))
        (checkout / 'hello').write_text('first\n')
        # Include enough incompressible data to cross several maximum-size frames.
        (checkout / 'payload').write_bytes(os.urandom(180000))
        self.run_git('add', '.', cwd=checkout)
        self.run_git('-c', 'user.name=Tester', '-c', 'user.email=test@example.com', 'commit', '-m', 'first', cwd=checkout)
        self.run_git('push', 'origin', 'HEAD:refs/heads/main', cwd=checkout)
        original = self.run_git('rev-parse', 'HEAD', cwd=checkout).stdout
        other = self.root / 'reader'
        self.binding = {**self.merri, 'url': self.url}
        self.run_git('clone', '-b', 'main', 'kuggis::owner/a.git', str(other))
        self.assertEqual((other / 'payload').read_bytes(), (checkout / 'payload').read_bytes())
        self.run_git('fetch', 'origin', cwd=other)
        started = len(self.backend.started)
        self.run_git('push', 'origin', 'HEAD:refs/heads/forbidden', cwd=other, ok=False)
        self.run_git('ls-remote', 'kuggis::owner/b.git', ok=False)
        self.assertEqual(len(self.backend.started), started)  # Denied before opening upstream.
        self.assertEqual(self.run_git('--git-dir', str(self.root / 'owner/a.git'), 'rev-parse', 'main').stdout, original)
        self.broker.change(self.merri['cell'], 'owner/a')
        self.run_git('fetch', 'origin', cwd=other, ok=False)
        self.binding = {**self.fri, 'url': self.url}
        self.run_git('fetch', 'origin', cwd=checkout)

    def test_revocation_expiry_restart_and_service_validation(self):
        issued = self.broker.issue(self.fri['credential'], 'owner/a')
        self.assertLessEqual(issued['expires_at'] - time.time(), 900)
        self.broker.git_grant(issued['token'], 'owner/a', 'git-receive-pack')
        for repo, service in [('owner/b', 'git-upload-pack'), ('owner/a', 'sh'), ('../a', 'git-upload-pack')]:
            with self.assertRaises(c.Failure):
                self.broker.git_grant(issued['token'], repo, service)
        with self.broker.db() as db:
            row = db.execute('SELECT digest FROM gateway_tokens').fetchone()
            self.assertEqual(row['digest'], hashlib.sha256(issued['token'].encode()).hexdigest())
        self.broker.change_role(self.fri['cell'], 'owner/a', 'read')
        restarted = b.Broker(self.root / 'broker.db', self.backend, 'a' * 64)
        grant = restarted.listing(self.fri['cell'])['repositories'][0]
        self.assertEqual(grant['permissions'], {'git': 'read'})
        self.assertEqual(grant['api_permissions'], {'issues': 'read', 'pull_requests': 'read', 'merge': False})
        with self.assertRaises(c.Failure):
            restarted.git_grant(issued['token'], 'owner/a', 'git-upload-pack')
        fresh = restarted.issue(self.fri['credential'], 'owner/a')
        with self.assertRaises(c.Failure):
            restarted.git_grant(fresh['token'], 'owner/a', 'git-receive-pack')
        self.broker.change_role(self.fri['cell'], 'owner/a', 'develop')
        writable = self.broker.issue(self.fri['credential'], 'owner/a')
        self.broker.git_grant(writable['token'], 'owner/a', 'git-receive-pack')
        self.broker.change_role(self.fri['cell'], 'owner/a', 'develop')
        self.broker.git_grant(writable['token'], 'owner/a', 'git-receive-pack')  # Idempotent, no revocation.
        with restarted.db() as db:
            db.execute('UPDATE gateway_tokens SET expires=0')
        with self.assertRaises(c.Failure):
            restarted.git_grant(fresh['token'], 'owner/a', 'git-upload-pack')
        with self.assertRaisesRegex(c.Failure, 'another transport'):
            b.Broker(self.root / 'broker.db', object(), 'a' * 64)

    def test_open_stream_closed_on_revocation(self):
        issued = self.broker.issue(self.fri['credential'], 'owner/a')
        conn, response, wire = git.connect(self.url, 'owner/a', 'git-upload-pack', issued['token'])
        try:
            self.assertTrue(wire.receive()[1])  # Initial ref advertisement; keep negotiation open.
            self.broker.change(self.fri['cell'], 'owner/a')
            conn.sock.settimeout(3)
            with self.assertRaises((EOFError, OSError)):
                while True:
                    wire.receive()
        finally:
            response.close()
            conn.close()

    def test_ssh_credential_never_becomes_github_api_token(self):
        with patch.object(g, 'SOCKET', str(self.socket)):
            with self.assertRaisesRegex(c.Failure, 'separate API authorization'):
                g.token('owner/a')
        with self.assertRaises(c.Failure):
            self.broker.change(self.fri['cell'], 'owner/a', 'write', workflows=True)

    def test_guest_configuration_normal_remotes_and_http_connect_proxy(self):
        # Exercise the actual guest configurator against an isolated Git config.
        config = self.root / 'gitconfig'
        env = {**self.environment, 'GIT_CONFIG_SYSTEM': str(config), 'GIT_CONFIG_NOSYSTEM': '0'}
        actual_run = subprocess.run
        def configured_run(argv, **kwargs):
            return actual_run(argv, env=env, cwd=self.root, **kwargs)
        with patch('os.geteuid', return_value=0), patch('pathlib.Path', side_effect=lambda path: self.root / Path(path).name), \
                patch.object(g.subprocess, 'run', side_effect=configured_run):
            g.configure('ssh-gateway')
            g.configure('ssh-gateway')  # Idempotent rewrite configuration.
        self.environment = env
        expected = 'localhost:%d' % self.server.server_address[1]
        targets = []
        class Proxy(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_CONNECT(self):
                targets.append(self.path)
                if self.path != expected:
                    self.send_error(403)
                    return
                with socket.create_connection(('127.0.0.1', int(expected.split(':')[1])), timeout=5) as upstream:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.flush()
                    for_streams = [upstream, self.connection]
                    while True:
                        readable, _, _ = select.select(for_streams, [], [], 5)
                        if not readable:
                            return
                        for stream in readable:
                            data = stream.recv(65536)
                            if not data:
                                return
                            (upstream if stream is self.connection else self.connection).sendall(data)
        proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        thread.start()
        try:
            self.environment.update(https_proxy='http://127.0.0.1:%d' % proxy.server_address[1],
                                    HTTPS_PROXY='http://127.0.0.1:%d' % proxy.server_address[1],
                                    NO_PROXY='', no_proxy='')
            for remote in ('https://github.com/owner/a.git', 'git@github.com:owner/a.git', 'ssh://git@github.com/owner/a.git'):
                self.run_git('ls-remote', remote)
            self.assertEqual(targets, [expected] * 3)
        finally:
            proxy.shutdown()
            proxy.server_close()
            thread.join()


class SSHBoundary(unittest.TestCase):
    def test_fixed_ssh_destination_and_no_agent_or_user_configuration(self):
        backend = object.__new__(SSHGit)
        backend.key_file, backend.known_hosts = '/protected/key', '/protected/known_hosts'
        argv = backend.command('Owner/Repo', 'git-upload-pack')
        self.assertEqual(argv[-2:], ['git@github.com', 'git-upload-pack owner/repo.git'])
        self.assertIn('IdentityAgent=none', argv)
        self.assertIn('ForwardAgent=no', argv)
        self.assertIn('StrictHostKeyChecking=yes', argv)
        self.assertEqual(argv[1:3], ['-F', '/dev/null'])
        for repo in ('owner/repo;id', '-o/ProxyCommand', 'owner/../repo', 'git@evil:repo'):
            with self.assertRaises(c.Failure):
                backend.command(repo, 'git-upload-pack')
        with self.assertRaises(c.Failure):
            backend.command('owner/repo', 'git-upload-archive')
        self.assertNotIn('SSH_AUTH_SOCK', backend.environment())

    def test_network_only_adds_gateway_and_framing_rejects_oversize_or_unmasked(self):
        workspace = c.enable_network({'resources': []}, 'https://broker.example', 'ssh-gateway')
        self.assertEqual(workspace['resources'][0]['endpoints'], [{'host': 'broker.example', 'port': 443}])
        for data in (b'\x82\x00', b'\x82\xff' + (65537).to_bytes(8, 'big'), b'\x02\x80'):
            with self.assertRaises(c.Failure):
                Wire(io.BytesIO(data), None).receive()


if __name__ == '__main__':
    unittest.main()
