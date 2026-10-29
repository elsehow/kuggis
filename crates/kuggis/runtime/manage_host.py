"""Trusted operator code sent over SSH. No agent-supplied code runs on the host."""
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

CONFIG = Path('/etc/kuggis/workspace.json')
BINARY = '/usr/local/lib/kuggis-v2/kuggis'
HELPER = '/usr/local/libexec/kuggis-host'
LIMIT = 1024 * 1024


def command(*args, timeout=60):
    result = subprocess.run(args, capture_output=True, timeout=timeout,
        env={**os.environ, 'PATH': '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin'})
    if result.returncode:
        raise RuntimeError('host command failed: ' + args[0])
    return result.stdout


def protected_read(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise RuntimeError('host configuration must be an owned protected regular file')
        data = stream.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise RuntimeError('host configuration exceeds 1 MiB')
        return data


def revision(data):
    return hashlib.sha256(data).hexdigest()


def without_network(workspace):
    result = copy.deepcopy(workspace)
    # Empty resource labels have no effect outside the proxy.
    result['resources'] = [{k: v for k, v in resource.items() if k != 'endpoints'}
                          for resource in result.get('resources', []) if resource.get('credentials')]
    return result


def atomic(path, data, mode=0o600):
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-management-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        Path(temporary).unlink(missing_ok=True)


def services():
    result = {}
    for unit in ('kuggis-v2.service', 'kuggis-v2-proxy.service', 'kuggis-v2-github.service'):
        fields = command('systemctl', 'show', unit, '-p', 'ActiveState', '-p', 'MainPID',
                         '-p', 'ExecMainStartTimestampMonotonic').decode().splitlines()
        result[unit] = dict(line.split('=', 1) for line in fields if '=' in line)
    return result


def handle(request):
    if not isinstance(request, dict) or request.get('action') not in ('inspect', 'network', 'probe'):
        raise ValueError('unknown management operation')
    action = request['action']
    expected = {'action', 'cell'} | ({'revision', 'workspace'} if action == 'network' else
                                    {'host', 'port'} if action == 'probe' else set())
    if set(request) != expected:
        raise ValueError('unexpected management fields')
    directory = CONFIG.parent.stat()
    if directory.st_uid != os.geteuid() or directory.st_mode & 0o022 or CONFIG.parent.is_symlink():
        raise RuntimeError('unprotected configuration directory')
    with open(CONFIG.parent / 'setup.lock', 'rb') as lock:
        fcntl.flock(lock, (fcntl.LOCK_EX if action == 'network' else fcntl.LOCK_SH) | fcntl.LOCK_NB)
        original = protected_read(CONFIG)
        current = json.loads(original)
        if current['name'] != request['cell']:
            raise RuntimeError('host belongs to a different cell')
        if action == 'inspect':
            return {'workspace': current, 'revision': revision(original), 'services': services(),
                    'updating': (CONFIG.parent / 'updating').exists()}
        if (CONFIG.parent / 'updating').exists():
            raise RuntimeError('host setup is incomplete; inspect cell status before changing access')
        if action == 'probe':
            # Run the socket probe in the guest network namespace, with trusted host
            # Python and no guest credentials or mutable guest executables.
            host, port = request['host'], request['port']
            import re
            if not isinstance(host, str) or not re.fullmatch(r'[a-z0-9.:[\]-]{1,253}', host):
                raise ValueError('invalid probe host')
            if type(port) is not int or not 1 <= port <= 65535:
                raise ValueError('invalid probe port')
            pid = int(command('machinectl', 'show', 'kuggis-v2', '-p', 'Leader', '--value'))
            code = '''import json,socket,sys
host,port=sys.argv[1],int(sys.argv[2])
try:
 with socket.create_connection(('10.205.0.1',3128),10) as sock:
  sock.settimeout(15)
  authority=('['+host+']' if ':' in host else host)+':'+str(port)
  sock.sendall(('CONNECT '+authority+' HTTP/1.1\\r\\nHost: '+authority+'\\r\\n\\r\\n').encode())
  line=sock.makefile('rb').readline(1024)
  status=int(line.split()[1])
  print(json.dumps({'reachable':status==200,'proxy_status':status,'authentication':'not_checked'}))
except (OSError,ValueError,IndexError):
 print(json.dumps({'reachable':False,'reason':'connection_failed','authentication':'not_checked'}))
'''
            return json.loads(command('nsenter', '-t', str(pid), '-n', '--', '/usr/bin/python3',
                                      '-c', code, host, str(port), timeout=35))
        if revision(original) != request['revision']:
            raise RuntimeError('configuration changed concurrently; rerun the command')
        candidate = request['workspace']
        if without_network(current) != without_network(candidate):
            raise ValueError('this operation may change only network endpoints')
        encoded = (json.dumps(candidate, indent=2) + '\n').encode()
        if len(encoded) > LIMIT:
            raise ValueError('configuration exceeds 1 MiB')
        if candidate == current:
            return {'workspace': current, 'revision': revision(original), 'changed': False, 'restart': 'none'}
        with tempfile.TemporaryDirectory(prefix='.validate-', dir=CONFIG.parent) as staging:
            staged = Path(staging) / 'workspace.json'
            staged.write_bytes(encoded)
            command(BINARY, 'validate', str(staged))
        before = services()['kuggis-v2.service']
        atomic(CONFIG.parent / 'workspace.previous.json', original)
        mode = CONFIG.stat().st_mode & 0o777
        atomic(CONFIG, encoded, mode)
        try:
            command('systemctl', 'restart', 'kuggis-v2-proxy.service')
            command('systemctl', 'is-active', '--quiet', 'kuggis-v2-proxy.service')
            if services()['kuggis-v2.service'] != before:
                raise RuntimeError('cell service changed during proxy update')
        except BaseException:
            atomic(CONFIG, original, mode)
            command('systemctl', 'restart', 'kuggis-v2-proxy.service')
            raise
        return {'workspace': candidate, 'revision': revision(encoded), 'changed': True, 'restart': 'proxy'}


if __name__ == '__main__':
    try:
        if os.geteuid() != 0:
            raise RuntimeError('host management requires root')
        data = sys.stdin.buffer.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise ValueError('management request exceeds 1 MiB')
        print(json.dumps({'ok': True, **handle(json.loads(data))}))
    except Exception as error:
        print(json.dumps({'ok': False, 'error': 'another configuration update is in progress; retry' if isinstance(error, BlockingIOError) else str(error)}))
        sys.exit(2)
