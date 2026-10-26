#!/usr/bin/env python3
"""Acceptance of shared-folder Files after upgrading a disposable cell.

Set up the currently deployed release on a disposable cell named files-acceptance,
with synthetic credentials. Create a chat titled Files acceptance and send it one
prompt, so Pi records its instructions; the reply may fail. Then run the new
binary's setup and this script as root. Reboot and run it again. See README.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess as sp
import sys
import tarfile
import time

assert json.loads(Path('/etc/cell/cell.json').read_text())['name'] == 'files-acceptance', 'disposable fixture only'
spec = importlib.util.spec_from_file_location('environment', '/usr/local/lib/cell/environment.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
SHARED = '/home/agent/shared'
SESSIONS = '/home/agent/.kuggis/chat/pi'
API = ['curl', '-sS', '--unix-socket', '/run/cell-chat/api.sock', '-H', 'Host: cell.internal']
RESULTS = []


def case(name, fn):
    print('TEST ' + name, flush=True)
    try:
        detail = fn() or ''
        RESULTS.append(dict(name=name, status='pass', detail=detail))
    except Exception as e:
        RESULTS.append(dict(name=name, status='fail', detail=str(e)))
    Path('/var/lib/cell/files-acceptance.json').write_text(json.dumps(RESULTS, indent=2))
    print(RESULTS[-1], flush=True)


def shell(script, user='agent', timeout=180):
    return h.guest('/bin/bash', '-euc', script, user=user, timeout=timeout).stdout.strip()


def files(route, path=None, *options):
    return [*API, *options, '--get', *(['--data-urlencode', 'path=' + path] if path else []),
            'http://cell.internal/api/' + route]


def status(route, path):
    return int(h.run(*files(route, path, '-o', '/dev/null', '-w', '%{http_code}')).stdout)


def listing(path=None):
    return json.loads(h.run(*files('files', path, '-f')).stdout)


def setup():
    assert f'--shared {SHARED}' in shell('systemctl cat cell-chat', user='root')
    assert shell(f'stat -c "%U %a" {SHARED}') == 'agent 700'
    return 'Chat service runs with --shared; the folder is private to the agent'


def boundary():
    assert listing()['root'] == SHARED
    shell(f'ln -sfn /home/agent/.pi/agent/auth.json {SHARED}/credentials-link')
    try:
        entries = {e['name']: e for e in listing()['entries']}
        assert entries['credentials-link']['kind'] == 'unavailable', entries['credentials-link']
        for path in ('/home/agent/.pi/agent/auth.json', '/home/agent/.kuggis/chat', '/etc/passwd',
                     f'{SHARED}/../.pi/agent/auth.json', f'{SHARED}/credentials-link',
                     '/home/agent/no-such-file'):
            for route in ('files', 'files/download'):
                assert status(route, path) == 403, (route, path)
        assert status('files', f'{SHARED}/no-such-file') == 404
    finally:
        shell(f'rm -f {SHARED}/credentials-link')
    probes = json.loads(h.run('/usr/bin/python3', '/usr/local/lib/cell/environment.py', 'check',
                              timeout=300).stdout)
    probe = next(p for p in probes if p['name'] == 'host: Files serve only the shared folder')
    assert probe['got'] == 'ok', probe
    return 'Credentials, chat state, system files, escaping links and ../ refused; check probe passes'


def download():
    # About 1 GiB of incompressible data, nested folders and unusual names.
    shell(f'''mkdir -p "{SHARED}/acceptance/round 25/nested"
cd "{SHARED}/acceptance"
for i in $(seq 1 16); do [ -s "round 25/part-$i.bin" ] || head -c 64M /dev/urandom > "round 25/part-$i.bin"; done
printf 'question,forecast\\n1,0.25\\n' > "round 25/nested/résults #1.csv"
''', timeout=600)
    sums = shell(f'cd {SHARED} && find acceptance -type f -print0 | xargs -0 sha256sum')
    expected = {name: digest for digest, name in (line.split('  ', 1) for line in sums.splitlines())}
    start = time.monotonic()
    curl = sp.Popen(files('files/download', f'{SHARED}/acceptance', '-f'), stdout=sp.PIPE)
    got, size = {}, 0
    with tarfile.open(fileobj=curl.stdout, mode='r|gz') as archive:
        for member in archive:
            if member.isfile():
                digest, source = hashlib.sha256(), archive.extractfile(member)
                while chunk := source.read(1 << 20):
                    digest.update(chunk)
                    size += len(chunk)
                got[member.name] = digest.hexdigest()
    assert curl.wait() == 0, 'download did not complete'
    elapsed = time.monotonic() - start
    assert got == expected, sorted(set(got) ^ set(expected))
    name = 'acceptance/round 25/nested/résults #1.csv'
    single = sp.run(files('files/download', f'{SHARED}/{name}', '-f'), stdout=sp.PIPE, check=True).stdout
    assert hashlib.sha256(single).hexdigest() == expected[name]
    mib = size / 2**20
    return f'{mib:.0f} MiB folder in {elapsed:.1f}s ({mib / elapsed:.0f} MiB/s); every file matches'


def cancel():
    # The bracket keeps this probe's own command line from matching.
    running = lambda: shell("grep -las 'Stream an [a]rchive' /proc/[0-9]*/cmdline || true", user='root')
    curl = sp.Popen(files('files/download', f'{SHARED}/acceptance'), stdout=sp.PIPE)
    curl.stdout.read(32 << 20)
    assert running(), 'archive process not found while downloading'
    curl.kill()
    curl.wait()
    start = time.monotonic()
    while running():
        assert time.monotonic() - start < 15, 'archive kept running after the download was cancelled'
        time.sleep(.5)
    return f'Archive process stopped {time.monotonic() - start:.1f}s after the client disconnected'


def prompt():
    threads = json.loads(h.run(*API, '-f', 'http://cell.internal/api/pi/threads?includeArchived=true').stdout)
    thread = next((t for t in threads if t['title'] == 'Files acceptance'), None)
    assert thread, 'fixture chat "Files acceptance" is missing'
    # Opening the chat resumes Pi, which first upgrades its recorded instructions.
    h.run(*API, '-f', '-o', '/dev/null', f'http://cell.internal/api/pi/threads/{thread["id"]}')
    records = [json.loads(line) for line in shell(f'cat {SESSIONS}/*_{thread["id"]}.jsonl').splitlines()
               if line.strip()]
    preambles = [r['message']['sections']['preamble'] for r in records
                 if isinstance(r.get('message'), dict) and r['message'].get('role') == 'system']
    assert preambles, 'fixture chat recorded no instructions; send it one prompt before upgrading'
    for preamble in preambles:
        assert SHARED in preamble, preamble[:300]
        assert preamble.endswith('authority controls are outside your environment.'), preamble[-300:]
    shell(f'ls {SESSIONS}/*_{thread["id"]}.jsonl.before-workspace-prompt-*')
    return 'Resumed chat names the shared folder, keeps its environment instructions and has a backup'


if __name__ == '__main__':
    for name, fn in [('setup', setup), ('boundary', boundary), ('download', download),
                     ('cancel', cancel), ('prompt-upgrade', prompt)]:
        case(name, fn)
    sys.exit(any(r['status'] != 'pass' for r in RESULTS))
