#!/usr/bin/env python3
"""Install/remove a deterministic Pi fixture ONLY on a disposable SSH canary.

Run as host root, passing CELL-CANARY install|restore. Install reads a JSON map
of fake-pi.mjs and fake-auth.mjs source from stdin. Real Git credentials are never
read. Refuses configured model credentials. Restore removes only our drop-in;
the normal production Pi command and chat store remain intact.
"""
import importlib.util
import json
from pathlib import Path
import sys

assert len(sys.argv) == 3
name, phase = sys.argv[1:]
assert name.endswith('-canary') and phase in ('install', 'restore')
assert json.loads(Path('/etc/cell/cell.json').read_text())['name'] == name
assert json.loads(Path('/etc/cell/host.json').read_text())['backend'] == 'ssh'
spec = importlib.util.spec_from_file_location('h', '/usr/local/lib/cell/environment.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
payload = sys.stdin.read() if phase == 'install' else '{}'
h.guest('python3', '-c', r'''
import json
from pathlib import Path
import subprocess
import sys
phase, payload = sys.argv[1:]
dropin = Path('/etc/systemd/system/cell-chat.service.d/99-supervisor-acceptance.conf')
root = Path('/home/agent/.kuggis/supervisor-acceptance')
if phase == 'install':
    auth = Path('/home/agent/.pi/agent/auth.json')
    assert not auth.exists() or not json.loads(auth.read_text()), 'canary must have no model credentials'
    assert not dropin.exists(), 'fixture already installed'
    sources = json.loads(payload)
    assert set(sources) == {'fake-pi.mjs', 'fake-auth.mjs'}
    root.mkdir(mode=0o755, exist_ok=True)
    for name, source in sources.items():
        p = root / name
        if name == 'fake-pi.mjs':
            source = source.replace('#!/usr/bin/env node', '#!/usr/local/lib/cell/pi-1.3.0-node-24.22.1/node/bin/node', 1)
        p.write_text(source)
        p.chmod(0o755)
    unit = Path('/etc/systemd/system/cell-chat.service').read_text()
    command = next(line for line in unit.splitlines() if line.startswith('ExecStart='))
    old = '--pi /usr/local/lib/cell/pi-1.3.0-node-24.22.1/pi'
    assert old in command
    command = command.replace(old, '--pi ' + str(root / 'fake-pi.mjs'))
    dropin.parent.mkdir(exist_ok=True)
    dropin.write_text('[Service]\nExecStart=\n' + command + '\nEnvironment=KUGGIS_FIXTURE_PROVIDER=openai\n')
else:
    dropin.unlink(missing_ok=True)
subprocess.run(['systemctl', 'daemon-reload'], check=True)
subprocess.run(['systemctl', 'restart', 'cell-chat'], check=True)
''', phase, payload, timeout=90)
print('PASS: supervisor chat fixture ' + phase)
