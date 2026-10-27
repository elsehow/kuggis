#!/usr/bin/python3
"""Effects for Rust's declarative deployment. Ubuntu 24.04, one guest per host.

Never traverse a mutable guest rootfs from the host. The only chroot/write into
that tree occurs during the initial trusted debootstrap, before its first boot.
"""
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess as sp
import sys
import tempfile
import time

ETC = Path('/etc/kuggis')
DATA = Path('/var/lib/kuggis')
RUNTIME = Path('/usr/local/lib/kuggis-v2')
CONFIG = ETC / 'workspace.json'
ROOT = DATA / 'rootfs'
MACHINE = 'kuggis-v2'
IFACE = 'kv2-host'
HOST = '10.205.0.1'
PROXY = 'http://10.205.0.1:3128'
ENVIRONMENT = 'kuggis-v2.service'
BASE_UNITS = ['kuggis-v2-wall.service', 'kuggis-v2-proxy.service', ENVIRONMENT]


def run(*args, data=None, timeout=180, check=True):
    result = sp.run([str(a) for a in args], input=data, stdin=sp.DEVNULL if data is None else None,
                    capture_output=True, timeout=timeout,
                    env={**os.environ, 'PATH': '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
                         'DEBIAN_FRONTEND': 'noninteractive'})
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} failed with exit status {result.returncode}')
    return result


def trusted_bytes(path, secret=False):
    """Read a root-owned regular file through protected, nonsymlink ancestors."""
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('expected an absolute trusted file path')
    descriptor = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
            info = os.fstat(descriptor)
            if info.st_uid != 0 or info.st_mode & 0o022:
                raise RuntimeError('unprotected configuration ancestor')
        file = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=descriptor)
        with os.fdopen(file, 'rb') as source:
            info = os.fstat(source.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0
                    or info.st_mode & (0o077 if secret else 0o022) or info.st_size > 1024 * 1024):
                raise RuntimeError('configuration/credential file has unsafe ownership, mode or size')
            return source.read(1024 * 1024 + 1)
    finally:
        os.close(descriptor)


def safe_directory(path, mode=None):
    path = Path(path)
    path.mkdir(parents=True, exist_ok=True, mode=mode or 0o755)
    # Managed directories must not have been planted by an untrusted local user.
    for ancestor in [path, *path.parents]:
        info = ancestor.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise RuntimeError('unsafe host installation directory')
    if mode is not None:
        os.chmod(path, mode)


def atomic_write(path, content, mode=0o644):
    path = Path(path)
    safe_directory(path.parent)
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as out:
            out.write(content)
            out.flush()
            os.fsync(out.fileno())
            os.fchmod(out.fileno(), mode)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def load():
    return json.loads(trusted_bytes(CONFIG))


def web_units(workspace):
    return [f'kuggis-v2-web-{web["name"]}.{kind}'
            for web in workspace.get('private_web', []) for kind in ('socket', 'service')]


def guest_command(argv, user='agent', terminal=False):
    return ['systemd-run', '--machine=' + MACHINE, '--quiet', '--wait', '--collect',
            '--pty' if terminal else '--pipe', '--expand-environment=no',
            '--service-type=exec', '--uid=' + user, '--working-directory=/home/agent/workspace',
            '--setenv=HOME=/home/agent', '--setenv=PATH=/home/agent/.local/bin:/home/agent/.cargo/bin:/usr/local/bin:/usr/bin:/bin',
            '--setenv=HTTP_PROXY=' + PROXY, '--setenv=HTTPS_PROXY=' + PROXY,
            '--setenv=http_proxy=' + PROXY, '--setenv=https_proxy=' + PROXY,
            '--setenv=NO_PROXY=localhost,127.0.0.1,::1',
            '--setenv=no_proxy=localhost,127.0.0.1,::1', '--', *argv]


def guest(*args, user='agent', data=None, timeout=180):
    return run(*guest_command(args, user=user), data=data, timeout=timeout)


def ready():
    for _ in range(60):
        try:
            guest('/usr/bin/true', timeout=5)
            return
        except (RuntimeError, sp.TimeoutExpired):
            time.sleep(1)
    raise RuntimeError('guest did not become ready')


def normalized():
    live = run('nft', '-j', 'list', 'table', 'inet', 'kuggis_v2').stdout
    return run(RUNTIME / 'kuggis', 'normalize-nft', data=live).stdout


def guard(installing=False):
    if (ETC / 'updating').exists():
        if not installing:
            raise RuntimeError('workspace setup is incomplete')
        # A boot during a crashed/incomplete update must fail. Only a currently
        # running installer, holding the host lock, may start a guest to configure.
        with open(ETC / 'setup.lock', 'rb') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                pass
            else:
                raise RuntimeError('incomplete update has no active installer')
    if normalized() != trusted_bytes(ETC / 'wall.expected.json'):
        raise RuntimeError('live firewall differs from deployment configuration')


def network():
    for _ in range(100):
        if run('ip', 'link', 'show', IFACE, check=False).returncode == 0:
            break
        time.sleep(.1)
    else:
        raise RuntimeError('workspace interface did not appear')
    run('ip', 'address', 'replace', HOST + '/30', 'dev', IFACE)
    run('ip', 'link', 'set', IFACE, 'up')


def initialize(workspace):
    if (DATA / 'initialized').exists():
        if not ROOT.is_dir():
            raise RuntimeError('initialized workspace storage is missing')
        return
    stage = DATA / 'rootfs.build'
    if stage.exists() or ROOT.exists():
        raise RuntimeError('partial initial bootstrap exists; inspect it before retrying')
    if run('machinectl', 'show', MACHINE, check=False).returncode == 0:
        raise RuntimeError('refusing host writes into an already running guest')
    # Bootstrap only the base runtime from main. Declarative workspace packages
    # are installed by guest.configure using its main+universe apt sources,
    # exactly as on subsequent setup calls.
    packages = ['systemd', 'systemd-sysv', 'dbus', 'ca-certificates', 'curl', 'python3', 'sudo',
                'iproute2', 'nftables', 'git', 'openssh-client', 'netcat-openbsd', 'tmux']
    run('debootstrap', '--variant=minbase', '--include=' + ','.join(sorted(set(packages))),
        'noble', stage, 'https://ports.ubuntu.com/ubuntu-ports' if platform.machine() == 'aarch64'
        else 'https://archive.ubuntu.com/ubuntu', timeout=1200)
    run('chroot', stage, 'useradd', '-m', '-u', '1000', '-s', '/bin/bash', 'agent')
    # This tree has never been booted or exposed to an agent. After initialized,
    # all configuration and file operations must go through guest namespaces.
    def initial(path, content, mode=0o644):
        target = stage / path.lstrip('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content.encode())
        target.chmod(mode)
    initial('/etc/sudoers.d/agent', 'Defaults:agent env_keep += "HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy"\nagent ALL=(ALL) NOPASSWD: ALL\n', 0o440)
    mirror = 'https://ports.ubuntu.com/ubuntu-ports' if platform.machine() == 'aarch64' else 'https://archive.ubuntu.com/ubuntu'
    initial('/etc/apt/sources.list', f'deb {mirror} noble main universe\ndeb {mirror} noble-updates main universe\ndeb {mirror} noble-security main universe\n')
    initial('/etc/systemd/system/kuggis-network.service', '''[Unit]
Description=Workspace interface
Before=network.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/ip link set eth0 up
ExecStart=/usr/sbin/ip address replace 10.205.0.2/30 dev eth0
ExecStart=/usr/sbin/ip route replace default via 10.205.0.1
[Install]
WantedBy=multi-user.target
''')
    wants = stage / 'etc/systemd/system/multi-user.target.wants'
    wants.mkdir(parents=True, exist_ok=True)
    (wants / 'kuggis-network.service').symlink_to('../kuggis-network.service')
    for unit in ('systemd-networkd.service', 'systemd-networkd.socket', 'systemd-resolved.service'):
        path = stage / 'etc/systemd/system' / unit
        path.unlink(missing_ok=True)
        path.symlink_to('/dev/null')
    (stage / 'etc/resolv.conf').unlink(missing_ok=True)
    initial('/etc/resolv.conf', 'nameserver 1.1.1.1\noptions timeout:1 attempts:1\n')
    (stage / 'run/kuggis-runtime').mkdir(parents=True, exist_ok=True)
    (stage / 'home/agent/workspace').mkdir(parents=True, exist_ok=True)
    os.chown(stage / 'home/agent/workspace', 1000, 1000)
    stage.rename(ROOT)
    atomic_write(DATA / 'initialized', b'2\n', 0o600)


def credentials(workspace):
    return [{'target': credential['target'],
             'data': base64.b64encode(trusted_bytes(credential['source'], secret=True)).decode()}
            for resource in workspace.get('resources', []) for credential in resource.get('credentials', [])]


def verify_bundle(bundle):
    if bundle['version'] != 2 or bundle['workspace']['version'] != 2:
        raise ValueError('unsupported deployment version')
    name = bundle['workspace']['name']
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,47}', name) or name.endswith('-'):
        raise ValueError('invalid workspace name')
    # Artifacts are compiler output; only these paths belong to this installer.
    fixed = {'/usr/local/libexec/kuggis-host', str(RUNTIME / 'guest.py'), '/etc/kuggis/wall.nft',
             '/etc/systemd/network/00-kuggis-v2.network',
             *('/etc/systemd/system/' + unit for unit in BASE_UNITS)}
    allowed = fixed | {'/etc/systemd/system/' + unit for unit in web_units(bundle['workspace'])}
    if set(bundle['files']) != allowed:
        raise ValueError('unexpected or missing deployment artifact')
    for path, file in bundle['files'].items():
        if '..' in Path(path).parts or file['mode'] not in (0o600, 0o644, 0o755):
            raise ValueError('unsafe deployment artifact')


def install(stream):
    line = stream.readline(1024 * 1024 + 1)
    if len(line) > 1024 * 1024 or not line.endswith(b'\n'):
        raise ValueError('invalid installation header')
    request = json.loads(line)
    bundle = request['bundle']
    verify_bundle(bundle)
    workspace = bundle['workspace']
    size = request['binary_size']
    if type(size) is not int or not 0 < size <= 64 * 1024 * 1024:
        raise ValueError('invalid binary size')
    release = dict(line.split('=', 1) for line in Path('/etc/os-release').read_text().splitlines() if '=' in line)
    if release.get('ID', '').strip('"') != 'ubuntu' or release.get('VERSION_ID', '').strip('"') != '24.04':
        raise RuntimeError('v2 currently requires Ubuntu 24.04')
    if Path('/etc/cell').exists():
        raise RuntimeError('refusing to install over a v1 cell; use a dedicated host')
    secrets = credentials(workspace)  # Validate sources before stopping a running workspace.
    with tempfile.TemporaryDirectory(prefix='kuggis-install-') as staging:
        binary = Path(staging) / 'kuggis'
        digest = hashlib.sha256()
        with binary.open('wb') as out:
            remaining = size
            while remaining:
                chunk = stream.read(min(remaining, 65536))
                if not chunk:
                    raise ValueError('incomplete installation upload')
                digest.update(chunk)
                out.write(chunk)
                remaining -= len(chunk)
        if stream.read(1) or digest.hexdigest() != request['sha256']:
            raise ValueError('installation checksum mismatch')
        with binary.open('rb') as source:
            elf = source.read(20)
        if len(elf) < 20 or elf[:6] != b'\x7fELF\x02\x01':
            raise ValueError('setup needs a 64-bit little-endian Linux binary')
        expected = {'x86_64': 62, 'aarch64': 183}.get(platform.machine())
        if expected is None or int.from_bytes(elf[18:20], 'little') != expected:
            raise ValueError('Linux binary architecture does not match the host')
        binary.chmod(0o755)
        # Re-parse through Rust so the host does not maintain a second schema.
        config = Path(staging) / 'workspace.json'
        config.write_text(json.dumps(workspace))
        run(binary, 'validate', config)
        if json.loads(run(binary, 'bundle', config).stdout) != bundle:
            raise RuntimeError('client configuration compiler and supplied Linux binary differ')
        safe_directory(ETC)
        safe_directory(DATA, 0o700)
        lock = os.open(ETC / 'setup.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(lock, 'wb') as locked:
            fcntl.flock(locked, fcntl.LOCK_EX | fcntl.LOCK_NB)
            old = load() if CONFIG.exists() else None
            if old and old['name'] != workspace['name']:
                raise RuntimeError('this host already belongs to another workspace')
            previous = web_units(old) if old else []
            run('systemctl', 'stop', *previous, ENVIRONMENT, 'kuggis-v2-proxy.service', check=False)
            # Fresh Ubuntu images need systemd-container installed below before
            # machinectl exists. An existing deployment must still be checked.
            if (old is not None or shutil.which('machinectl') is not None) and run('machinectl', 'show', MACHINE, check=False).returncode == 0:
                raise RuntimeError('workspace did not stop; refusing to modify a live boundary')
            atomic_write(ETC / 'updating', b'2\n', 0o600)
            try:
                # exe images may include a host-side coding agent. The trusted
                # host must not run agent workloads outside this guest boundary.
                for unit in ('shelley.socket', 'shelley.service'):
                    state = run('systemctl', 'show', unit, '-p', 'LoadState', '--value', check=False).stdout.strip()
                    if state == b'loaded':
                        run('systemctl', 'disable', '--now', unit)
                run('apt-get', 'update', '-qq', timeout=600)
                run('apt-get', 'install', '-y', '-qq', 'systemd-container', 'debootstrap', 'nftables', 'python3', 'iproute2', 'util-linux', 'curl', timeout=900)
                if run('id', 'kuggis-proxy', check=False).returncode:
                    run('useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', 'kuggis-proxy')
                for unit in previous:
                    if unit not in web_units(workspace):
                        run('systemctl', 'disable', unit, check=False)
                        (Path('/etc/systemd/system') / unit).unlink(missing_ok=True)
                for path, file in bundle['files'].items():
                    atomic_write(path, file['content'].encode(), file['mode'])
                atomic_write(RUNTIME / 'kuggis', binary.read_bytes(), 0o755)
                atomic_write(CONFIG, json.dumps(workspace, indent=2).encode() + b'\n')
                atomic_write(ETC / 'deployment.json', json.dumps(bundle).encode(), 0o600)
                run('nft', '-f', ETC / 'wall.nft')
                atomic_write(ETC / 'wall.expected.json', normalized(), 0o600)
                initialize(workspace)
                run('systemctl', 'daemon-reload')
                run('systemctl', 'enable', *BASE_UNITS)
                run('systemctl', 'restart', 'kuggis-v2-wall.service', 'kuggis-v2-proxy.service', ENVIRONMENT)
                ready()
                old_targets = {c['target'] for r in (old or {}).get('resources', []) for c in r.get('credentials', [])}
                new_targets = {c['target'] for c in secrets}
                payload = {'launch': workspace['launch'], 'packages': workspace.get('packages', []), 'credentials': secrets,
                           'private_web': workspace.get('private_web', []),
                           'removed_credentials': sorted(old_targets - new_targets)}
                guest('/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'configure', user='root',
                      data=json.dumps(payload).encode(), timeout=900)
                sockets = [unit for unit in web_units(workspace) if unit.endswith('.socket')]
                if sockets:
                    run('systemctl', 'enable', '--now', *sockets)
                report = check(installing=True)
                if not report['ok']:
                    raise RuntimeError('post-install boundary checks failed')
                (ETC / 'updating').unlink()
                print(json.dumps(report))
            except BaseException:
                atomic_write(ETC / 'updating', b'failed\n', 0o600)
                run('systemctl', 'stop', *web_units(workspace), ENVIRONMENT, 'kuggis-v2-proxy.service', check=False)
                raise


def check(installing=False):
    checks = []
    def case(name, operation):
        try:
            operation()
            checks.append({'name': name, 'passed': True})
        except Exception as error:
            checks.append({'name': name, 'passed': False, 'detail': str(error)})
    case('host firewall', lambda: guard(installing=installing))
    def namespaces():
        pid = int(run('machinectl', 'show', MACHINE, '-p', 'Leader', '--value').stdout)
        if Path(f'/proc/{pid}/uid_map').read_text().split() != ['0', '524288', '65536']:
            raise RuntimeError('unexpected guest UID mapping')
        for namespace in ('user', 'mnt', 'pid', 'net'):
            if os.readlink(f'/proc/{pid}/ns/{namespace}') == os.readlink(f'/proc/1/ns/{namespace}'):
                raise RuntimeError('guest shares a host namespace')
    case('guest namespaces', namespaces)
    case('proxy service', lambda: run('systemctl', 'is-active', '--quiet', 'kuggis-v2-proxy.service'))
    def probes():
        pid = int(run('machinectl', 'show', MACHINE, '-p', 'Leader', '--value').stdout)
        code = '''import socket
def refused(host, port):
 try:
  sock = socket.create_connection((host, port), 2)
 except OSError:
  return
 sock.close()
 raise RuntimeError('direct guest connection escaped the boundary')
for host, port in [('1.1.1.1',443), ('169.254.169.254',80), ('10.205.0.1',22), ('2606:4700:4700::1111',443)]:
 refused(host,port)
sock=socket.create_connection(('10.205.0.1',3128),3)
sock.settimeout(3)
sock.sendall(b'CONNECT not-configured.invalid:65535 HTTP/1.1\\r\\n\\r\\n')
if b' 403 ' not in sock.recv(1024).split(b'\\r\\n')[0]:
 raise RuntimeError('unconfigured proxy target was not refused')
sock.close()
'''
        # Only switch network namespace: Python, source, filesystem and output
        # stay host-controlled, so guest root cannot forge a passing probe.
        run('nsenter', '-t', str(pid), '-n', '--', '/usr/bin/python3', '-c', code, timeout=30)
    case('guest network probes', probes)
    return {'ok': all(item['passed'] for item in checks), 'checks': checks}


def dispatch(args):
    if sys.platform != 'linux' or os.geteuid() != 0:
        raise RuntimeError('host operations require root on Linux')
    if args == ['install']:
        install(sys.stdin.buffer)
        return
    if args == ['guard']:
        guard(installing=True)
        return
    if args == ['network']:
        network()
        return
    if len(args) < 2 or args[1] != load()['name']:
        raise ValueError('workspace does not belong to this host')
    verb, _, *rest = args
    if verb == 'check' and not rest:
        report = check()
        print(json.dumps(report))
        if not report['ok']:
            raise RuntimeError('workspace boundary check failed')
        return
    guard()
    if verb == 'connect' and not rest:
        # Persistent server belongs to its own guest service, not to this SSH
        # connection's transient cgroup. Do not enable/replay it at guest boot.
        guest('systemctl', 'start', 'kuggis-terminal.service', user='root')
        command = guest_command(['/usr/bin/tmux', '-L', 'kuggis', 'attach-session', '-t', 'workspace'], terminal=True)
    elif verb == 'shell' and not rest:
        command = guest_command(['/bin/bash', '-l'], terminal=True)
    elif verb == 'exec' and len(rest) >= 2 and rest[0] == '--':
        if not rest[1] or rest[1].startswith('-') or any('\0' in arg for arg in rest[1:]):
            raise ValueError('expected a guest executable and argv')
        command = guest_command(rest[1:])
    elif verb == 'get' and len(rest) == 2 and rest[0] == '--':
        command = guest_command(['/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'get', rest[1]])
    elif verb == 'put' and len(rest) in (2, 3):
        replace = len(rest) == 3 and rest[0] == '--replace'
        if (rest[1:] if replace else rest)[0] != '--' or len(rest) != (3 if replace else 2):
            raise ValueError('invalid upload arguments')
        command = guest_command(['/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'put', 'replace' if replace else 'create', rest[-1]])
    else:
        raise ValueError('unsupported host operation')
    os.execv('/usr/bin/systemd-run', command)


if __name__ == '__main__':
    try:
        dispatch(sys.argv[1:])
    except Exception as error:
        print(f'kuggis host: {type(error).__name__}: {error}', file=sys.stderr)
        sys.exit(1)
