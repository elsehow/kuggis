#!/usr/bin/python3
"""Host-only relay. A guest can request a repo token, never choose a cell or scope."""
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import re
import socketserver
import sys
from github_common import Failure, broker_url, enable_network, repository, request
import host

CONNECTION = host.ETC / 'github-connection.json'
SOCKET_DIR = Path('/run/kuggis-github')


def connection():
    value = json.loads(host.trusted_bytes(CONNECTION, secret=True))
    if (set(value) not in ({'url', 'cell', 'credential'}, {'url', 'cell', 'credential', 'transport'})
            or value.get('transport', 'github') not in ('github', 'ssh-gateway')
            or not re.fullmatch('[a-f0-9]{32}', value['cell'])
            or not re.fullmatch('[A-Za-z0-9_-]{64}', value['credential'])):
        raise Failure('invalid host GitHub connection')
    broker_url(value['url'])
    return value


def status():
    if not CONNECTION.exists():
        return {'connected': False}
    value = connection()
    return {'connected': True, 'url': value['url'], 'cell': value['cell'], 'transport': value.get('transport', 'github')}


def attach(value):
    # An existing binding is immutable; retries reuse it, never impersonate another cell.
    if CONNECTION.exists() and connection() != value:
        raise Failure('host already has a different GitHub identity')
    if (set(value) not in ({'url', 'cell', 'credential'}, {'url', 'cell', 'credential', 'transport'})
            or value.get('transport', 'github') not in ('github', 'ssh-gateway')
            or not re.fullmatch('[a-f0-9]{32}', value['cell'])
            or not re.fullmatch('[A-Za-z0-9_-]{64}', value['credential'])):
        raise Failure('invalid enrollment')
    broker_url(value['url'])
    workspace = enable_network(host.load(), value['url'], value.get('transport', 'github'))
    # Require the new socket mount before changing host configuration.
    host.guest('/usr/bin/test', '-d', '/run/kuggis-github')
    host.atomic_write(CONNECTION, json.dumps(value).encode(), 0o600)
    host.atomic_write(host.CONFIG, json.dumps(workspace).encode())
    host.run(host.RUNTIME / 'kuggis', 'validate', host.CONFIG)
    host.run('systemctl', 'restart', 'kuggis-v2-proxy.service', 'kuggis-v2-github.service')
    host.guest('/usr/bin/python3', '/run/kuggis-runtime/github_guest.py', 'configure',
               value.get('transport', 'github'), user='root')
    return status()


def control(args):
    if args == ['status']:
        result = status()
    elif args == ['attach']:
        data = sys.stdin.buffer.read(8193)
        if len(data) > 8192:
            raise Failure('enrollment too large')
        result = attach(json.loads(data))
    else:
        raise Failure('unknown GitHub host operation')
    print(json.dumps(result))


class Relay(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(35)

    def do_POST(self):
        status_code = 200
        try:
            size = int(self.headers.get('Content-Length', '-1'))
            if (self.path not in ('/token', '/api') or not 0 < size <= (65536 if self.path == '/api' else 512)
                    or self.headers.get('Transfer-Encoding')):
                raise Failure('invalid request')
            body = json.loads(self.rfile.read(size))
            expected = {'repo'} if self.path == '/token' else {'repo', 'kind', 'action', 'params'}
            if not isinstance(body, dict) or set(body) != expected:
                raise Failure('invalid request fields; cell identity is host-owned')
            repo = repository(body['repo'])
            value = connection()
            if self.path == '/api':
                result = request(value['url'] + '/api', value['credential'], {**body, 'repo': repo},
                                 trusted_errors=True, timeout=150)
            else:
                result = request(value['url'] + '/token', value['credential'], {'repo': repo})
            if self.path == '/token' and result.get('transport', 'github') != value.get('transport', 'github'):
                raise Failure('broker transport differs from the host binding')
            if self.path == '/token' and result.get('transport') == 'ssh-gateway':
                result['url'] = value['url']
        except Failure as error:
            status_code, result = error.status, {'error': str(error)}
        except Exception:
            status_code, result = 503, {'error': 'GitHub credential service unavailable'}
        data = json.dumps(result).encode()
        try:
            self.send_response(status_code)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass


def serve():
    connection()
    host.safe_directory(SOCKET_DIR)
    path = SOCKET_DIR / 'token.sock'
    path.unlink(missing_ok=True)
    server = socketserver.UnixStreamServer(str(path), Relay)
    os.chown(path, 524288 + 1000, 524288 + 1000)
    os.chmod(path, 0o600)
    server.serve_forever()


if __name__ == '__main__':
    try:
        if sys.argv[1:] != ['serve']:
            raise Failure('expected serve')
        serve()
    except Exception:
        print('kuggis GitHub relay failed', file=sys.stderr)
        sys.exit(2)
