#!/usr/bin/env python3
"""Exercise the real management protocol on local fixtures; never run host effects."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

root = Path(os.environ['KUGGIS_MANAGEMENT_FIXTURE'])
words = shlex.split(sys.argv[-1])
if words[:4] == ['sudo', '-n', '--', '/usr/local/libexec/kuggis-host']:
    assert words[4:7] == ['exec', 'fri', '--']
    assert words[7:9] == ['/usr/bin/python3', '-c']
    home = root / 'agent'
    if words[-1] == 'callback':
        # Fake SSH shares the laptop's namespace; give the fixture's native
        # server a different socket while retaining the production relay code.
        words[9] = words[9].replace("HTTPConnection('127.0.0.1', port,",
            "HTTPConnection('127.0.0.1', int((Path.home()/'.codex/.fixture-callback-port').read_text()),")
    os.execvpe(sys.executable, [sys.executable, *words[8:]], {**os.environ, 'HOME': str(home)})
assert words[:5] == ['sudo', '-n', '--', '/usr/bin/python3', '-c']
config = root / 'workspace.json'
if len(words) == 7 and words[-1] == 'install':
    request = json.loads(sys.stdin.buffer.readline())
    binary = sys.stdin.buffer.read()
    assert len(binary) == request['binary_size']
    assert hashlib.sha256(binary).hexdigest() == request['sha256']
    spec = importlib.util.spec_from_file_location('host', Path(os.environ['KUGGIS_RUNTIME_ROOT']) / 'host.py')
    host = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(host)
    host.CONFIG = config
    host.trusted_bytes = lambda path: Path(path).read_bytes()
    host.check_update(request, json.loads(config.read_text()), request['bundle']['workspace'])
    config.write_text(json.dumps(request['bundle']['workspace']))
    with (root / 'calls').open('a') as stream:
        stream.write('install\n')
    print(json.dumps({'ok': True, 'test_transport_only': True}))
    sys.exit(0)
assert len(words) == 6
namespace = {'__name__': 'management_fixture'}
exec(words[5], namespace)
namespace['CONFIG'] = config
namespace['BINARY'] = os.environ['KUGGIS_BINARY']
def command(*args, **kwargs):
    if args[0] == namespace['BINARY']:
        return subprocess.run(args, capture_output=True, check=True).stdout
    if args[:2] == ('systemctl', 'show'):
        return b'ActiveState=active\nMainPID=123\nExecMainStartTimestampMonotonic=456\n'
    if args[:2] == ('systemctl', 'restart'):
        assert args[2] == 'kuggis-v2-proxy.service', 'never restart a cell for network changes'
        with (root / 'calls').open('a') as stream:
            stream.write('restart-proxy\n')
        failure = root / 'fail-restart'
        if failure.exists():
            failure.unlink()
            raise RuntimeError('fixture restart failed')
        return b''
    if args[:2] == ('systemctl', 'is-active'):
        return b''
    if args[0] == 'machinectl':
        return b'123'
    if args[0] == 'nsenter':
        host, port = args[-2], int(args[-1])
        workspace = json.loads(config.read_text())
        allowed = workspace.get('egress') == 'internet' or any(
            (e['host'], e['port']) == (host, port) for r in workspace['resources'] for e in r['endpoints'])
        return json.dumps({'reachable': allowed, 'proxy_status': 200 if allowed else 403,
                           'authentication': 'not_checked'}).encode()
    raise AssertionError('unexpected host command')
namespace['command'] = command
try:
    request = json.load(sys.stdin)
    if request['action'] == 'network' and (root / 'concurrent-edit').exists():
        workspace = json.loads(config.read_text())
        workspace['packages'].append('make')
        config.write_text(json.dumps(workspace))
    print(json.dumps({'ok': True, **namespace['handle'](request)}))
except Exception as error:
    print(json.dumps({'ok': False, 'error': str(error)}))
    sys.exit(2)
