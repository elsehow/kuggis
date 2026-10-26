"""Host-only, repository-scoped Git smart HTTP gateway, shared by all hosts.

The guest can request upload-pack or receive-pack, never an arbitrary URL,
API operation, header, credential, or redirect. Upstream TLS is always GitHub.
Credentials are supplied by systemd LoadCredential, outside all guest mounts.
"""
import base64
import fcntl
import http.client
import http.server
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import ssl
import stat
import sys
import tempfile
import threading

CELL = Path('/etc/cell/cell.json')
CREDENTIALS = Path('/etc/cell/git-credentials.json')
PORT = 3129
MAX_REQUEST = 128 * 1024**2
MAX_RESPONSE = 1024**3
REPO = r'[a-z0-9_.-]{1,100}/[a-z0-9_.-]{1,100}'
ROUTE = re.compile(r'/([A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100})\.git/(info/refs\?service=git-(upload|receive)-pack|git-(upload|receive)-pack)\Z')


def canonical_repo(repo):
    return isinstance(repo, str) and re.fullmatch(REPO, repo) is not None and all(
        p not in ('.', '..') for p in repo.split('/'))


def scopes(cell):
    result = {}
    for grant in cell['grants']:
        if 'code' not in grant:
            continue
        code = grant['code']
        repo = code.get('via') or code['repo']
        if not canonical_repo(repo) or type(code['write']) is not bool:
            raise ValueError('invalid repository declaration')
        result[repo] = result.get(repo, False) or code['write']
    return result


def credentials(path, systemd_copy=False):
    # The original store must be private to its owner. Ubuntu systemd gives
    # the service access to a root-owned 0440 copy via a per-user ACL inside
    # its private, read-only credential mount. Do not confuse that copy with
    # an ordinary group-readable configuration file.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        private = (info.st_uid == 0 and stat.S_IMODE(info.st_mode) in (0o400, 0o440)) if systemd_copy else (
            info.st_uid == os.getuid() and not info.st_mode & 0o077)
        if not stat.S_ISREG(info.st_mode) or not private or info.st_size > 1024**2:
            raise ValueError('Git credentials must be a private owned regular file')
        with os.fdopen(fd, 'r', closefd=False) as source:
            data = json.load(source)
    finally:
        os.close(fd)
    if not isinstance(data, dict) or any(
        not canonical_repo(repo) or not isinstance(token, str) or not re.fullmatch(r'[A-Za-z0-9_]{1,4096}', token)
        for repo, token in data.items()
    ):
        raise ValueError('invalid Git credential configuration')
    return data


def authorize(cell, method, path):
    match = ROUTE.fullmatch(path)
    if not match:
        raise PermissionError('only scoped Git smart HTTP is supported')
    repo, tail, advertised, rpc = match.groups()
    repo = repo.lower()
    service = advertised or rpc
    if (advertised and method != 'GET') or (rpc and method != 'POST'):
        raise PermissionError('invalid Git method')
    allowed = scopes(cell)
    if repo not in allowed or (service == 'receive' and not allowed[repo]):
        raise PermissionError('repository operation is not granted')
    kind = 'advertisement' if advertised else 'result'
    return repo, service, f'application/x-git-{service}-pack-{kind}'


class GitHub(http.client.HTTPSConnection):
    def __init__(self):
        super().__init__('github.com', timeout=30, context=ssl.create_default_context())

    def connect(self):
        # Validate the resolved addresses and connect to those exact addresses;
        # certificate verification/SNI still uses github.com. No proxy env,
        # second DNS resolution, redirects, metadata or private destinations.
        addresses = socket.getaddrinfo('github.com', 443, type=socket.SOCK_STREAM)
        if not addresses or any(not ipaddress.ip_address(a[4][0]).is_global or
                                ipaddress.ip_address(a[4][0]).is_multicast for a in addresses):
            raise OSError('GitHub did not resolve to public addresses')
        for family, kind, proto, _, address in addresses:
            raw = socket.socket(family, kind, proto)
            raw.settimeout(30)
            try:
                raw.connect(address)
                self.sock = self._context.wrap_socket(raw, server_hostname='github.com')
                return
            except OSError:
                raw.close()
        raise OSError('GitHub connection unavailable')


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, cell_path, tokens, upstream=GitHub):
        self.cell_path, self.tokens, self.upstream = Path(cell_path), tokens, upstream
        self.slots = threading.BoundedSemaphore(16)
        super().__init__(address, Handler)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def handle_error(self, request, address):
        # Neither raw HTTP headers nor exception text (possibly upstream data)
        # belong in logs containing credential-handling requests.
        print('Git request failed', file=sys.stderr)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def setup(self):
        super().setup()
        self.connection.settimeout(30)

    def log_message(self, *args):
        pass

    def refuse(self, status):
        self.send_response(status)
        self.send_header('Content-Length', '0')
        self.send_header('Connection', 'close')
        self.end_headers()

    def copy_exact(self, target, size):
        while size:
            chunk = self.rfile.read(min(size, 65536))
            if not chunk:
                raise ValueError('truncated request')
            target.write(chunk)
            size -= len(chunk)

    def request_body(self, target):
        lengths = self.headers.get_all('Content-Length', [])
        transfers = self.headers.get_all('Transfer-Encoding', [])
        if len(lengths) > 1 or len(transfers) > 1 or (lengths and transfers):
            raise ValueError('ambiguous framing')
        encodings = self.headers.get_all('Content-Encoding', [])
        if encodings not in ([], ['gzip']) or (encodings and self.command != 'POST'):
            raise ValueError('unsupported content encoding')
        if self.headers.get('Expect') or self.headers.get('Upgrade'):
            raise ValueError('unsupported framing')
        if transfers:
            if transfers != ['chunked'] or self.command != 'POST':
                raise ValueError('unsupported transfer encoding')
            total = 0
            while True:
                line = self.rfile.readline(32)
                if not re.fullmatch(b'[0-9a-fA-F]{1,16}\r\n', line):
                    raise ValueError('invalid chunk')
                size = int(line.strip(), 16)
                total += size
                if total > MAX_REQUEST:
                    raise ValueError('request too large')
                self.copy_exact(target, size)
                if self.rfile.read(2) != b'\r\n':
                    raise ValueError('chunk trailers are not supported')
                if size == 0:
                    return total
        raw = lengths[0] if lengths else '0'
        if not re.fullmatch(r'[0-9]{1,12}', raw):
            raise ValueError('invalid length')
        size = int(raw)
        if size > MAX_REQUEST or (self.command == 'GET' and size):
            raise ValueError('request too large')
        self.copy_exact(target, size)
        return size

    def serve_git(self):
        upstream = None
        started = False
        try:
            repo, service, content_type = authorize(
                json.loads(self.server.cell_path.read_text()), self.command, self.path)
            token = self.server.tokens.get(repo)
            if service == 'receive' and token is None:
                self.refuse(503)
                return
            if self.command == 'POST' and self.headers.get_all('Content-Type') != [f'application/x-git-{service}-pack-request']:
                self.refuse(415)
                return
            with tempfile.SpooledTemporaryFile(max_size=1024**2) as body:
                size = self.request_body(body)
                # An upload may have taken time; recheck authority before any
                # authenticated upstream request. Setup also stops this service.
                authorize(json.loads(self.server.cell_path.read_text()), self.command, self.path)
                headers = {'User-Agent': 'kuggis-scoped-git', 'Accept': content_type,
                           'Content-Length': str(size)}
                if token:
                    headers['Authorization'] = 'Basic ' + base64.b64encode(('x-access-token:' + token).encode()).decode()
                if self.command == 'POST':
                    headers['Content-Type'] = f'application/x-git-{service}-pack-request'
                    if self.headers.get('Content-Encoding') == 'gzip':
                        # Git compresses larger negotiation requests. Forward
                        # opaque bytes; the broker never inflates guest input.
                        headers['Content-Encoding'] = 'gzip'
                protocol = self.headers.get_all('Git-Protocol', [])
                if protocol in (['version=1'], ['version=2']):
                    headers['Git-Protocol'] = protocol[0]
                elif protocol:
                    raise ValueError('unsupported Git protocol')
                body.seek(0)
                upstream = self.server.upstream()
                suffix = ROUTE.fullmatch(self.path).group(2)
                upstream.request(self.command, '/' + repo + '.git/' + suffix, body=body if size else None, headers=headers)
                response = upstream.getresponse()
                # Do not relay redirects, cookies, auth challenges, error
                # bodies, or unexpected content from a renamed repository.
                if response.status != 200 or response.getheader('Content-Type') != content_type:
                    self.refuse(502)
                    return
                length = response.getheader('Content-Length')
                if length is not None and (not length.isdecimal() or int(length) > MAX_RESPONSE):
                    self.refuse(502)
                    return
                self.send_response(200)
                self.send_header('Content-Type', content_type)
                self.send_header('Cache-Control', 'no-store')
                self.send_header('Connection', 'close')
                if length is not None:
                    self.send_header('Content-Length', length)
                self.end_headers()
                started = True
                total = 0
                while chunk := response.read(65536):
                    total += len(chunk)
                    if total > MAX_RESPONSE:
                        break
                    self.wfile.write(chunk)
        except PermissionError:
            if not started:
                self.refuse(403)
        except ValueError:
            if not started:
                self.refuse(400)
        except (OSError, http.client.HTTPException):
            if not started:
                self.refuse(502)
        finally:
            if upstream is not None:
                upstream.close()
            self.close_connection = True

    do_GET = do_POST = serve_git


def preflight(cell_path):
    wanted = scopes(json.loads(Path(cell_path).read_text()))
    tokens = credentials(CREDENTIALS) if CREDENTIALS.exists() else {}
    if any(write and repo not in tokens for repo, write in wanted.items()):
        raise ValueError('writable Git grants need a host-held repository credential; configure /etc/cell/git-credentials.json before setup')


def store_credential(repo, source):
    if os.getuid() != 0 or not canonical_repo(repo):
        raise ValueError('credential installation requires host root and owner/repo')
    token = source.read(4098).rstrip('\n')
    if not re.fullmatch(r'[A-Za-z0-9_]{1,4096}', token):
        raise ValueError('invalid token on stdin')
    CREDENTIALS.parent.mkdir(parents=True, exist_ok=True)
    with CREDENTIALS.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        saved = credentials(CREDENTIALS) if CREDENTIALS.exists() else {}
        saved[repo] = token
        with tempfile.NamedTemporaryFile(mode='w', dir=CREDENTIALS.parent, delete=False) as target:
            staged = Path(target.name)
            try:
                json.dump(saved, target)
                target.flush(); os.fsync(target.fileno())
                staged.replace(CREDENTIALS)
            finally:
                staged.unlink(missing_ok=True)
    print('Repository credential stored. Restart cell-git or run setup to activate it.')


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == 'preflight':
        preflight(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == 'credential':
        store_credential(sys.argv[2], sys.stdin)
    elif sys.argv[1:] == ['serve']:
        path = Path(os.environ['CREDENTIALS_DIRECTORY']) / 'github'
        Server(('0.0.0.0', PORT), CELL, credentials(path, systemd_copy=True)).serve_forever()
    else:
        raise SystemExit('expected preflight CELLFILE, credential OWNER/REPO, or serve')
