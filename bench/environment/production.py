#!/usr/bin/env python3
"""Acceptance of actual `cell setup`, on an env-migration disposable VM only.

First install the previous binary, create a chat titled Migration acceptance,
and put 'preserved' in /home/agent/work/migration-marker. Then run the new
setup and this script as root. Credentials must be synthetic. See README.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import shlex
import sys
import time

assert json.loads(Path('/etc/cell/cell.json').read_text())['name'] == 'env-migration', 'disposable fixture only'
spec = importlib.util.spec_from_file_location('environment', '/usr/local/lib/cell/environment.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
RESULTS = []


def case(name, fn):
    print('TEST ' + name, flush=True)
    try:
        detail = fn() or ''
        RESULTS.append(dict(name=name, status='pass', detail=detail))
    except Exception as e:
        RESULTS.append(dict(name=name, status='fail', detail=str(e)))
    Path('/var/lib/cell/production-acceptance.json').write_text(json.dumps(RESULTS, indent=2))
    print(RESULTS[-1], flush=True)


def shell(script, user='agent', timeout=180):
    return h.guest('/bin/bash', '-euc', script, user=user, timeout=timeout).stdout.strip()


def migration():
    assert shell('cat /home/agent/work/migration-marker') == 'preserved'
    before = hashlib.sha256(Path('/home/agent/.pi/agent/auth.json').read_bytes()).hexdigest()
    after = shell('sha256sum /home/agent/.pi/agent/auth.json').split()[0]
    assert before == after, 'credential bytes changed'
    threads = json.loads(h.run('curl', '-fsS', '--unix-socket', '/run/cell-chat/api.sock', '-H',
                              'Host: cell.internal', 'http://cell.internal/api/pi/threads?includeArchived=true').stdout)
    assert any(t['title'] == 'Migration acceptance' for t in threads), threads
    assert Path('/var/lib/cell/migrations/environment-v1/home.tar').exists()
    return 'Workspace marker, credential bytes, chat, and recovery snapshot preserved'


def tools():
    return shell('''sudo -n sh -c 'echo autonomous > /etc/autonomy-marker'
sudo env DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config xz-utils
cd /tmp
curl -fsS --max-time 120 https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init -o rustup-init
curl -fsS --max-time 30 https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init.sha256 -o rustup-init.sha256
sha256sum -c rustup-init.sha256
chmod +x rustup-init
./rustup-init -y --profile minimal --default-toolchain 1.95.0 --no-modify-path
curl -fsS --max-time 120 https://nodejs.org/dist/v22.14.0/node-v22.14.0-linux-x64.tar.xz -o node.tar.xz
curl -fsS --max-time 30 https://nodejs.org/dist/v22.14.0/SHASUMS256.txt -o node-sha.txt
awk '/ node-v22.14.0-linux-x64.tar.xz$/ {print $1 "  node.tar.xz"}' node-sha.txt | sha256sum -c -
sudo tar -xJf node.tar.xz -C /usr/local --strip-components=1
$HOME/.cargo/bin/cargo --version
node --version
npm --version
''', timeout=900)[-500:]


def builds():
    return shell('''export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
mkdir -p "$HOME/autonomy-smoke/src"
cd "$HOME/autonomy-smoke"
printf '[package]\nname="autonomy-smoke"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nitoa="=1.0.15"\n' > Cargo.toml
printf '#[test]\nfn registry_dependency(){assert_eq!(itoa::Buffer::new().format(42),"42");}\n' > src/lib.rs
cargo test
npm init -y
npm install --ignore-scripts --no-audit --no-fund is-number@7.0.0
node -e "require('node:assert/strict').equal(require('is-number')(42), true)"
''', timeout=300)[-500:]


def boundary():
    before = hashlib.sha256(Path('/etc/cell/secret').read_bytes()).hexdigest()
    shell('''test ! -e /etc/cell/secret
test ! -e /proc/1/root/etc/cell/secret
test ! -S /run/host/bus
test ! -S /run/docker.sock
sudo nft flush ruleset
sudo useradd -m boundary-user 2>/dev/null || id boundary-user
''')
    assert hashlib.sha256(Path('/etc/cell/secret').read_bytes()).hexdigest() == before
    h.guard()
    h.run('/usr/local/bin/cell', 'check', timeout=180)
    # Real agent outbox -> host signer; a refusal symlink cannot overwrite key.
    shell('''ln -sf /etc/cell/secret /var/lib/cell/outbox/boundary-forged.refused
printf junk > /var/lib/cell/outbox/boundary-forged.tmp
mv /var/lib/cell/outbox/boundary-forged.tmp /var/lib/cell/outbox/boundary-forged.json
printf '{"fetch":{"url":"https://nodejs.org/"}}' > /var/lib/cell/outbox/boundary-valid.tmp
chmod 644 /var/lib/cell/outbox/boundary-valid.tmp
mv /var/lib/cell/outbox/boundary-valid.tmp /var/lib/cell/outbox/boundary-valid.json
''')
    for _ in range(50):
        if not Path('/var/lib/cell/outbox/boundary-forged.json').exists():
            break
        time.sleep(.1)
    assert 'expected' in Path('/var/lib/cell/outbox/boundary-forged.refused').read_text()
    assert any(p.suffix[1:].isdigit() for p in Path('/var/lib/cell/outbox').glob('boundary-valid.*'))
    assert hashlib.sha256(Path('/etc/cell/secret').read_bytes()).hexdigest() == before
    return 'Root firewall changes, alternate guest UID, absent host key/socket, signed check and refusal-link attack'


def persistence():
    h.run('/usr/local/bin/cell', 'setup', '/etc/cell/cell.json', timeout=900)
    assert shell('cat /etc/autonomy-marker') == 'autonomous'
    shell('$HOME/.cargo/bin/cargo --version; node --version; npm --version')
    migration()
    return 'Production setup reapplied without losing tools, configuration, chat or credentials'


def fail_closed():
    h.run('systemctl', 'stop', 'cell-agent')
    h.run('nft', 'delete', 'table', 'inet', 'cell')
    try:
        assert h.run('systemctl', 'start', 'cell-agent', check=False).returncode != 0
        assert h.run('machinectl', 'show', h.MACHINE, check=False).returncode != 0
    finally:
        h.run('nft', '-f', '/etc/cell/wall.nft')
        h.run('systemctl', 'reset-failed', 'cell-agent')
        h.run('systemctl', 'start', 'cell-agent')
    time.sleep(3)
    h.run('/usr/local/bin/cell', 'check')
    return 'Missing host firewall prevents boot; restored rules recover chat'


if __name__ == '__main__':
    for name, fn in [('migration', migration), ('toolchain-install', tools), ('registry-builds', builds),
                     ('boundary', boundary), ('reapply', persistence), ('missing-wall', fail_closed)]:
        case(name, fn)
    sys.exit(any(r['status'] != 'pass' for r in RESULTS))
