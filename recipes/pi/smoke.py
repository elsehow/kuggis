#!/usr/bin/env python3
"""One real model/tool round trip, using only disposable files in the guest."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) != 2 or not Path('/run/systemd/container').exists() or os.geteuid() != 1000:
    raise SystemExit('run inside the guest as agent: python3 smoke.py PROVIDER/MODEL')
model = sys.argv[1]
binary = Path.home() / '.local/bin/pi'
with tempfile.TemporaryDirectory(prefix='.pi-smoke-', dir=Path.cwd()) as directory:
    result = subprocess.run(
        [str(binary), '--offline', '--model', model, '--thinking', 'low', '--no-session', '--mode', 'json',
         'Use the write tool to create result.txt containing exactly kuggis-pi-ok followed by a newline. '
         'Then use the bash tool to cat result.txt and verify it. Reply READY. Do not inspect other files.'],
        cwd=directory, stdin=subprocess.DEVNULL, capture_output=True, timeout=180)
    if result.returncode:
        raise SystemExit(f'Pi failed with status {result.returncode}; check model authentication and endpoint grants')
    events = [json.loads(line) for line in result.stdout.splitlines() if line.startswith(b'{')]
    tools = sorted({event['toolName'] for event in events if event.get('type') == 'tool_execution_start'})
    if not {'write', 'bash'} <= set(tools) or (Path(directory) / 'result.txt').read_bytes() != b'kuggis-pi-ok\n':
        raise SystemExit('Pi did not complete the requested write/bash round trip')
print(json.dumps({'ok': True, 'model': model, 'tool_calls': tools, 'file_verified': True}))
