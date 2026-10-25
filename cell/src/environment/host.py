"""Host-only lifecycle for a persistent, user-namespaced cell environment.

No command from a mutable rootfs is ever executed in the host namespaces.
Only the initial, never-booted rootfs is traversed or written from the host.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess as sp
import sys
import tempfile
import time

ETC = Path('/etc/cell')
LIB = Path('/usr/local/lib/cell')
ROOT = Path('/var/lib/machines/cell-env')
READY = ETC / 'environment-initialized'
SPEC = json.loads((ETC / 'environment.json').read_text())
MACHINE = SPEC['machine']
IFACE = SPEC['interface']
HOST = SPEC['host_ip']
PROXY = 'http://' + HOST + ':3128'
BASE = SPEC['uid_base']
RUNTIME = json.loads((ETC / 'host.json').read_text()) if (ETC / 'host.json').exists() else {'backend': 'exe_dev'}


def run(*args, timeout=180, check=True, **kw):
    result = sp.run([str(a) for a in args], text=True, capture_output=True,
                    timeout=timeout, **kw)
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} exited {result.returncode}: {result.stderr[-3000:]}')
    return result


def write(path, content, mode=0o644):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + '.new')
    tmp.write_text(content)
    tmp.chmod(mode)
    tmp.replace(path)


def guest(*args, user='root', timeout=180):
    return run('systemd-run', '--machine=' + MACHINE, '--quiet', '--wait', '--pipe',
               '--expand-environment=no', '--property=RuntimeMaxSec=' + str(timeout),
               '--uid=' + user, '/usr/bin/env',
               'HOME=' + ('/home/agent' if user == 'agent' else '/root'),
               'HTTPS_PROXY=' + PROXY, 'HTTP_PROXY=' + PROXY,
               'https_proxy=' + PROXY, 'http_proxy=' + PROXY,
               'NO_PROXY=localhost,127.0.0.1,::1', 'no_proxy=localhost,127.0.0.1,::1',
               *args, timeout=timeout + 10)


def normalize(value):
    if isinstance(value, list):
        return [normalize(v) for v in value if not isinstance(v, dict) or 'metainfo' not in v]
    if isinstance(value, dict):
        return {k: ({} if k == 'counter' else normalize(v)) for k, v in value.items()
                if k not in ('handle', 'index')}
    return value


def policy(value):
    # Chain creation order can differ when migrating an existing table versus
    # loading it on a clean boot. Preserve rule order *within* every chain.
    entries = normalize(value)['nftables']
    return sorted(entries, key=lambda entry: (
        next(iter(entry)),
        json.dumps(entry) if 'rule' not in entry else entry['rule']['chain']))


def guard():
    live = json.loads(run('nft', '-j', 'list', 'table', 'inet', 'cell').stdout)
    wanted = json.loads((ETC / 'wall.expected.json').read_text())
    if policy(live) != policy(wanted):
        raise RuntimeError('live firewall differs from the host-approved boundary')


def network():
    for _ in range(100):
        if run('ip', 'link', 'show', IFACE, check=False).returncode == 0:
            break
        time.sleep(.1)
    run('ip', 'address', 'replace', HOST + '/30', 'dev', IFACE)
    run('ip', 'link', 'set', IFACE, 'up')


def socket_dir():
    p = Path('/run/cell-chat')
    p.mkdir(exist_ok=True)
    # Only this small directory is shared, never the host /run or system bus.
    os.chown(p, BASE + 1000, BASE + 1000)
    p.chmod(0o770)
    run('setfacl', '-m', 'g:cell-chat:rwx,d:g:cell-chat:rwx,d:u::rwx,d:g::rwx,d:o::---', p)
    # The publishing bridge's socket and public configuration. The guest binds
    # this directory, so it must exist before, and outlive, the bridge.
    p = Path('/run/cell-publish')
    p.mkdir(exist_ok=True)
    shutil.chown(p, 'cell-say', 'cell-say')
    p.chmod(0o711)


def stage_ca_package():
    # A guest need not grant Ubuntu mirror access. Download through the trusted
    # host's signed apt metadata, then install only inside the guest namespaces.
    version = run('dpkg-query', '-W', '-f=${Version}', 'ca-certificates').stdout.strip()
    run('dpkg', '--compare-versions', version, 'ge', '20260816ubuntu0.24.04.1')
    with tempfile.TemporaryDirectory(prefix='.ca-package-', dir=LIB) as temp:
        run('apt-get', 'download', 'ca-certificates=' + version, cwd=temp)
        package, = Path(temp).glob('ca-certificates_*.deb')
        package.chmod(0o644)
        package.replace(LIB / 'ca-certificates.deb')


def prepare():
    run('apt-get', 'update', '-qq', timeout=300)
    run('apt-get', 'install', '-y', '-qq', 'systemd-container', 'debootstrap',
        'nftables', 'acl', 'python3', 'ca-certificates', timeout=600,
        env={**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'})
    stage_ca_package()
    if not READY.exists():
        if run('machinectl', 'show', MACHINE, check=False).returncode == 0:
            raise RuntimeError('uninitialized rootfs is already running; refusing host traversal')
        stage = ROOT.with_name('cell-env.build')
        # A partial bootstrap is operator-visible; do not silently delete it.
        if not (ETC / 'environment-bootstrap-complete').exists():
            write(ETC / 'bootstrap-wgetrc', 'timeout = 20\ntries = 3\ninet4_only = on\n')
            run('debootstrap', '--variant=minbase',
                '--include=systemd,systemd-sysv,dbus,ca-certificates,curl,python3,sudo,iproute2,nftables,git,openssh-client',
                'noble', stage, 'https://us.archive.ubuntu.com/ubuntu', timeout=900,
                env={**os.environ, 'WGETRC': str(ETC / 'bootstrap-wgetrc')})
            write(ETC / 'environment-bootstrap-complete', '1\n')
        write(stage / 'etc/apt/sources.list',
              'deb https://us.archive.ubuntu.com/ubuntu noble main universe\n'
              'deb https://us.archive.ubuntu.com/ubuntu noble-updates main universe\n'
              'deb https://security.ubuntu.com/ubuntu noble-security main universe\n')
        write(stage / 'etc/apt/apt.conf.d/80cell-proxy', f'Acquire::https::Proxy "{PROXY}";\nAcquire::http::Proxy "{PROXY}";\nAcquire::https::Timeout "30";\nAcquire::Retries "3";\nAcquire::Languages "none";\n')
        if run('chroot', stage, 'id', 'agent', check=False).returncode:
            run('chroot', stage, 'useradd', '-m', '-u', '1000', '-s', '/bin/bash', 'agent')
        # Copy the stopped legacy home, retaining the original for rollback.
        if Path('/home/agent').exists():
            run('cp', '-a', '/home/agent/.', stage / 'home/agent', timeout=600)
            run('chown', '-hR', '1000:1000', stage / 'home/agent', timeout=600)
        write(stage / 'etc/sudoers.d/agent',
              'Defaults:agent env_keep += "HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy"\n'
              'agent ALL=(ALL) NOPASSWD: ALL\n', 0o440)
        write(stage / 'etc/systemd/system/cell-network.service', f'''[Unit]
Description=Cell environment network
Before=network.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/ip link set host0 up
ExecStart=/usr/sbin/ip address replace {SPEC['guest_ip']}/30 dev host0
ExecStart=/usr/sbin/ip route replace default via {HOST}
[Install]
WantedBy=multi-user.target
''')
        wants = stage / 'etc/systemd/system/multi-user.target.wants'
        wants.mkdir(parents=True, exist_ok=True)
        link = wants / 'cell-network.service'
        link.unlink(missing_ok=True)
        link.symlink_to('../cell-network.service')
        for unit in ('systemd-networkd.service', 'systemd-networkd.socket', 'systemd-resolved.service'):
            p = stage / 'etc/systemd/system' / unit
            p.unlink(missing_ok=True)
            p.symlink_to('/dev/null')
        (stage / 'etc/resolv.conf').unlink(missing_ok=True)
        write(stage / 'etc/resolv.conf', 'nameserver 1.1.1.1\noptions timeout:1 attempts:1\n')
        # Destination mount points exist before nspawn enters the untrusted tree.
        for p in ('run/cell-chat', 'run/cell-runtime', 'var/lib/cell/inbox', 'var/lib/cell/outbox', 'var/lib/cell/said', 'etc/cell'):
            (stage / p).mkdir(parents=True, exist_ok=True)
        write(stage / 'etc/cell/cell.json', '{}\n')
        if ROOT.exists():
            raise RuntimeError('rootfs exists without initialization marker; inspect before retry')
        stage.rename(ROOT)
        write(READY, '1\n')
    shutil.copy2('/usr/local/bin/cell', LIB / 'cell')
    write(LIB / 'host.json', json.dumps(RUNTIME))
    outbox = Path('/var/lib/cell/outbox')
    # The shipper owns the directory; guest agent's mapped group can enqueue.
    os.chown(outbox, __import__('pwd').getpwnam('cell-say').pw_uid, BASE + 1000)
    outbox.chmod(0o2770)
    write('/etc/systemd/network/00-cell-environment.network', f'[Match]\nName={IFACE}\n[Link]\nUnmanaged=yes\n')
    write('/etc/systemd/system/cell-agent.service', f'''[Unit]
Description=Cell persistent autonomous environment
Requires=cell-wall.service
After=cell-wall.service cell-proxy.service cell-say.service
[Service]
Type=notify
Delegate=yes
KillMode=mixed
TimeoutStartSec=180
TimeoutStopSec=30
ExecStartPre=/usr/bin/python3 {LIB}/environment.py guard
ExecStartPre=/usr/bin/python3 {LIB}/environment.py socket
ExecStart=/usr/bin/systemd-nspawn --quiet --keep-unit --boot --settings=no --machine={MACHINE} --directory={ROOT} --private-users={BASE}:{SPEC['uid_count']} --private-users-ownership=chown --network-veth --link-journal=no --resolv-conf=off --bind-ro={LIB}:/run/cell-runtime --bind-ro=/etc/cell/cell.json:/etc/cell/cell.json --bind-ro=/var/lib/cell/inbox --bind-ro=/var/lib/cell/said --bind=/var/lib/cell/outbox --bind=/run/cell-chat --bind-ro=/run/cell-publish
ExecStartPost=/usr/bin/python3 {LIB}/environment.py network
MemoryMax={RUNTIME.get('memory_max', 4 * 1024**3)}
TasksMax=2048
Restart=on-failure
RestartSec=3
[Install]
WantedBy=multi-user.target
''')


def configure():
    for _ in range(60):
        try:
            guest('/usr/bin/true', timeout=5)
            break
        except (RuntimeError, sp.TimeoutExpired):
            time.sleep(1)
    # Host never interprets a path returned by the guest.
    guest('/usr/bin/python3', '/run/cell-runtime/environment-guest.py', timeout=600)


def check():
    probes = []
    def case(name, fn):
        try:
            fn()
            probes.append(dict(name=name, expect='ok', got='ok', detail=''))
        except Exception as e:
            probes.append(dict(name=name, expect='ok', got='failed', detail=str(e)))
    def namespaces():
        pid = run('machinectl', 'show', MACHINE, '-p', 'Leader', '--value').stdout.strip()
        assert Path(f'/proc/{pid}/uid_map').read_text().split() == ['0', str(BASE), str(SPEC['uid_count'])]
        for ns in ('user', 'mnt', 'pid', 'net'):
            assert os.readlink(f'/proc/{pid}/ns/{ns}') != os.readlink(f'/proc/1/ns/{ns}'), ns
    case('host: private UID, mount, PID and network namespaces', namespaces)
    case('host: complete firewall matches approved rules', guard)
    def memory_limit():
        actual = int(run('systemctl', 'show', 'cell-agent', '-p', 'MemoryMax', '--value').stdout.strip())
        assert actual == RUNTIME.get('memory_max', 4 * 1024**3), 'guest memory ceiling differs from host configuration'
    case('host: guest memory ceiling matches runtime', memory_limit)
    def network_probes():
        pid = run('machinectl', 'show', MACHINE, '-p', 'Leader', '--value').stdout.strip()
        # Trusted Python and source run with only the net namespace switched.
        # Guest root cannot forge their code, stdout, expected result or resolver.
        code = '''import socket
def connect(host, port):
 s=socket.create_connection((host,port),3); s.close()
def proxy(target):
 s=socket.create_connection(("10.203.0.1",3128),3); s.settimeout(15)
 s.sendall(("CONNECT "+target+" HTTP/1.1\\r\\nHost: "+target+"\\r\\n\\r\\n").encode())
 data=s.recv(1024); s.close(); return data.split(b"\\r\\n")[0]
assert b" 403 " in proxy(DENIED_HOST + ":443")
assert b" 400 " in proxy("1.1.1.1:443")
assert DOOR_STATUS in proxy("door-nobody.int.exe.xyz:443")
if ALLOWED_HOST is not None:
 assert b" 200 " in proxy(ALLOWED_HOST + ":443"), "allowed public host is unreachable through the proxy"
for host,port in [("1.1.1.1",443),("169.254.169.254",80),("10.203.0.1",8000),("10.203.0.1",22),("2606:4700:4700::1111",443)]:
 try: connect(host,port)
 except OSError: continue
 raise AssertionError((host,port,"direct route escaped"))
'''
        allowed = sorted(run('/usr/local/bin/cell', 'allow', '/etc/cell/cell.json').stdout.splitlines())
        denied = next('denied-' + str(i) + '.invalid' for i in range(len(allowed) + 1)
                      if 'denied-' + str(i) + '.invalid' not in allowed)
        public = allowed[0] if RUNTIME['backend'] == 'ssh' and allowed else None
        status = b' 200 ' if RUNTIME['backend'] == 'exe_dev' else b' 403 '
        code = f'DENIED_HOST = {denied!r}\nALLOWED_HOST = {public!r}\nDOOR_STATUS = {status!r}\n' + code
        run('nsenter', '-t', pid, '-n', '/usr/bin/python3', '-c', code, timeout=90)
    case('host: guest network reaches proxy and cannot bypass it', network_probes)
    def chat():
        run('curl', '-fsS', '--max-time', '10', '--unix-socket', '/run/cell-chat/api.sock',
            '-H', 'Host: cell.internal', 'http://cell.internal/api/pi/threads?includeArchived=true')
    case('host: persistent chat API available', chat)
    case('guest: agent can sudo', lambda: guest('sudo', '-n', 'true', user='agent'))
    if Path('/etc/cell/publisher.json').exists():
        # A framed reply, even a refusal, means the socket is visible in the
        # guest and the bridge accepts the agent's UID; it drops anyone else.
        code = '''import json, socket, struct
config = json.load(open("/run/cell-publish/publisher.json"))
s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(config["socket"])
s.sendall(struct.pack(">I", 2) + b"{}")
reply = b""
while chunk := s.recv(65536): reply += chunk
assert len(reply) > 4, "bridge closed the connection without a reply"
assert "Err" in json.loads(reply[4:])
'''
        case('guest: agent reaches the publish bridge',
             lambda: guest('/usr/bin/python3', '-c', code, user='agent', timeout=30))
    print(json.dumps(probes))


def providers():
    # Credentials never leave their environment. This is an availability hint,
    # not an authority claim; the guest can edit its own provider configuration.
    print(guest('/usr/bin/python3', '-c',
                'import json; from pathlib import Path; p=Path("/home/agent/.pi/agent/auth.json"); '
                'print(json.dumps(list(json.loads(p.read_text())) if p.exists() else []))',
                user='agent').stdout.strip())


if __name__ == '__main__':
    {'prepare': prepare, 'configure': configure, 'guard': guard,
     'network': network, 'socket': socket_dir, 'check': check,
     'providers': providers}[sys.argv[1]]()
