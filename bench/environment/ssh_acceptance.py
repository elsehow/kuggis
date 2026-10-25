#!/usr/bin/env python3
"""Live acceptance on a disposable SSH-hosted cell; never a production cell.

Run as host root: python3 ssh_acceptance.py CELL PHASE
Phases: seed, boundary, verify, fail-closed, post-reboot.
Seed installs jq using the cell's Ubuntu mirror grants, creates one idle chat,
and writes guest markers. No provider credentials or model calls are needed.
Verify after CLI reapply, service restart, and a separately requested reboot.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import sys


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    name, phase = sys.argv[1:]
    cell = json.loads(Path('/etc/cell/cell.json').read_text())
    assert cell['name'] == name and name.endswith('-canary'), 'disposable canary only'
    assert json.loads(Path('/etc/cell/host.json').read_text())['backend'] == 'ssh'
    assert all(set(g) <= {'host', 'provider'} for g in cell['grants'])
    spec = importlib.util.spec_from_file_location('environment', '/usr/local/lib/cell/environment.py')
    h = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(h)
    state = Path('/var/lib/cell/ssh-acceptance.json')

    def shell(script, user='agent', timeout=180):
        return h.guest('/bin/bash', '-euc', script, user=user, timeout=timeout).stdout.strip()

    def api(method, route, body=None):
        args = ['curl', '-fsS', '--max-time', '60', '--unix-socket', '/run/cell-chat/api.sock',
                '-H', 'Host: cell.internal', '-X', method]
        if body is not None:
            args += ['-H', 'Content-Type: application/json', '--data', json.dumps(body)]
        return json.loads(h.run(*args, 'http://cell.internal/api/pi/' + route).stdout)

    def identity():
        return {path: digest(path) for path in (
            '/etc/machine-id', '/etc/cell/secret', '/etc/cell/key.pub',
            '/etc/cell/cell.json', '/etc/cell/host.json', '/usr/local/bin/cell')}

    def verify():
        saved = json.loads(state.read_text())
        assert identity() == saved['identity'], 'host identity/configuration changed'
        assert shell('cat /home/agent/work/ssh-acceptance-marker') == 'workspace preserved'
        assert shell('cat /etc/ssh-acceptance-marker') == 'guest configuration preserved'
        assert shell('jq --version') == saved['jq']
        assert shell('/usr/local/lib/cell/pi-1.3.0-node-24.22.1/pi --version') == saved['pi']
        threads = api('GET', 'threads?includeArchived=true')
        found = [t for t in threads if t['id'] == saved['thread']]
        assert len(found) == 1 and found[0]['title'] == 'SSH persistence acceptance'
        assert found[0]['status'] == 'idle', found[0]['status']
        snapshot = api('GET', 'threads/' + saved['thread'])
        assert snapshot['metadata']['id'] == saved['thread']
        h.run('/usr/local/bin/cell', 'check', timeout=180)
        return 'Identity, workspace, guest configuration, installed jq, Pi and idle chat preserved; boundary passes'

    if phase == 'seed':
        assert not state.exists(), 'seed already exists; use verify'
        assert not api('GET', 'threads?includeArchived=true'), 'canary already contains chats'
        assert json.loads(h.run('python3', '/usr/local/lib/cell/environment.py', 'providers').stdout) == [], 'credentials present'
        shell('''
test ! -e /home/agent/work/ssh-acceptance-marker
printf 'workspace preserved\n' > /home/agent/work/ssh-acceptance-marker
sudo -n sh -c 'printf "guest configuration preserved\\n" > /etc/ssh-acceptance-marker'
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq jq
''', timeout=600)
        thread = api('POST', 'threads', {'title': 'SSH persistence acceptance'})
        saved = dict(identity=identity(), thread=thread['metadata']['id'],
                     jq=shell('jq --version'),
                     pi=shell('/usr/local/lib/cell/pi-1.3.0-node-24.22.1/pi --version'),
                     boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        with state.open('x') as f:
            f.write(json.dumps(saved, indent=2) + '\n')
        state.chmod(0o600)
        print(verify())
    elif phase == 'boundary':
        before = identity()
        shell('''
test ! -e /etc/cell/secret
test ! -e /proc/1/root/etc/cell/secret
test ! -S /run/host/bus
test ! -S /run/docker.sock
sudo -n nft flush ruleset
sudo -n useradd -m boundary-user 2>/dev/null || id boundary-user
''')
        # Flushing the guest firewall must leave the host boundary intact.
        h.guard()
        h.run('/usr/local/bin/cell', 'check', timeout=180)
        # A different guest UID must also be unable to contact host SSH or
        # metadata services. This code runs inside the guest's namespaces.
        h.guest('python3', '-c', '''import socket
for host, port in [('10.203.0.1', 22), ('169.254.169.254', 80), ('1.1.1.1', 443)]:
 try: s = socket.create_connection((host, port), 3)
 except OSError: continue
 s.close(); raise AssertionError((host, port, 'direct route escaped'))
''', user='boundary-user')
        assert identity() == before
        print('Guest root firewall changes and alternate UID cannot bypass host isolation; host identity unchanged')
    elif phase in ('verify', 'post-reboot'):
        print(verify())
        saved = json.loads(state.read_text())
        rebooted = saved['boot_id'] != Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        if phase == 'post-reboot':
            assert rebooted, 'host boot ID did not change'
        print('Host rebooted since seed:', rebooted)
    elif phase == 'fail-closed':
        expected = Path('/etc/cell/wall.expected.json')
        backup = expected.with_suffix('.acceptance-backup')
        assert not backup.exists()
        h.guard()
        h.run('systemctl', 'stop', 'cell-agent')
        expected.rename(backup)
        try:
            # Keep the actual host firewall loaded, including proxy protection.
            assert h.run('systemctl', 'start', 'cell-agent', check=False).returncode != 0
            assert h.run('machinectl', 'show', h.MACHINE, check=False).returncode != 0
        finally:
            backup.rename(expected)
            h.run('systemctl', 'reset-failed', 'cell-agent')
            h.run('systemctl', 'start', 'cell-agent')
            h.configure()
        print('Guest startup refused without the approved firewall snapshot; recovery:', verify())
    else:
        raise SystemExit('unknown phase: ' + phase)


if __name__ == '__main__':
    main()
