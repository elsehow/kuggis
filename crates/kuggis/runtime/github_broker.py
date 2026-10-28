"""GitHub broker: SSH gateway or App tokens, with per-cell repository grants.

Run outside guests. Only the operator identity can change grants. SQLite write
transactions serialize issuance with policy changes, including across processes.
"""
import argparse
import base64
from contextlib import contextmanager
import datetime
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import secrets
import sqlite3
import ssl
import socketserver
import subprocess
import sys
import time
import threading
import uuid
from github_common import Failure, permissions, private_read, repository, request
from github_ssh import SSHGit
from github_gateway import serve_git
from github_api import API_LIMIT, GitHubAPI, api_permissions, execute as api_execute, plan as api_plan


def expiry(value):
    try:
        return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()
    except (ValueError, TypeError, AttributeError):
        raise Failure('invalid token expiry from GitHub', 502) from None


class GitHub:
    def __init__(self, app_id, key):
        self.app_id, self.key = str(app_id), key

    def jwt(self):
        def b64(data):
            return base64.urlsafe_b64encode(data).rstrip(b'=')
        now = int(time.time())
        data = b'.'.join(b64(json.dumps(part, separators=(',', ':')).encode()) for part in (
            {'alg': 'RS256', 'typ': 'JWT'}, {'iat': now - 60, 'exp': now + 540, 'iss': self.app_id}))
        private_read(self.key)
        signed = subprocess.run(['openssl', 'dgst', '-sha256', '-sign', self.key],
                                input=data, capture_output=True, timeout=10)
        if signed.returncode:
            raise Failure('GitHub App signing failed', 502)
        return (data + b'.' + b64(signed.stdout)).decode()

    def api(self, path, token, data=None, method='POST'):
        return request('https://api.github.com' + path, token, data, method)

    def resolve(self, repo, scope):
        installation = self.api('/repos/' + repo + '/installation', self.jwt(), method='GET')['id']
        probe = self.api('/app/installations/%d/access_tokens' % installation, self.jwt(),
                         {'repositories': [repo.split('/')[1]], 'permissions': scope})
        try:
            repos = probe.get('repositories', [])
            actual = {k: v for k, v in probe.get('permissions', {}).items() if k != 'metadata'}
            if (len(repos) != 1 or repos[0]['full_name'].lower() != repo or actual != scope
                    or probe.get('permissions', {}).get('metadata') not in (None, 'read')):
                raise Failure('GitHub did not confirm the exact repository scope', 502)
            return {'repository_id': repos[0]['id'], 'installation_id': installation}
        finally:
            self.revoke(probe['token'])

    def mint(self, grant):
        result = self.api('/app/installations/%d/access_tokens' % grant['installation_id'], self.jwt(),
            {'repository_ids': [grant['repository_id']], 'permissions': grant['permissions']})
        try:
            repos = result.get('repositories', [])
            actual = {k: v for k, v in result.get('permissions', {}).items() if k != 'metadata'}
            if (len(repos) != 1 or repos[0]['id'] != grant['repository_id']
                    or actual != grant['permissions']
                    or result.get('permissions', {}).get('metadata') not in (None, 'read')):
                raise Failure('GitHub token scope differs from the grant', 502)
            expires = expiry(result['expires_at'])
            if not time.time() < expires <= time.time() + 3660:
                raise Failure('unexpected GitHub token lifetime', 502)
            return result['token'], expires
        except Exception:
            self.revoke(result['token'])
            raise

    def revoke(self, token):
        try:
            self.api('/installation/token', token, method='DELETE')
        except Failure as error:
            if error.status != 401:
                raise


class Broker:
    def __init__(self, database, github, admin_token, api=None):
        self.database, self.github, self.admin = str(database), github, admin_token
        self.api = api
        self.transport = getattr(github, 'transport', 'github')
        self.permissions = getattr(github, 'permissions', permissions)
        if len(admin_token) < 32:
            raise Failure('admin token must have at least 32 characters')
        with self.db() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS cells (id TEXT PRIMARY KEY, label TEXT NOT NULL, secret TEXT UNIQUE NOT NULL);
                CREATE TABLE IF NOT EXISTS grants (cell TEXT NOT NULL, repo TEXT NOT NULL, value TEXT NOT NULL,
                    PRIMARY KEY(cell, repo));
                CREATE TABLE IF NOT EXISTS tokens (token TEXT PRIMARY KEY, cell TEXT NOT NULL, repo TEXT NOT NULL,
                    expires REAL NOT NULL, pending INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS gateway_tokens (digest TEXT PRIMARY KEY, cell TEXT NOT NULL,
                    repo TEXT NOT NULL, expires REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            ''')
            db.execute('BEGIN IMMEDIATE')
            mode = db.execute("SELECT value FROM metadata WHERE key='transport'").fetchone()
            existing = db.execute('SELECT 1 FROM grants UNION SELECT 1 FROM tokens LIMIT 1').fetchone()
            if ((mode and mode['value'] != self.transport)
                    or (not mode and existing and self.transport != 'github')):
                raise Failure('broker database belongs to another transport; use a separate database and re-enroll cells')
            db.execute("INSERT OR IGNORE INTO metadata VALUES ('transport', ?)", (self.transport,))
            db.commit()

    @contextmanager
    def db(self):
        db = sqlite3.connect(self.database, timeout=60, isolation_level=None)
        db.row_factory = sqlite3.Row
        db.execute('PRAGMA synchronous=FULL')
        try:
            yield db
        finally:
            db.close()

    def admin_check(self, token):
        if not hmac.compare_digest(token, self.admin):
            raise Failure('unauthorized', 401)

    def authenticate(self, db, token):
        digest = hashlib.sha256(token.encode()).hexdigest()
        row = db.execute('SELECT id FROM cells WHERE secret=?', (digest,)).fetchone()
        if row is None:
            raise Failure('unauthorized', 401)
        return row['id']

    def enroll(self, label):
        if not isinstance(label, str) or not 1 <= len(label) <= 200 or any(ord(c) < 32 for c in label):
            raise Failure('invalid cell label')
        identity, secret = uuid.uuid4().hex, secrets.token_urlsafe(48)
        with self.db() as db:
            db.execute('INSERT INTO cells VALUES (?, ?, ?)',
                       (identity, label, hashlib.sha256(secret.encode()).hexdigest()))
        value = {'cell': identity, 'credential': secret}
        if self.transport != 'github':
            value['transport'] = self.transport
        return value

    def listing(self, cell):
        with self.db() as db:
            self.check_cell(db, cell)
            return {'cell': cell, 'repositories': [json.loads(r['value']) for r in
                    db.execute('SELECT value FROM grants WHERE cell=? ORDER BY repo', (cell,))],
                    'pending_revocations': db.execute(
                        'SELECT count(*) FROM tokens WHERE cell=? AND pending=1 AND expires>?',
                        (cell, time.time())).fetchone()[0]}

    @staticmethod
    def check_cell(db, cell):
        if not isinstance(cell, str) or not db.execute('SELECT 1 FROM cells WHERE id=?', (cell,)).fetchone():
            raise Failure('unknown cell', 404)

    def drain(self, db, cell, repo):
        db.execute('DELETE FROM tokens WHERE expires<=?', (time.time(),))
        failed = False
        for row in db.execute('SELECT token FROM tokens WHERE cell=? AND repo=? AND pending=1', (cell, repo)).fetchall():
            try:
                self.github.revoke(row['token'])
            except Exception:
                failed = True
            else:
                db.execute('DELETE FROM tokens WHERE token=?', (row['token'],))
        return not failed

    def change_role(self, cell, repo, role):
        if not isinstance(role, str) or role not in ('read', 'develop'):
            raise Failure('role must be read or develop')
        if self.transport != 'ssh-gateway':
            raise Failure('repository roles require the SSH gateway backend')
        level = 'write' if role == 'develop' else 'read'
        return self.change(cell, repo, level, api_scope=api_permissions(level, level, False))

    def change(self, cell, repo, access=None, workflows=False, *, api_scope=None):
        repo = repository(repo)
        scope = self.permissions(access, workflows) if access is not None else None
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            try:
                self.check_cell(db, cell)
                previous = db.execute('SELECT value FROM grants WHERE cell=? AND repo=?', (cell, repo)).fetchone()
                # Preserve numeric identity on updates, including offline downgrades.
                resolved = ({k: json.loads(previous['value'])[k] for k in getattr(
                                self.github, 'identity_fields', ('repository_id', 'installation_id'))}
                            if previous else self.github.resolve(repo, scope) if scope else None)
                if previous and scope:
                    old_scope = json.loads(previous['value'])['permissions']
                    if any(key not in old_scope or (level == 'write' and old_scope[key] != 'write')
                           for key, level in scope.items()):
                        if self.github.resolve(repo, scope) != resolved:
                            raise Failure('repository identity changed; remove the old grant before adding a replacement')
                value = {'repo': repo, 'permissions': scope, **resolved} if scope else None
                if value and previous:
                    for key in ('api_permissions', 'api_repository_id'):
                        old = json.loads(previous['value'])
                        if key in old:
                            value[key] = old[key]
                if value and api_scope is not None:
                    value['api_permissions'] = api_scope
                if (json.loads(previous['value']) if previous else None) != value:
                    db.execute('DELETE FROM grants WHERE cell=? AND repo=?', (cell, repo))
                    if value:
                        db.execute('INSERT INTO grants VALUES (?, ?, ?)', (cell, repo, json.dumps(value)))
                    db.execute('UPDATE tokens SET pending=1 WHERE cell=? AND repo=?', (cell, repo))
                    db.execute('DELETE FROM gateway_tokens WHERE cell=? AND repo=?', (cell, repo))
                db.commit()  # Persist reduced authority before attempting remote revocation.
            except BaseException:
                db.rollback()
                raise
            db.execute('BEGIN IMMEDIATE')
            try:
                complete = self.drain(db, cell, repo)
                db.commit()
            except BaseException:
                db.rollback()
                raise
        if not complete:
            raise Failure('grant saved; revocation incomplete; retry the same operation (tokens expire within one hour)', 503)
        return self.listing(cell)

    def issue(self, credential, repo):
        repo = repository(repo)
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            try:
                cell = self.authenticate(db, credential)
                if not self.drain(db, cell, repo):
                    db.commit()
                    raise Failure('revocation pending; token issuance is blocked', 503)
                row = db.execute('SELECT value FROM grants WHERE cell=? AND repo=?', (cell, repo)).fetchone()
                if row is None:
                    db.commit()
                    raise Failure('repository access is not granted to this cell', 403)
                if self.transport == 'ssh-gateway':
                    token, expires = 'kg_' + secrets.token_urlsafe(48), time.time() + 900
                    db.execute('DELETE FROM gateway_tokens WHERE expires<=?', (time.time(),))
                    db.execute('INSERT INTO gateway_tokens VALUES (?, ?, ?, ?)',
                               (hashlib.sha256(token.encode()).hexdigest(), cell, repo, expires))
                    db.commit()
                    return {'token': token, 'expires_at': expires, 'transport': self.transport}
                cached = db.execute('SELECT token, expires FROM tokens WHERE cell=? AND repo=? AND pending=0 AND expires>?',
                                    (cell, repo, time.time() + 120)).fetchone()
                if cached:
                    token, expires = cached['token'], cached['expires']
                else:
                    token, expires = self.github.mint(json.loads(row['value']))
                    db.execute('INSERT INTO tokens VALUES (?, ?, ?, ?, 0)', (token, cell, repo, expires))
                db.commit()  # No guest receives a token before its revocation record is durable.
                return {'token': token, 'expires_at': expires}
            except BaseException:
                if db.in_transaction:
                    db.rollback()
                raise

    def git_grant(self, token, repo, service):
        if self.transport != 'ssh-gateway' or service not in ('git-upload-pack', 'git-receive-pack'):
            raise Failure('unsupported Git transport or service', 403)
        repo = repository(repo)
        with self.db() as db:
            row = db.execute('''SELECT t.cell, g.value FROM gateway_tokens t JOIN grants g
                ON t.cell=g.cell AND t.repo=g.repo WHERE t.digest=? AND t.repo=? AND t.expires>?''',
                (hashlib.sha256(token.encode()).hexdigest(), repo, time.time())).fetchone()
        if row is None:
            raise Failure('Git credential expired, revoked, or not granted for this repository', 403)
        grant = json.loads(row['value'])
        if service == 'git-receive-pack' and grant['permissions']['git'] != 'write':
            raise Failure('this cell has read-only access to this repository', 403)
        return row['cell'], grant

    def change_api(self, cell, repo, issues, pulls, merge):
        if self.transport != 'ssh-gateway':
            raise Failure('separate API grants apply to the SSH gateway backend')
        repo = repository(repo)
        scope = api_permissions(issues, pulls, merge)
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            self.check_cell(db, cell)
            row = db.execute('SELECT value FROM grants WHERE cell=? AND repo=?', (cell, repo)).fetchone()
            if row is None:
                raise Failure('add the repository to this cell before setting API permissions', 404)
            grant = json.loads(row['value'])
            grant['api_permissions'] = scope
            db.execute('UPDATE grants SET value=? WHERE cell=? AND repo=?', (json.dumps(grant), cell, repo))
            db.commit()
        return self.listing(cell)

    def api_operation(self, credential, body):
        if set(body) != {'repo', 'kind', 'action', 'params'}:
            raise Failure('API requests accept only repo, kind, action, and params')
        repo = repository(body['repo'])
        kind, action, params = body['kind'], body['action'], body['params']
        level = api_plan(kind, action, params)
        with self.db() as db:
            # Policy changes serialize with API operations. A completed upstream write
            # cannot be rolled back; never automatically retry an ambiguous request.
            db.execute('BEGIN IMMEDIATE')
            cell = self.authenticate(db, credential)
            def grant_for(name):
                row = db.execute('SELECT value FROM grants WHERE cell=? AND repo=?', (cell, name)).fetchone()
                if row is None:
                    raise Failure('repository is not granted to this cell', 403)
                return json.loads(row['value'])
            grant = grant_for(repo)
            scope = grant.get('api_permissions', {})
            allowed = scope.get('issues' if kind == 'issue' else 'pull_requests', 'none')
            if allowed == 'none' or (level == 'write' and allowed != 'write'):
                raise Failure('this cell lacks the required PR/issue API permission for this repository', 403)
            if action == 'merge' and not scope.get('merge', False):
                raise Failure('PR merging is not granted to this cell', 403)
            if self.api is None:
                raise Failure('GitHub API provider is not configured on the broker', 503)
            def resolve(name, call):
                current = grant_for(name)  # Forks also need a grant to this cell.
                metadata = call('/repos/' + name)
                identity = metadata.get('id')
                if (type(identity) is not int or identity <= 0 or metadata.get('full_name', '').lower() != name
                        or current.get('api_repository_id', identity) != identity):
                    raise Failure('API repository identity changed; operator must remove and re-add the grant', 403)
                if 'api_repository_id' not in current:
                    current['api_repository_id'] = identity
                    db.execute('UPDATE grants SET value=? WHERE cell=? AND repo=?', (json.dumps(current), cell, name))
                return metadata
            result = api_execute(self.api.session(), repo, kind, action, params, resolve)
            db.commit()
        return {'result': result}

    def handle(self, path, auth, body):
        if not isinstance(body, dict):
            raise Failure('expected an object')
        if path == '/token':
            if set(body) != {'repo'}:
                raise Failure('token requests accept only repo; identity and permissions are server-owned')
            return self.issue(auth, body['repo'])
        if path == '/api':
            return self.api_operation(auth, body)
        self.admin_check(auth)
        if path == '/admin/status' and not body:
            return {'ok': True, 'transport': self.transport, 'api_configured': self.api is not None}
        if path == '/admin/api/status' and not body:
            if self.api is None:
                raise Failure('GitHub API provider is not configured on the broker', 503)
            return self.api.status()
        if path == '/admin/cells' and set(body) == {'label'}:
            return self.enroll(body['label'])
        if path == '/admin/repos/list' and set(body) == {'cell'}:
            return self.listing(body['cell'])
        if path == '/admin/repos/add' and set(body) == {'cell', 'repo', 'access', 'workflows'}:
            return self.change(body['cell'], body['repo'], body['access'], body['workflows'])
        if path == '/admin/repos/add' and set(body) == {'cell', 'repo', 'role'}:
            return self.change_role(body['cell'], body['repo'], body['role'])
        if path == '/admin/repos/remove' and set(body) == {'cell', 'repo'}:
            return self.change(body['cell'], body['repo'])
        if path == '/admin/repos/api' and set(body) == {'cell', 'repo', 'issues', 'pull_requests', 'merge'}:
            return self.change_api(body['cell'], body['repo'], body['issues'], body['pull_requests'], body['merge'])
        if path == '/admin/resolve' and set(body) == {'repo', 'access', 'workflows'}:
            return self.github.resolve(repository(body['repo']), self.permissions(body['access'], body['workflows']))
        raise Failure('unknown operation or fields', 404)


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def do_GET(self):
        serve_git(self)

    def log_message(self, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(35)

    def do_POST(self):
        status = 200
        try:
            size = int(self.headers.get('Content-Length', '-1'))
            if not 0 <= size <= (API_LIMIT if self.path == '/api' else 8192) or self.headers.get('Transfer-Encoding'):
                raise Failure('invalid request framing')
            auth = self.headers.get('Authorization', '')
            if not auth.startswith('Bearer ') or len(auth) > 1024:
                raise Failure('unauthorized', 401)
            result = self.server.broker.handle(self.path, auth[7:], json.loads(self.rfile.read(size)))
        except Failure as error:
            status, result = error.status, {'error': str(error)}
        except Exception:
            status, result = 500, {'error': 'GitHub broker operation failed'}
        data = json.dumps(result).encode()
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass  # Issuance/revocation remains recorded when a client disconnects.


class Server(socketserver.ThreadingMixIn, HTTPServer):
    daemon_threads = True

    def __init__(self, address, context, broker):
        self.context, self.broker = context, broker
        self.slots = threading.BoundedSemaphore(32)
        self.git_slots = threading.BoundedSemaphore(16)
        self.git_cells = {}
        self.git_lock = threading.Lock()
        super().__init__(address, Handler)

    def get_request(self):
        sock, address = super().get_request()
        sock.settimeout(5)
        try:
            return self.context.wrap_socket(sock, server_side=True), address
        except Exception:
            sock.close()
            raise

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    args = parser.parse_args()
    config = json.loads(private_read(args.config))
    required = {'admin_token_file', 'database', 'tls_cert', 'tls_key', 'listen', 'port'}
    if config.get('transport') == 'ssh-gateway':
        required |= {'transport', 'ssh_key_file', 'known_hosts_file'}
    else:
        required |= {'app_id', 'private_key_file'}
    optional = {'gh_config_dir'} if config.get('transport') == 'ssh-gateway' else set()
    if set(config) - optional != required:
        raise Failure('broker configuration has missing or unknown fields')
    os.umask(0o077)
    database = Path(config['database'])
    database.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    for path in (database.parent, database) if database.exists() else (database.parent,):
        info = path.lstat()
        if path.is_symlink() or info.st_uid != os.geteuid() or info.st_mode & 0o077:
            raise Failure('broker database and directory must be owned and private')
    private_read(config['tls_key'])
    if config.get('transport') == 'ssh-gateway':
        backend = SSHGit(config['ssh_key_file'], config['known_hosts_file'])
    else:
        private_read(config['private_key_file'])
        backend = GitHub(config['app_id'], config['private_key_file'])
    broker = Broker(database, backend,
                    private_read(config['admin_token_file']).decode().strip(),
                    GitHubAPI(config['gh_config_dir']) if 'gh_config_dir' in config else None)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(config['tls_cert'], config['tls_key'])
    server = Server((config['listen'], config['port']), context, broker)
    server.serve_forever()


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print('kuggis github broker: ' + (str(error) if isinstance(error, Failure) else 'startup failed'), file=sys.stderr)
        sys.exit(2)
