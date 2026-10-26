"""Trusted, operator-supplied bootstrap. Run via pinned SSH as host root.

Only the uploaded binary and declarations are staged. Guest paths are never
read here. A failed setup keeps its staging directory for diagnosis.
"""
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile


def optional_json(path):
    p = Path(path)
    return json.loads(p.read_text()) if p.exists() else None


def observe():
    if os.getuid() != 0:
        raise RuntimeError('host bootstrap requires root through sudo -n')
    release = {}
    for line in Path('/etc/os-release').read_text().splitlines():
        if '=' in line:
            k, v = line.split('=', 1)
            release[k] = v.strip('"')
    resolvers = set(ipaddress.ip_address(line.split()[1])
                    for line in Path('/etc/resolv.conf').read_text().splitlines()
                    if line.split() and line.split()[0] == 'nameserver' and len(line.split()) >= 2)
    dns = [str(ip) for ip in sorted(resolvers, key=lambda ip: (ip.version, int(ip)))]
    memory = next(int(line.split()[1]) * 1024 for line in Path('/proc/meminfo').read_text().splitlines()
                  if line.startswith('MemTotal:'))
    key_path = Path('/etc/cell/key.pub')
    binary = Path('/usr/local/bin/cell')
    checked = None
    for path in Path('/var/lib/cell/said').glob('*.json'):
        statement = json.loads(path.read_text())
        if 'checked' in statement.get('said', {}) and (checked is None or statement['seq'] > checked['seq']):
            checked = statement
    return dict(machine_id=Path('/etc/machine-id').read_text().strip(),
                os=release.get('ID', ''), version=release.get('VERSION_ID', ''),
                init=Path('/proc/1/comm').read_text().strip(),
                arch=platform.machine(), dns=dns, memory_total=memory,
                cell=optional_json('/etc/cell/cell.json'),
                key=key_path.read_text().strip() if key_path.exists() else None,
                runtime=optional_json('/etc/cell/host.json'),
                binary=hashlib.sha256(binary.read_bytes()).hexdigest() if binary.exists() else None,
                checked=checked)


def exact(stream, size):
    result = bytearray()
    while len(result) < size:
        chunk = stream.read(min(size - len(result), 1024 * 1024))
        if not chunk:
            raise RuntimeError('truncated deployment stream')
        result.extend(chunk)
    return result


def apply(stream):
    # One header line followed by precisely the reviewed ELF binary; no tar
    # paths or shell arguments supplied by the target or a mutable guest.
    header = stream.readline(1024 * 1024)
    if not header.endswith(b'\n'):
        raise RuntimeError('missing or oversized deployment header')
    request = json.loads(header)
    size = request['size']
    if not isinstance(size, int) or not 64 <= size <= 256 * 1024 * 1024:
        raise RuntimeError('invalid binary size')
    binary = exact(stream, size)
    if stream.read(1):
        raise RuntimeError('trailing deployment input')
    if hashlib.sha256(binary).hexdigest() != request['binary']:
        raise RuntimeError('uploaded binary differs from the reviewed binary')
    with open('/run/lock/kuggis-host.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if observe() != request['before']:
            raise RuntimeError('host changed after approval; review a new plan')
        stage = Path(tempfile.mkdtemp(prefix='kuggis-host-', dir='/var/tmp'))
        (stage / 'cell').write_bytes(binary)
        (stage / 'cell').chmod(0o700)
        (stage / 'cell.json').write_text(json.dumps(request['cell'], separators=(',', ':')))
        (stage / 'host.json').write_text(json.dumps(request['runtime'], separators=(',', ':')))
        try:
            subprocess.run([str(stage / 'cell'), 'setup', str(stage / 'cell.json'),
                            '--host-runtime', str(stage / 'host.json')], check=True,
                           stdin=subprocess.DEVNULL)
        except Exception:
            print('Setup failed; diagnostic staging retained at ' + str(stage), file=sys.stderr)
            raise
        shutil.rmtree(stage)


if __name__ == '__main__':
    if sys.argv[1:] == ['inspect']:
        print(json.dumps(observe(), sort_keys=True))
    elif sys.argv[1:] == ['apply']:
        apply(sys.stdin.buffer)
    else:
        raise RuntimeError('expected inspect or apply')
