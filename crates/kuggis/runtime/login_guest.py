"""Native login adapter. Always entered as agent through the installed host helper."""
import base64
import fcntl
import http.client
import json
import os
from pathlib import Path
import runpy
import selectors
import subprocess
import sys
import time
import tomllib
from urllib.parse import urlsplit

LOGIN_LIMIT = 2 * 1024 * 1024


def emit(**event):
    print(json.dumps(event), flush=True)


def native_environment(home):
    return {**os.environ, 'CODEX_HOME': str(home / '.codex')}


def cached_status(home):
    result = subprocess.run([str(home / '.local/bin/codex'), '-c', 'cli_auth_credentials_store="file"',
                             'login', 'status'], env=native_environment(home), capture_output=True, timeout=30)
    return result.returncode == 0 and b'ChatGPT' in result.stdout + result.stderr


def prepare_login(home, request):
    binary = home / '.local/bin/codex'
    result = subprocess.run([str(binary), '--version'], capture_output=True, timeout=30, check=True)
    version = result.stdout.decode().strip().removeprefix('codex-cli ')
    if tuple(map(int, version.split('.'))) < (0, 165, 0):
        raise ValueError('cell login requires Codex 0.165.0 or newer; update the Codex recipe first')
    namespace = {'__name__': 'kuggis_codex_recipe'}
    source = request['launcher']
    exec(compile(source, 'kuggis-codex', 'exec'), namespace)
    common = namespace['shared_home'](home)
    names = []
    for path in sorted((home / '.kuggis/agents/codex').glob('*')):
        if path.is_dir() and not path.is_symlink():
            namespace['prepare'](home, path.name, home / 'workspace')
            names.append(path.name)
    launcher = home / '.local/bin/kuggis-codex'
    if launcher.exists() and not (home / '.local/bin/kuggis-codex.before-shared-login').exists():
        namespace['atomic'](home / '.local/bin/kuggis-codex.before-shared-login', launcher.read_bytes(), 0o700)
    namespace['atomic'](launcher, source, 0o755)
    return {'event': 'prepared', 'chats': names, 'codex_home': str(common), 'version': version,
            'running_chats': 'unchanged'}


class NativeLogin:
    def __init__(self, home):
        self.child = subprocess.Popen([str(home / '.local/bin/codex'), '-c', 'model_provider="openai"',
            '-c', 'cli_auth_credentials_store="file"', 'app-server', '--stdio'],
            env=native_environment(home), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, bufsize=0)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.child.stdout, selectors.EVENT_READ)
        self.selector.register(sys.stdin, selectors.EVENT_READ)
        self.sequence = 0
        self.notifications = []

    def send(self, message):
        self.child.stdin.write(json.dumps(message).encode() + b'\n')
        self.child.stdin.flush()

    def receive(self, timeout=60):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for key, _ in self.selector.select(min(1, max(0, deadline - time.monotonic()))):
                if key.fileobj is sys.stdin:
                    # Closing the laptop process's input cancels login as well.
                    if not sys.stdin.buffer.readline(LOGIN_LIMIT + 1):
                        raise InterruptedError('login cancelled')
                    raise ValueError('unexpected login input')
                line = self.child.stdout.readline(LOGIN_LIMIT + 1)
                if not line:
                    raise RuntimeError('native Codex login stopped; check cell connectivity')
                if len(line) > LOGIN_LIMIT:
                    raise ValueError('native login response is too large')
                return json.loads(line)
        raise TimeoutError('native Codex login timed out')

    def call(self, method, params):
        self.sequence += 1
        self.send({'id': self.sequence, 'method': method, 'params': params})
        while True:
            message = self.receive()
            if message.get('id') == self.sequence:
                if 'error' in message:
                    if method == 'account/read':
                        raise RuntimeError('Codex workspace discovery failed; check provider network access and organization routing')
                    raise RuntimeError('native Codex rejected ' + method)
                return message['result']
            self.notifications.append(message)

    def close(self):
        self.selector.close()
        self.child.stdin.close()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()


def login(home, force):
    common = home / '.codex'
    with (common / '.kuggis-login.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        native = NativeLogin(home)
        try:
            native.call('initialize', {'clientInfo': {'name': 'kuggis', 'version': '0.2.0'},
                                      'capabilities': {'experimentalApi': True}})
            native.send({'method': 'initialized', 'params': {}})
            reused = not force and cached_status(home)
            if not reused:
                started = native.call('account/login/start', {'type': 'chatgpt'})
                url = started['authUrl']
                if urlsplit(url).scheme != 'https' or urlsplit(url).hostname != 'auth.openai.com':
                    raise ValueError('native Codex returned an unexpected sign-in origin')
                emit(event='authorize', url=url)
                deadline = time.monotonic() + 600
                while True:
                    message = native.notifications.pop(0) if native.notifications else native.receive(
                        max(0, min(600, deadline - time.monotonic())))
                    if time.monotonic() >= deadline:
                        raise TimeoutError('browser login timed out; rerun kuggis login')
                    if message.get('method') == 'account/login/completed':
                        event = message['params']
                        if event.get('loginId') != started['loginId']:
                            continue
                        if not event.get('success'):
                            raise RuntimeError('browser login failed; verify account permissions and provider network connectivity, then retry')
                        break
            account = native.call('account/read', {'refreshToken': False})
            if not account.get('account'):
                raise RuntimeError('Codex did not retain the login; rerun kuggis login')
            recipe = runpy.run_path(str(home / '.local/bin/kuggis-codex'), run_name='kuggis_codex_recipe')
            recipe['activate'](home)
            route = account.get('workspaceRouting') or {}
            emit(event='complete', authenticated=True, reused=reused, backend_origin=route.get('backendOrigin'))
        finally:
            # Closing app-server tears down its native OAuth listener too.
            native.close()


def callback(request):
    port, path = request['port'], request['path']
    if type(port) is not int or port not in (1455, 1457):
        raise ValueError('unexpected native callback port')
    if not isinstance(path, str) or len(path) > 16384 or '\r' in path or '\n' in path:
        raise ValueError('invalid callback request')
    parsed = urlsplit(path)
    if parsed.scheme or parsed.netloc or parsed.path not in ('/auth/callback', '/success'):
        raise ValueError('unexpected callback path')
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=60)
    try:
        connection.request('GET', path, headers={'Connection': 'close'})
        response = connection.getresponse()
        body = response.read(256 * 1024 + 1)
        if len(body) > 256 * 1024:
            raise ValueError('callback response is too large')
        return {'status': response.status, 'headers': [(k, v) for k, v in response.getheaders()
                if k.lower() in ('content-type', 'location')], 'body': base64.b64encode(body).decode()}
    finally:
        connection.close()


def guest_login_main():
    request = json.loads(sys.stdin.buffer.readline(LOGIN_LIMIT + 1))
    home = Path.home()
    mode = sys.argv[1]
    if mode == 'prepare':
        emit(**prepare_login(home, request))
    elif mode == 'callback':
        emit(**callback(request))
    elif mode == 'status':
        config = home / '.codex/config.toml'
        emit(event='status', authenticated=cached_status(home),
             provider=tomllib.loads(config.read_text()).get('model_provider', 'openai') if config.exists() else None)
    elif mode == 'login':
        login(home, request.get('force', False))
    else:
        raise ValueError('unknown login operation')


if __name__ == '__main__':
    try:
        guest_login_main()
    except BaseException as error:
        message = ('another login is already running for this cell' if isinstance(error, BlockingIOError) else
                   str(error) if isinstance(error, (RuntimeError, ValueError, TimeoutError, InterruptedError)) else
                   'cell login adapter failed; check the Codex installation and cell connectivity')
        emit(event='error', message=message)
        sys.exit(2)
