#!/usr/bin/env python3
"""Test-only transport: execute guest file operations in a temporary directory.

Never invokes sudo, the host installer, a real SSH client or a network endpoint.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import sys

words = shlex.split(sys.argv[-1])
if 'KUGGIS_TEST_LOG' in os.environ:
    with open(os.environ['KUGGIS_TEST_LOG'], 'a') as log:
        log.write(json.dumps(words[:4] if words[0] == 'share' else ['install' if words[3:5] == ['/usr/bin/python3', '-c'] else words[4]]) + '\n')
if words[0] == 'share':
    if sys.argv[-2] != 'exe.dev' or words[1] not in ('set-private', 'port'):
        raise RuntimeError('unexpected provider operation')
    print(json.dumps({'error': 'fixture refusal'} if os.environ.get('KUGGIS_TEST_EXE_FAIL') else {'ok': True}))
    sys.exit(0)
if words[:3] != ['sudo', '-n', '--']:
    raise RuntimeError('unexpected SSH command')
if words[3:5] == ['/usr/bin/python3', '-c']:
    request = json.loads(sys.stdin.buffer.readline())
    binary = sys.stdin.buffer.read()
    if len(binary) != request['binary_size'] or hashlib.sha256(binary).hexdigest() != request['sha256']:
        raise RuntimeError('installation framing mismatch')
    if '/etc/kuggis/wall.nft' not in request['bundle']['files']:
        raise RuntimeError('missing compiled boundary')
    print(json.dumps({'test_transport_only': True}))
    sys.exit(0)
if words[3] != '/usr/local/libexec/kuggis-host':
    raise RuntimeError('unexpected host helper')
spec = importlib.util.spec_from_file_location('guest', os.environ['KUGGIS_TEST_GUEST'])
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)
guest.WORK = Path(os.environ['KUGGIS_TEST_WORKSPACE'])
verb = words[4]
if verb == 'put':
    guest.put(words[-1], sys.stdin.buffer, '--replace' in words[6:])
elif verb == 'get':
    guest.get(words[-1], sys.stdout.buffer)
else:
    raise RuntimeError('test transport supports only transfers and install framing')
sys.stdout.flush()
sys.exit(int(os.environ.get('KUGGIS_TEST_EXIT_STATUS', '0')))
