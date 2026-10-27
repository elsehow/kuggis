"""Run only inside the guest namespaces. Never use this to traverse a host rootfs."""
import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unicodedata

HOME = Path('/home/agent')
WORK = HOME / 'workspace'
PROXY = 'http://10.205.0.1:3128'
MAX_BYTES = 16 * 1024**3


def relative(value):
    if (not value or len(value.encode()) > 4096 or '\\' in value
            or any(unicodedata.category(c) == 'Cc' for c in value)
            or any(p in ('', '.', '..') for p in value.split('/'))):
        raise ValueError('invalid guest path')
    return value


def install_file(path, data, mode=0o644):
    """All paths, symlinks and programs here belong to the guest, never the host."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as out:
            out.write(data)
            out.flush()
            os.fsync(out.fileno())
            os.fchmod(out.fileno(), mode)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


WEB_START = '<!-- kuggis:private-web -->'
WEB_END = '<!-- /kuggis:private-web -->'


def web_guidance(mappings):
    lines = [WEB_START, '## Private browser previews',
             'Preview ports are configured by the deployment environment in /etc/kuggis/web.json.',
             'No publish command or deployment credentials are needed for a private preview.', '']
    for web in mappings:
        lines.append(f'- {web["name"]}: guest port {web["guest_port"]}; private host relay port {web["host_port"]}.')
    if mappings:
        lines.extend([
            '',
            'Inspect existing listeners/services before starting another server. Reuse the project server when present.',
            'The host relay reaches 10.205.0.2 at the guest port. A new server should bind 0.0.0.0 or 10.205.0.2.',
            'A server bound only to 127.0.0.1 needs a guest relay; preserve an existing relay if one is already configured.',
            'The user opens the preview on their own computer with `kuggis web CONFIG_OR_NAME` (optionally PREVIEW_NAME or --port LOCAL_PORT).',
            'Do not run that client command inside the cell. Report the preview name and guest port; the client prints the browser URL.',
            'Keep the server running with the project\'s existing service mechanism. Private preview access does not publish publicly.',
        ])
    else:
        lines.append('No private preview ports are configured for this workspace.')
    return '\n'.join([*lines, WEB_END]) + '\n'


def merge_web_guidance(existing, mappings):
    """Replace only our marked block, preserving all other user instructions."""
    block = web_guidance(mappings)
    if WEB_START not in existing and WEB_END not in existing:
        return existing + ('\n\n' if existing else '') + block
    if existing.count(WEB_START) != 1 or existing.count(WEB_END) != 1:
        raise ValueError('ambiguous Kuggis preview guidance markers in AGENTS.md')
    start, end = existing.index(WEB_START), existing.index(WEB_END)
    if end < start:
        raise ValueError('reversed Kuggis preview guidance markers in AGENTS.md')
    return existing[:start] + block.rstrip('\n') + existing[end + len(WEB_END):]


def configure_web(mappings):
    guidance = HOME / 'AGENTS.md'
    updated = merge_web_guidance(guidance.read_text() if guidance.exists() else '', mappings)
    install_file('/etc/kuggis/web.json', (json.dumps(mappings, indent=2) + '\n').encode())
    install_file(guidance, updated.encode())
    os.chown(guidance, 1000, 1000)


def configure(payload):
    if os.geteuid() != 0:
        raise RuntimeError('guest configuration requires guest root')
    WORK.mkdir(parents=True, exist_ok=True)
    os.chown(WORK, 1000, 1000)
    configure_web(payload.get('private_web', []))
    # Root may have been remapped by nspawn. These are guest-visible UIDs.
    install_file('/etc/profile.d/kuggis.sh', (
        f'export HTTP_PROXY={PROXY} HTTPS_PROXY={PROXY}\n'
        'export http_proxy=$HTTP_PROXY https_proxy=$HTTPS_PROXY\n'
        'export NO_PROXY=localhost,127.0.0.1,::1 no_proxy=localhost,127.0.0.1,::1\n'
    ).encode())
    install_file('/etc/apt/apt.conf.d/80kuggis', (
        f'Acquire::http::Proxy "{PROXY}";\nAcquire::https::Proxy "{PROXY}";\n'
        'Acquire::https::Timeout "30";\nAcquire::Retries "2";\n'
    ).encode())
    # Exact destinations are still enforced by the host CONNECT policy.
    install_file('/etc/ssh/ssh_config.d/90-kuggis.conf', (
        'Host *\n  BatchMode yes\n  StrictHostKeyChecking yes\n'
        '  ForwardAgent no\n  ProxyCommand /usr/bin/nc -X connect -x 10.205.0.1:3128 %h %p\n'
    ).encode())
    install_file('/etc/kuggis/launch.json', json.dumps(payload['launch']).encode())
    install_file('/usr/local/bin/kuggis-launch', b'''#!/usr/bin/python3
import json, os
argv = json.load(open('/etc/kuggis/launch.json'))
os.chdir('/home/agent/workspace')
os.execvpe(argv[0], argv, os.environ)
''', 0o755)
    install_file('/etc/kuggis/tmux.conf', b'set -g remain-on-exit on\nset -g exit-empty off\nset -s extended-keys on\n')
    install_file('/etc/systemd/system/kuggis-terminal.service', f'''[Unit]
Description=Workspace terminal (started only on explicit connect)
[Service]
Type=forking
User=agent
Group=agent
WorkingDirectory=/home/agent/workspace
Environment=HOME=/home/agent
Environment=PATH=/home/agent/.local/bin:/home/agent/.cargo/bin:/usr/local/bin:/usr/bin:/bin
Environment=HTTP_PROXY={PROXY} HTTPS_PROXY={PROXY} http_proxy={PROXY} https_proxy={PROXY}
Environment=NO_PROXY=localhost,127.0.0.1,::1 no_proxy=localhost,127.0.0.1,::1
ExecStart=/usr/bin/tmux -L kuggis -f /etc/kuggis/tmux.conf new-session -d -s workspace -c /home/agent/workspace /usr/local/bin/kuggis-launch
ExecStop=/usr/bin/tmux -L kuggis kill-server
Restart=no
UMask=0077
'''.encode())
    # Credentials are already scoped by the deployment environment. Do not log
    # their bytes, return them to the host, or reuse guest-generated paths there.
    for target in payload.get('removed_credentials', []):
        (HOME / relative(target)).unlink(missing_ok=True)
    for secret in payload['credentials']:
        target = HOME / relative(secret['target'])
        install_file(target, base64.b64decode(secret['data'], validate=True), 0o600)
        os.chown(target, 1000, 1000)
        # Only the guest agent can enter the credential directory.
        parent = target.parent
        while parent != HOME:
            os.chown(parent, 1000, 1000)
            os.chmod(parent, 0o700)
            parent = parent.parent
    subprocess.run(['systemctl', 'daemon-reload'], check=True)
    packages = payload.get('packages', [])
    missing = [p for p in packages if subprocess.run(
        ['dpkg-query', '-W', '-f=${Status}', p], capture_output=True).stdout != b'install ok installed']
    if missing:
        env = {**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'}
        subprocess.run(['apt-get', 'update', '-qq'], check=True, env=env)
        subprocess.run(['apt-get', 'install', '-y', '-qq', '--', *missing], check=True, env=env)


def header(stream):
    line = stream.readline(1025)
    if len(line) > 1024 or not line.endswith(b'\n'):
        raise ValueError('invalid transfer header')
    value = json.loads(line)
    if (set(value) != {'size', 'sha256'} or type(value['size']) is not int
            or not 0 <= value['size'] <= MAX_BYTES
            or not isinstance(value['sha256'], str) or len(value['sha256']) != 64
            or any(c not in '0123456789abcdef' for c in value['sha256'])):
        raise ValueError('invalid transfer header')
    return value


def put(path, stream, replace=False):
    target = WORK / relative(path)
    value = header(stream)
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-transfer-', dir=target.parent)
    try:
        with os.fdopen(fd, 'wb') as out:
            remaining = value['size']
            digest = hashlib.sha256()
            while remaining:
                data = stream.read(min(remaining, 65536))
                if not data:
                    raise ValueError('incomplete file transfer')
                out.write(data)
                digest.update(data)
                remaining -= len(data)
            if stream.read(1) or digest.hexdigest() != value['sha256']:
                raise ValueError('transfer checksum or framing mismatch')
            out.flush()
            os.fsync(out.fileno())
        if replace:
            os.replace(temporary, target)
        else:
            os.link(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def get(path, stream):
    # Avoid opening a final symlink or blocking on an agent-controlled FIFO.
    fd = os.open(WORK / relative(path), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_BYTES:
            raise ValueError('source must be a regular file of at most 16 GiB')
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
        source.seek(0)
        stream.write(json.dumps({'size': info.st_size, 'sha256': digest}).encode() + b'\n')
        remaining = info.st_size
        while remaining:
            data = source.read(min(remaining, 65536))
            if not data:
                raise ValueError('source truncated during transfer')
            stream.write(data)
            remaining -= len(data)
        stream.flush()


def main(args):
    if not Path('/run/systemd/container').exists():
        raise RuntimeError('this helper must run inside the guest')
    if args == ['configure']:
        configure(json.load(sys.stdin))
    elif len(args) == 2 and args[0] == 'get':
        get(args[1], sys.stdout.buffer)
    elif len(args) == 3 and args[0] == 'put' and args[1] in ('create', 'replace'):
        put(args[2], sys.stdin.buffer, args[1] == 'replace')
    else:
        raise ValueError('invalid guest operation')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as error:
        print(f'kuggis guest: {type(error).__name__}: {error}', file=sys.stderr)
        sys.exit(1)
