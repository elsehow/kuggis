#!/usr/bin/python3
"""On-host acceptance for the dedicated v2-acceptance fixture only.

Run as root after setup. Uses only synthetic files and a guest web server. This
changes the disposable guest and temporarily removes its host firewall, restoring
it in a finally block. It must never target an existing user workspace.
"""
import importlib.util
import contextlib
import hashlib
import io
import json
from pathlib import Path
import sys
import time
from importlib.machinery import SourceFileLoader

loader = SourceFileLoader('kuggis_host', '/usr/local/libexec/kuggis-host')
spec = importlib.util.spec_from_loader(loader.name, loader)
h = importlib.util.module_from_spec(spec)
loader.exec_module(h)
if h.load()['name'] != 'v2-acceptance':
    raise RuntimeError('disposable v2-acceptance workspace required')
if h.load()['launch'] != ['/bin/bash', '-l']:
    raise RuntimeError('acceptance requires the Bash fixture; never send shell tests into an agent TUI')

results = []


def case(name, fn):
    print('TEST ' + name, file=sys.stderr, flush=True)
    try:
        fn()
        results.append({'name': name, 'passed': True})
    except Exception as error:
        results.append({'name': name, 'passed': False, 'detail': str(error)})


def require(value, message):
    if not value:
        raise RuntimeError(message)


def boundary():
    require(h.check()['ok'], 'initial boundary check failed')
    h.guest('sudo', '-n', 'nft', 'flush', 'ruleset')
    require(h.check()['ok'], 'guest root changed host boundary')
    h.guest('sudo', '-n', 'sh', '-c', 'test ! -e /run/host/bus && test ! -e /etc/kuggis/workspace.json')


def permitted_egress():
    mirror = 'ports.ubuntu.com/ubuntu-ports' if h.platform.machine() == 'aarch64' else 'archive.ubuntu.com/ubuntu'
    h.guest('curl', '-fsSI', '--max-time', '20', f'https://{mirror}/dists/noble/Release')


def terminal():
    h.guest('systemctl', 'start', 'kuggis-terminal.service', user='root')
    h.guest('rm', '-f', '/home/agent/workspace/terminal-marker')
    before = h.guest('tmux', '-L', 'kuggis', 'display-message', '-p', '#{pid}').stdout.strip()
    h.guest('tmux', '-L', 'kuggis', 'send-keys', '-t', 'workspace',
            'printf terminal-survived > /home/agent/workspace/terminal-marker', 'Enter')
    for _ in range(30):
        try:
            marker = h.guest('cat', '/home/agent/workspace/terminal-marker').stdout
            if marker == b'terminal-survived':
                break
        except RuntimeError:
            pass
        time.sleep(.1)
    else:
        raise RuntimeError('terminal command did not complete')
    after = h.guest('tmux', '-L', 'kuggis', 'display-message', '-p', '#{pid}').stdout.strip()
    require(before == after, 'terminal server changed between independent connections')


def web():
    h.guest('python3', '-c', "from pathlib import Path; Path('index.html').write_text('kuggis-private-preview')")
    h.guest('systemd-run', '--collect', '--unit=kuggis-test-web', '--uid=agent',
            '--working-directory=/home/agent/workspace', '/usr/bin/python3', '-m', 'http.server', '3000', '--bind', '0.0.0.0', user='root')
    try:
        for _ in range(30):
            result = h.run('curl', '-fsS', '--max-time', '3', 'http://127.0.0.1:8000', check=False)
            if result.returncode == 0 and b'kuggis-private-preview' in result.stdout:
                break
            time.sleep(.1)
        else:
            raise RuntimeError('host loopback relay did not reach guest app')
        listen = h.run('systemctl', 'show', 'kuggis-v2-web-preview.socket', '-p', 'Listen', '--value').stdout
        require(b'127.0.0.1:8000' in listen and b'0.0.0.0:8000' not in listen, 'relay is not loopback-only')
    finally:
        h.guest('systemctl', 'stop', 'kuggis-test-web', user='root')


def missing_wall():
    h.run('systemctl', 'stop', h.ENVIRONMENT)
    h.run('nft', 'delete', 'table', 'inet', 'kuggis_v2')
    try:
        started = h.run('systemctl', 'start', h.ENVIRONMENT, check=False)
        require(started.returncode != 0, 'guest booted without its firewall')
    finally:
        h.run('nft', '-f', h.ETC / 'wall.nft')
        h.run('systemctl', 'reset-failed', h.ENVIRONMENT)
        h.run('systemctl', 'start', h.ENVIRONMENT)
        h.ready()
    require(h.guest('cat', '/home/agent/workspace/terminal-marker').stdout == b'terminal-survived', 'file lost on restart')
    active = h.run(
        *h.guest_command(['systemctl', 'is-active', '--quiet', 'kuggis-terminal.service'], user='root'), check=False)
    require(active.returncode != 0, 'terminal work was automatically relaunched after restart')


def reapply():
    workspace = h.load()
    binary = (h.RUNTIME / 'kuggis').read_bytes()
    bundle = json.loads(h.run(h.RUNTIME / 'kuggis', 'bundle', h.CONFIG).stdout)
    request = {'bundle': bundle, 'binary_size': len(binary), 'sha256': hashlib.sha256(binary).hexdigest()}
    stream = io.BytesIO(json.dumps(request).encode() + b'\n' + binary)
    with contextlib.redirect_stdout(io.StringIO()):
        h.install(stream)
    require(h.load() == workspace, 'declaration changed during identical reapply')
    require(h.guest('cat', '/home/agent/workspace/terminal-marker').stdout == b'terminal-survived', 'setup lost workspace data')
    require(h.check()['ok'], 'boundary invalid after reapply')


for name, function in [('boundary', boundary), ('permitted-https-egress', permitted_egress), ('persistent-terminal', terminal), ('loopback-web-relay', web), ('missing-wall-and-restart', missing_wall), ('reapply-preserves-data', reapply)]:
    case(name, function)
print(json.dumps({'ok': all(result['passed'] for result in results), 'checks': results}, indent=2))
sys.exit(not all(result['passed'] for result in results))
