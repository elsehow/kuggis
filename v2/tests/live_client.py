#!/usr/bin/env python3
"""Real SSH transfer acceptance; only the disposable v2-acceptance fixture."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

binary, config = map(Path, sys.argv[1:])
if json.loads(config.read_text())['name'] != 'v2-acceptance':
    raise RuntimeError('disposable v2-acceptance workspace required')
prefix = [str(binary.resolve())]
config = str(config.resolve())


def cli(verb, *args, ok=True):
    result = subprocess.run([*prefix, verb, config, *map(str, args)], capture_output=True, timeout=60)
    if (result.returncode == 0) != ok:
        raise RuntimeError(f'{verb}: unexpected exit {result.returncode}: {result.stderr.decode(errors="replace")}')
    return result


with tempfile.TemporaryDirectory(prefix='kuggis-live-') as directory:
    root = Path(directory)
    source, output = root / 'input', root / 'output'
    data = bytes(range(256)) * 4096
    source.write_bytes(data)
    path = "acceptance/a'b.bin"
    cli('put', source, path, '--replace')
    cli('get', path, output)
    assert output.read_bytes() == data
    source.write_bytes(b'replacement\x00\xff')
    cli('put', source, path, ok=False)
    cli('get', path, output, ok=False)
    assert output.read_bytes() == data
    cli('put', source, path, '--replace')
    cli('get', path, output, '--replace')
    assert output.read_bytes() == source.read_bytes()
    # Send a truncated frame through the actual SSH command and guest helper.
    partial = 'acceptance/interrupted-' + root.name
    invocation = json.loads(cli('ssh-command', 'put', partial).stdout)
    frame = json.dumps({'size': 100, 'sha256': hashlib.sha256(b'x' * 100).hexdigest()}).encode() + b'\nxxx'
    result = subprocess.run([invocation['program'], *invocation['args']], input=frame, capture_output=True, timeout=60)
    assert result.returncode != 0, 'truncated upload unexpectedly succeeded'
    cli('get', partial, root / 'partial', ok=False)
    assert not (root / 'partial').exists()

print(json.dumps({'ok': True, 'checks': ['binary-round-trip', 'quoted-path', 'create-only', 'explicit-replacement', 'interrupted-upload-not-published']}))
