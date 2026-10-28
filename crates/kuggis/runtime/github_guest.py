#!/usr/bin/python3
"""Guest Git credential helper and explicit-repository gh adapter."""
import http.client
import json
import os
import socket
import subprocess
import sys
from github_common import Failure, repository

SOCKET = '/run/kuggis-github/token.sock'


def transport():
    from pathlib import Path
    try:
        return Path('/etc/kuggis-github-transport').read_text().strip()
    except FileNotFoundError:
        return 'github'


class Connection(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(SOCKET)


def api_request(path, body):
    data = json.dumps(body).encode()
    if len(data) > 65536:
        raise Failure('API request exceeds 64 KiB')
    connection = Connection('localhost', timeout=160)
    try:
        connection.request('POST', path, data, {'Content-Type': 'application/json'})
        response = connection.getresponse()
        raw = response.read(2 * 1024 * 1024 + 1)
        if len(raw) > 2 * 1024 * 1024:
            raise Failure('API response too large; request a smaller page')
        value = json.loads(raw)
        if response.status != 200:
            raise Failure(value.get('error', 'API operation failed'), response.status)
        return value['result']
    finally:
        connection.close()


def credential_result(repo):
    connection = Connection('localhost', timeout=65)
    try:
        connection.request('POST', '/token', json.dumps({'repo': repository(repo)}),
                           {'Content-Type': 'application/json'})
        response = connection.getresponse()
        data = response.read(16385)
        if response.status != 200 or len(data) > 16384:
            raise Failure('GitHub access unavailable; ask the operator to check this cell\'s repo grants')
        result = json.loads(data)
        value = result['token']
        if not isinstance(value, str) or not value or any(c.isspace() for c in value):
            raise Failure('invalid credential response')
        return result
    finally:
        connection.close()


def token(repo):
    result = credential_result(repo)
    if result.get('transport', 'github') != 'github':
        raise Failure('SSH gateway supports Git clone/fetch/push; GitHub API commands need separate API authorization')
    return result['token']


def credential(operation, stream, out):
    if operation in ('store', 'erase'):
        return  # Git owns no persistent token cache; refresh is broker-owned.
    if operation != 'get':
        raise Failure('invalid credential operation')
    fields = {}
    remaining = 8192
    while True:
        line = stream.readline(remaining + 1)
        remaining -= len(line)
        if remaining < 0:
            raise Failure('credential request too large')
        if line in ('', '\n'):
            break
        line = line.rstrip('\n')
        key, separator, value = line.partition('=')
        if not separator or key in fields:
            raise Failure('invalid credential request')
        fields[key] = value
    if fields.get('protocol') != 'https' or fields.get('host') != 'github.com':
        return
    path = fields.get('path', '')
    if path.endswith('.git'):
        path = path[:-4]
    value = token(repository(path))
    out.write('username=x-access-token\npassword=' + value + '\n\n')


def gh(args):
    if len(args) < 4 or args[0] != '--repo' or args[2] != '--':
        raise Failure('usage: kuggis-gh --repo OWNER/REPO -- COMMAND [ARGS...]')
    repo = repository(args[1])
    argv = args[3:]
    if transport() == 'ssh-gateway':
        from github_collab import run
        print(json.dumps(run(repo, argv, api_request), indent=2))
        return
    # Never send a credential to another hostname selected by CLI arguments.
    if (argv[0] not in ('pr', 'issue', 'repo', 'api', 'run', 'workflow', 'release')
            or any(a in ('--hostname', '--host') or a.startswith(('--hostname=', '--host=')) for a in argv)
            or (argv[0] == 'api' and any('://' in a or a.startswith('//') for a in argv[1:]))):
        raise Failure('unsupported gh command or hostname; use github.com and repository-scoped commands')
    env = {k: v for k, v in os.environ.items() if k not in (
        'GH_TOKEN', 'GITHUB_TOKEN', 'GH_ENTERPRISE_TOKEN', 'GITHUB_ENTERPRISE_TOKEN', 'GH_HOST', 'GH_REPO')}
    env.update(GH_HOST='github.com', GH_REPO=repo, GH_TOKEN=token(repo), GH_PROMPT_DISABLED='1')
    os.execve('/usr/bin/gh', ['/usr/bin/gh', *argv], env)


def configure(transport='github'):
    if os.geteuid() != 0:
        raise Failure('configuration requires guest root')
    if transport not in ('github', 'ssh-gateway'):
        raise Failure('unknown GitHub transport')
    from pathlib import Path
    marker = Path('/etc/kuggis-github-transport')
    marker.write_text(transport + '\n')
    marker.chmod(0o644)
    if transport == 'ssh-gateway':
        remote = Path('/usr/local/bin/git-remote-kuggis')
        remote.write_text('#!/bin/sh\nexec /usr/bin/python3 /run/kuggis-runtime/github_git.py "$@"\n')
        remote.chmod(0o755)
        subprocess.run(['git', 'config', '--system', '--replace-all', 'url.kuggis::.insteadOf', 'https://github.com/'], check=True)
        for prefix in ('git@github.com:', 'ssh://git@github.com/'):
            subprocess.run(['git', 'config', '--system', '--add', 'url.kuggis::.insteadOf', prefix], check=True)
        subprocess.run(['git', 'config', '--system', 'protocol.kuggis.allow', 'always'], check=True)
    helper = '/usr/bin/python3 /run/kuggis-runtime/github_guest.py credential'
    # A URL-specific empty entry resets any earlier system helper for GitHub.
    subprocess.run(['git', 'config', '--system', '--replace-all', 'credential.https://github.com.helper', ''], check=True)
    subprocess.run(['git', 'config', '--system', '--add', 'credential.https://github.com.helper', helper], check=True)
    subprocess.run(['git', 'config', '--system', 'credential.https://github.com.useHttpPath', 'true'], check=True)
    path = Path('/usr/local/bin/kuggis-gh')
    path.write_text('#!/bin/sh\nexec /usr/bin/python3 /run/kuggis-runtime/github_guest.py gh "$@"\n')
    path.chmod(0o755)


def main(args):
    if args and args[0] == 'configure' and len(args) in (1, 2):
        configure(args[1] if len(args) == 2 else 'github')
    elif len(args) == 2 and args[0] == 'credential':
        credential(args[1], sys.stdin, sys.stdout)
    elif args and args[0] == 'gh':
        gh(args[1:])
    else:
        raise Failure('invalid GitHub helper operation')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as error:
        print('kuggis github: ' + (str(error) if isinstance(error, Failure) else 'credential service unavailable'), file=sys.stderr)
        sys.exit(1)
