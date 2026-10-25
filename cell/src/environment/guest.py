"""Invoked only inside the guest, including on reapply. Never on the host."""
import json
import os
from pathlib import Path
import shutil
import subprocess as sp
import tempfile

if not Path('/run/systemd/container').exists():
    raise RuntimeError('guest setup requires a container')

PROXY = 'http://10.203.0.1:3128'
ENV = {**os.environ, 'DEBIAN_FRONTEND': 'noninteractive', 'HOME': '/home/agent', 'HTTPS_PROXY': PROXY,
       'HTTP_PROXY': PROXY, 'https_proxy': PROXY, 'http_proxy': PROXY,
       'NO_PROXY': 'localhost,127.0.0.1,::1', 'no_proxy': 'localhost,127.0.0.1,::1',
       'GH_HOST': 'github.int.exe.xyz'}

def run(*args, **kw):
    return sp.run(args, check=True, text=True, capture_output=True, env=ENV, **kw).stdout.strip()

def write(path, value, mode=0o644):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    # All paths here are guest paths, even if the agent replaced them with links.
    path.write_text(value)
    path.chmod(mode)

Path('/usr/local/bin').mkdir(parents=True, exist_ok=True)
shutil.copy2('/run/cell-runtime/cell', '/usr/local/bin/cell.new')
os.replace('/usr/local/bin/cell.new', '/usr/local/bin/cell')
pi = Path('/usr/local/lib/cell/pi-1.3.0-node-24.22.1')
if not (pi / 'pi').exists():
    pi.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.pi-seed-', dir=pi.parent) as temp:
        stage = Path(temp) / pi.name
        shutil.copytree('/run/cell-runtime/' + pi.name, stage, symlinks=True)
        stage.rename(pi)
# Older cell binaries and user scripts may still name the standalone launcher.
legacy = Path('/usr/local/lib/cell/pi-1.3.0/pi')
if legacy.exists() and legacy.resolve() != pi / 'pi':
    backup = Path('/var/lib/cell/migrations/emudhra-20261022')
    backup.mkdir(parents=True, exist_ok=True, mode=0o700)
    saved = backup / 'pi-standalone'
    if not saved.exists():
        shutil.copy2(legacy, saved)
        saved.chmod(0o600)
    legacy.unlink()
    legacy.symlink_to(pi / 'pi')
sudoers = Path('/etc/sudoers.d/agent')
if sudoers.exists() and sudoers.read_text() == 'agent ALL=(ALL) NOPASSWD: ALL\n':
    write(sudoers, 'Defaults:agent env_keep += "HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy"\n'
          'agent ALL=(ALL) NOPASSWD: ALL\n', 0o440)
sources = Path('/etc/apt/sources.list')
original_sources = ('deb https://archive.ubuntu.com/ubuntu noble main universe\n'
                    'deb https://archive.ubuntu.com/ubuntu noble-updates main universe\n'
                    'deb https://security.ubuntu.com/ubuntu noble-security main universe\n')
if sources.exists() and sources.read_text() == original_sources:
    write(sources, original_sources.replace('https://archive.ubuntu.com', 'https://us.archive.ubuntu.com'))
proxy_config = Path('/etc/apt/apt.conf.d/80cell-proxy')
if proxy_config.exists() and proxy_config.read_text() == f'Acquire::https::Proxy "{PROXY}";\nAcquire::http::Proxy "{PROXY}";\n':
    write(proxy_config, proxy_config.read_text() + 'Acquire::https::Timeout "30";\nAcquire::Retries "3";\nAcquire::Languages "none";\n')
# Debootstrap starts from the release pocket. Refresh guest trust independently
# of the host, without requiring additional network grants for Ubuntu mirrors.
package = '/run/cell-runtime/ca-certificates.deb'
candidate = run('dpkg-deb', '-f', package, 'Version')
installed = run('dpkg-query', '-W', '-f=${Version}', 'ca-certificates')
if sp.run(['dpkg', '--compare-versions', installed, 'lt', candidate], env=ENV).returncode == 0:
    run('dpkg', '--install', package, timeout=180)
run('update-ca-certificates', '--fresh', timeout=60)
version = run('dpkg-query', '-W', '-f=${Version}', 'ca-certificates')
run('dpkg', '--compare-versions', version, 'ge', '20260816ubuntu0.24.04.1')
write('/etc/profile.d/cell.sh', '''export HTTPS_PROXY=http://10.203.0.1:3128 HTTP_PROXY=http://10.203.0.1:3128
export https_proxy=$HTTPS_PROXY http_proxy=$HTTP_PROXY
export NO_PROXY=localhost,127.0.0.1,::1 no_proxy=localhost,127.0.0.1,::1
export GH_HOST=github.int.exe.xyz PI_CODING_AGENT_DIR=/home/agent/.pi/agent
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
''')
workspace = run('runuser', '-u', 'agent', '--', '/usr/local/bin/cell', 'prepare', '/etc/cell/cell.json')
if '\n' in workspace or not workspace.startswith('/home/agent/'):
    raise RuntimeError('invalid workspace')
# systemd quoting, including literal expansion characters in repository names.
workspace = workspace.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$')
write('/etc/systemd/system/cell-chat.service', f'''[Unit]
Description=Cell persistent Pi sessions
After=cell-network.service
StartLimitIntervalSec=60
StartLimitBurst=5
[Service]
User=agent
Group=agent
WorkingDirectory=/home/agent
Environment=HOME=/home/agent PI_CODING_AGENT_DIR=/home/agent/.pi/agent
Environment=PATH=/home/agent/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
Environment=HTTPS_PROXY={PROXY} HTTP_PROXY={PROXY} https_proxy={PROXY} http_proxy={PROXY}
Environment=NO_PROXY=localhost,127.0.0.1,::1 no_proxy=localhost,127.0.0.1,::1
Environment=GH_HOST=github.int.exe.xyz
ExecStart=/usr/local/bin/cell chat --cell /etc/cell/cell.json --workspace "{workspace}" --state /home/agent/.kuggis/chat --socket /run/cell-chat/api.sock --agent-dir /home/agent/.pi/agent --pi /usr/local/lib/cell/pi-1.3.0-node-24.22.1/pi --inbox /var/lib/cell/inbox --outbox /var/lib/cell/outbox --shared /home/agent/shared
Restart=always
RestartSec=2
UMask=0077
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
''')
run('systemctl', 'daemon-reload')
run('systemctl', 'enable', 'cell-chat')
run('systemctl', 'restart', 'cell-chat')
