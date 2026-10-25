#!/usr/bin/env python3
"""Verify the public-read Git upgrade on the disposable SSH canary.

Requires the earlier ssh_acceptance.py seed and a read-only Code grant for
octocat/hello-world. Run as host root, passing the exact canary cell name.
No credentials or remote writes are performed.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import sys

cell = json.loads(Path('/etc/cell/cell.json').read_text())
assert len(sys.argv) == 2 and cell['name'] == sys.argv[1] and sys.argv[1].endswith('-canary')
assert json.loads(Path('/etc/cell/host.json').read_text())['backend'] == 'ssh'
assert [g['code'] for g in cell['grants'] if 'code' in g] == [{'repo': 'octocat/hello-world', 'write': False}]
assert json.loads(Path('/etc/cell/git-credentials.json').read_text()) == {}, 'anonymous fixture only'
spec = importlib.util.spec_from_file_location('environment', '/usr/local/lib/cell/environment.py')
h = importlib.util.module_from_spec(spec); spec.loader.exec_module(h)
saved = json.loads(Path('/var/lib/cell/ssh-acceptance.json').read_text())
for path in ('/etc/machine-id', '/etc/cell/secret', '/etc/cell/key.pub', '/etc/cell/host.json'):
    assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == saved['identity'][path], path

def shell(script):
    return h.guest('/bin/bash', '-euc', script, user='agent').stdout.strip()

assert shell('cat /home/agent/work/ssh-acceptance-marker') == 'workspace preserved'
assert shell('cat /etc/ssh-acceptance-marker') == 'guest configuration preserved'
assert shell('jq --version') == saved['jq']
threads = json.loads(h.run('curl', '-fsS', '--unix-socket', '/run/cell-chat/api.sock',
                          '-H', 'Host: cell.internal', 'http://cell.internal/api/pi/threads').stdout)
assert any(t['id'] == saved['thread'] and t['status'] == 'idle' for t in threads)
shell('''
test -d /home/agent/work/octocat/hello-world/.git
git -C /home/agent/work/octocat/hello-world fetch --quiet
git ls-remote https://github.com/octocat/Hello-World.git HEAD >/dev/null
git ls-remote https://github.int.exe.xyz/octocat/hello-world.git HEAD >/dev/null
''')
h.guest('python3', '-c', '''import subprocess
commands = [
 ['git', 'ls-remote', 'https://github.com/octocat/Spoon-Knife.git'],
 ['git', '-C', '/home/agent/work/octocat/hello-world', 'push', '--dry-run', 'origin', 'HEAD:refs/heads/kuggis-scope-test'],
]
for command in commands:
 r = subprocess.run(command, capture_output=True, timeout=30)
 assert r.returncode != 0 and b'403' in r.stderr, 'scope refusal missing'
''', user='agent')
shell('''
sudo -n test ! -e /etc/cell/git-credentials.json
sudo -n test ! -e /proc/1/root/etc/cell/git-credentials.json
sudo -n test ! -e /run/credentials/cell-git.service/github
''')
# Trusted host Python in only the guest network namespace tests the old broad
# CONNECT route; mutable guest binaries cannot manufacture this result.
pid = h.run('machinectl', 'show', h.MACHINE, '-p', 'Leader', '--value').stdout.strip()
h.run('nsenter', '-t', pid, '-n', 'python3', '-c', '''import socket
for host in ['github.com', 'github.int.exe.xyz']:
 s = socket.create_connection(('10.203.0.1', 3128), 3)
 s.settimeout(10)
 s.sendall(('CONNECT ' + host + ':443 HTTP/1.1\\r\\nHost: ' + host + ':443\\r\\n\\r\\n').encode())
 assert b' 403 ' in s.recv(1024), 'unscoped GitHub tunnel allowed'
 s.close()
''')
h.run('/usr/local/bin/cell', 'check')
print('PASS: public clone/fetch, standard and legacy URL rewrites, denied repository and push, private credentials, blocked broad tunnels, and upgrade persistence')
