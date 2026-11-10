#!/usr/bin/env python3
"""After native sign-in: require a real tool call and a verified file, not just model prose."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) != 1 or not Path('/run/systemd/container').exists() or os.geteuid() != 1000:
    raise SystemExit('run inside the guest as agent: python3 smoke.py')
with tempfile.TemporaryDirectory(prefix='.codex-smoke-', dir=Path.cwd()) as directory:
    result = subprocess.run(
        [str(Path.home()/'.local/bin/codex'), '-c', 'model_provider="openai"',
         '-c', 'cli_auth_credentials_store="file"', 'exec', '--ephemeral', '--skip-git-repo-check', '--json',
         'Use your shell tool to write exactly kuggis-codex-ok followed by a newline to result.txt, '
         'then cat result.txt to verify it. Reply READY. Do not inspect other files.'],
        env={**os.environ,'CODEX_HOME':str(Path.home()/'.codex')}, cwd=directory,
        stdin=subprocess.DEVNULL, capture_output=True, timeout=180)
    events = [json.loads(line) for line in result.stdout.splitlines() if line.startswith(b'{')]
    calls = [e['item'] for e in events if e.get('type') == 'item.completed'
             and e.get('item',{}).get('type') == 'command_execution' and e['item'].get('exit_code') == 0]
    if result.returncode or not calls or (Path(directory)/'result.txt').read_bytes() != b'kuggis-codex-ok\n':
        raise SystemExit('Codex did not complete the model/tool round trip; '
                         'check kuggis login CELL --agent codex --status')
print(json.dumps({'ok':True,'tool_calls':len(calls),'file_verified':True}))
