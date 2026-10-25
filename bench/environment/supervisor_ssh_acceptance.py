#!/usr/bin/env python3
"""Live supervisor acceptance, on a separate canary-only supervisor instance.

Run on the supervisor: SCRIPT http://127.0.0.1:18080 CELL-CANARY RECEIPT
Requires ssh_supervisor_fixture.py's deterministic Pi fixture. Restarts ONLY
kuggis-ovh-acceptance.service. Adds/removes example.com through reviewed plans.
Leaves one archived test chat and a private receipt; makes no model calls.
"""
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

assert len(sys.argv) == 4
base, name, receipt = sys.argv[1:]
assert base == 'http://127.0.0.1:18080' and name.endswith('-canary')

def request(path, method='GET', body=None, expected=200):
    req = urllib.request.Request(base + path, method=method,
        data=None if body is None else json.dumps(body).encode(), headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=240) as response:
            status, raw = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, raw = error.code, error.read()
    assert status == expected, (path, status, raw[:500])
    return json.loads(raw) if raw else None

def until(fn, label, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = fn()
            if result:
                return result
        except (ConnectionError, urllib.error.URLError):
            pass
        time.sleep(.3)
    raise AssertionError('timeout: ' + label)

view = request('/api/cells')
assert len(view['cells']) == 1, 'isolated supervisor required'
cell = view['cells'][0]
assert cell['binding'] == name and cell['external_host'] and cell['vm'] is None
prefix = '/api/cells/' + cell['id']
original_grants = cell['grants']
original_threads = request(prefix + '/pi/threads?includeArchived=true')
assert all(t['status'] != 'running' for t in original_threads)
assert request(prefix + '/health')['state'] == 'ready'
assert request(prefix + '/info')['cell'] == name
assert request(prefix + '/settings')['cell'] == name

created = request(prefix + '/pi/threads', 'POST', {'title': 'SSH supervisor acceptance'})
thread = created['metadata']['id']
path = prefix + '/pi/threads/' + thread
chunks = []
def read_events():
    try:
        with urllib.request.urlopen(base + path + '/events', timeout=60) as stream:
            for line in stream:
                if line.startswith(b'data:'):
                    chunks.append(line)
    except (OSError, ValueError):
        pass  # Supervisor restart intentionally closes the original stream.
reader = threading.Thread(target=read_events, daemon=True)
reader.start()
until(lambda: chunks, 'initial SSE snapshot')
prompt = 'slow supervisor persistence ' + 'sample ' * 150
request(path + '/messages', 'POST', {'input': {'content': prompt}}, expected=204)
until(lambda: any(b'text_delta' in line for line in chunks), 'streaming response')
assert request(path)['metadata']['status'] == 'running'
plan = request(prefix + '/plan', 'POST', {'grants': original_grants})
request('/api/changes/' + plan['id'] + '/apply', 'POST', {'approval': plan['approval']}, expected=409)
print('PASS: live SSE deltas and active-run configuration guard', flush=True)

subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'kuggis-ovh-acceptance'], check=True)
until(lambda: request(prefix + '/health')['state'] == 'ready', 'supervisor restart')
snapshot = until(lambda: (s if (s := request(path))['metadata']['status'] == 'idle' else None), 'continued run')
assert len([m for m in snapshot['messages'] if m['role'] == 'user']) == 1
assert [m['role'] for m in snapshot['messages']] == ['user', 'assistant', 'toolResult', 'assistant']
assert any(prompt in json.dumps(m) for m in snapshot['messages'])
assert len(chunks) > 3
print('PASS: supervisor restart preserves the running chat without duplicate messages', flush=True)

request(path, 'PATCH', {'title': 'SSH acceptance preserved'}, expected=204)
request(path + '/archive', 'POST', expected=204)
assert all(t['id'] != thread for t in request(prefix + '/pi/threads'))
request(path + '/unarchive', 'POST', expected=204)
assert request(path)['metadata']['title'] == 'SSH acceptance preserved'

def apply(grants):
    plan = request(prefix + '/plan', 'POST', {'grants': grants})
    result = request('/api/changes/' + plan['id'] + '/apply', 'POST', {'approval': plan['approval']})
    until(lambda: request(prefix + '/health')['state'] == 'ready', 'chat after setup')
    return result

test_grant = {'host': 'example.com'}
assert test_grant not in original_grants
assert apply(original_grants + [test_grant])['actions'] == 1
assert test_grant in request('/api/cells')['cells'][0]['grants']
assert apply(original_grants)['actions'] == 1
assert apply(original_grants)['actions'] == 0
assert request(path)['metadata']['title'] == 'SSH acceptance preserved'
assert request(path)['messages'] == snapshot['messages']
now = request(prefix + '/pi/threads?includeArchived=true')
assert all(any(t['id'] == old['id'] and t['title'] == old['title'] for t in now) for old in original_threads)
request(path + '/archive', 'POST', expected=204)
print('PASS: reviewed grant add/remove, no-op apply, and preserved chat history', flush=True)

result = {'cell': name, 'id': cell['id'], 'thread': thread, 'original_threads': [t['id'] for t in original_threads],
          'sse': True, 'active_run_guard': True, 'supervisor_restart': True, 'grant_changes': True, 'noop': True}
p = Path(receipt)
p.write_text(json.dumps(result, indent=2) + '\n')
p.chmod(0o600)
