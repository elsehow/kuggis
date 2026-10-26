#!/usr/bin/env python3
"""Check private Git reads and dry-run pushes on a seeded SSH canary.

Run as host root: SCRIPT CELL-CANARY OWNER/REPO read|write
Requires ssh_acceptance.py's seed, a scoped credential, and the matching Code
grant. Confirm separately that the repository is private. Never updates remote
refs; write mode tests authentication/negotiation, not server-side update hooks.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

assert len(sys.argv) == 4, 'expected CELL-CANARY OWNER/REPO read|write'
name, repo, mode = sys.argv[1:]
assert name.endswith('-canary') and mode in ('read', 'write')
assert re.fullmatch(r'[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+', repo)
repo = repo.lower()
cell = json.loads(Path('/etc/cell/cell.json').read_text())
assert cell['name'] == name
assert json.loads(Path('/etc/cell/host.json').read_text())['backend'] == 'ssh'
codes = [g['code'] for g in cell['grants'] if 'code' in g]
matching = [c for c in codes if (c.get('via') or c['repo']).lower() == repo]
assert matching and any(c.get('write', False) for c in matching) == (mode == 'write')
denied = 'octocat/spoon-knife'
assert all((c.get('via') or c['repo']).lower() != denied for c in codes)

spec = importlib.util.spec_from_file_location('environment', '/usr/local/lib/cell/environment.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
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

# An independent clone avoids depending on setup's primary-workspace choice.
# Only the temporary guest directory is removed, even if an assertion fails.
h.guest('python3', '-c', r'''
import subprocess
import sys
import tempfile
import uuid

repo, mode, denied = sys.argv[1:]
def git(*args):
    return subprocess.run(['git', *args], capture_output=True, timeout=120)
def success(result, operation):
    assert result.returncode == 0, operation + ' failed (output withheld)'

with tempfile.TemporaryDirectory(prefix='private-git-acceptance-') as directory:
    result = git('clone', '--quiet', '--no-checkout', 'https://github.com/' + repo + '.git', directory)
    success(result, 'private clone')
    success(git('-C', directory, 'fetch', '--quiet'), 'private fetch')
    success(git('ls-remote', 'https://github.int.exe.xyz/' + repo + '.git', 'HEAD'), 'legacy URL read')
    ref = 'refs/heads/kuggis-access-check-' + uuid.uuid4().hex
    def absent():
        result = git('-C', directory, 'ls-remote', '--heads', 'origin', ref)
        success(result, 'remote ref check')
        assert not result.stdout.strip(), 'dry-run target ref unexpectedly exists'
    absent()
    result = git('-C', directory, 'push', '--porcelain', '--dry-run', 'origin', 'HEAD:' + ref)
    if mode == 'write':
        success(result, 'authenticated dry-run push')
        assert b'[new branch]' in result.stdout, 'dry-run did not propose the fresh branch'
    else:
        assert result.returncode != 0 and b'403' in result.stderr, 'read-only push was not refused'
    absent()
result = git('ls-remote', 'https://github.com/' + denied + '.git')
assert result.returncode != 0 and b'403' in result.stderr, 'ungranted repository was not refused'
''', repo, mode, denied, user='agent', timeout=600)

shell('''
sudo -n test ! -e /etc/cell/git-credentials.json
sudo -n test ! -e /proc/1/root/etc/cell/git-credentials.json
sudo -n test ! -e /run/credentials/cell-git.service/github
''')
# Independent host-side probes in only the guest network namespace. Guest
# binaries cannot manufacture the broker's authorization result.
pid = h.run('machinectl', 'show', h.MACHINE, '-p', 'Leader', '--value').stdout.strip()
h.run('nsenter', '-t', pid, '-n', 'python3', '-c', '''
import http.client
import sys
repo, mode, denied = sys.argv[1:]
for target, service, expected in [
    (repo, 'git-upload-pack', 200),
    (repo, 'git-receive-pack', 200 if mode == 'write' else 403),
    (denied, 'git-upload-pack', 403),
]:
    c = http.client.HTTPConnection('10.203.0.1', 3129, timeout=30)
    c.request('GET', '/' + target + '.git/info/refs?service=' + service)
    response = c.getresponse()
    assert response.status == expected, 'unexpected broker authorization result'
    if expected == 200:
        assert response.getheader('Content-Type') == 'application/x-' + service + '-advertisement'
    c.close()
''', repo, mode, denied)
h.run('/usr/local/bin/cell', 'check')
print('PASS: private clone/fetch, legacy URL, ' +
      ('authenticated dry-run push' if mode == 'write' else 'read-only push refusal') +
      ', absent remote test ref, denied repository, credential isolation, and preserved canary state')
