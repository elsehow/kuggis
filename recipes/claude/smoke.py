#!/usr/bin/env python3
"""After native sign-in: require a real tool call and a verified file."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) != 1 or not Path('/run/systemd/container').exists() or os.geteuid() != 1000:
    raise SystemExit('run inside the guest as agent: python3 smoke.py')
with tempfile.TemporaryDirectory(prefix='.claude-smoke-', dir=Path.cwd()) as directory:
    result = subprocess.run(
        [str(Path.home() / '.local/bin/claude'), '-p', '--output-format', 'json',
         '--dangerously-skip-permissions', '--no-session-persistence',
         'Use your shell tool to write exactly kuggis-claude-ok followed by a newline to result.txt, '
         'then cat result.txt to verify it. Reply READY. Do not inspect other files.'],
        cwd=directory, stdin=subprocess.DEVNULL, capture_output=True, timeout=180)
    written = Path(directory) / 'result.txt'
    if result.returncode or not written.is_file() or written.read_bytes() != b'kuggis-claude-ok\n':
        raise SystemExit('Claude Code did not complete the model/tool round trip; '
                         'check kuggis login CELL --agent claude --status')
print(json.dumps({'ok': True, 'file_verified': True}))
