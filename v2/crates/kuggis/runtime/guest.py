"""Run only inside the guest namespaces. Never use this to traverse a host rootfs."""
import base64
import fcntl
import hashlib
import json
import os
import re
import shutil
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
AGENTS = Path('/etc/kuggis/agents.json')


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


def chat_name(value):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,47}', value) or value.endswith('-'):
        raise ValueError('invalid chat name')
    return value


def launch_argv(default, named, name):
    """Recipes own agent-specific history selection; never copy the default chat."""
    chat_name(name)
    if name == 'default':
        return default
    if (not named or not isinstance(named, list) or not all(isinstance(arg, str) for arg in named)
            or not named[0] or named[0].startswith('-') or '{chat}' in named[0]
            or any('\0' in arg for arg in named) or not any('{chat}' in arg for arg in named[1:])):
        raise ValueError('named chats require a chat_launch recipe; configure it and run setup first')
    return [arg.replace('{chat}', name) for arg in named]


def launch(name):
    config = json.loads(AGENTS.read_text()) if AGENTS.exists() else {}
    if config.get('agents'):
        agent = select_chat(name)
        recipe = config['agents'][agent]
        argv = launch_argv(recipe['launch'], recipe['chat_launch'], name)
    else:
        argv = launch_argv(json.loads(Path('/etc/kuggis/launch.json').read_text()),
                           json.loads(Path('/etc/kuggis/chat-launch.json').read_text()), name)
    os.chdir(WORK)
    os.execvpe(argv[0], argv, {**os.environ, 'KUGGIS_CHAT': name})


def revive_chat(name):
    """Only revive an exited pane; concurrent attaches never start two agents."""
    chat_name(name)
    locks = HOME / '.kuggis/chat-locks'
    locks.mkdir(parents=True, exist_ok=True)
    with (locks / (name + '.lock')).open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        tmux = ['/usr/bin/tmux', '-L', 'kuggis' if name == 'default' else 'kuggis-chat-' + name]
        pane = subprocess.run([*tmux, 'display-message', '-p', '-t', '=workspace:0.0', '#{pane_dead}'],
                              check=True, capture_output=True).stdout.strip()
        if pane == b'1':
            subprocess.run([*tmux, 'respawn-pane', '-t', '=workspace:0.0', '-c', str(WORK),
                            '/usr/local/bin/kuggis-launch', name], check=True)


def choose_agent(config, saved, requested):
    """A saved binding wins; an explicit mismatch is a migration, never a switch."""
    if saved is not None and (set(saved) != {'version', 'agent'} or saved['version'] != 1):
        raise ValueError('invalid saved chat binding')
    selected = saved['agent'] if saved else requested or config.get('default_agent')
    if not isinstance(selected, str) or selected not in config.get('agents', {}):
        raise ValueError('chat agent is not configured; install its recipe before connecting')
    chat_name(selected)
    if requested and requested != selected:
        raise ValueError(f'chat belongs to {selected}, not {requested}; use a new chat name or explicitly migrate it')
    return selected


def list_chats():
    """Read-only discovery. Never bind a chat, start a service, or launch an agent."""
    config = json.loads(AGENTS.read_text()) if AGENTS.exists() else {}
    bindings = HOME / '.kuggis/chat-bindings'
    names = {p.stem for p in bindings.glob('*.json')}
    if not config.get('agents'):
        names.add('default')
        names.update(p.name for p in (HOME / '.kuggis/chats').glob('*') if p.is_dir())
    chats = []
    for name in sorted(names):
        chat_name(name)
        path = bindings / (name + '.json')
        agent = choose_agent(config, json.loads(path.read_text()), None) if path.exists() else None
        socket = 'kuggis' if name == 'default' else 'kuggis-chat-' + name
        pane = subprocess.run(['/usr/bin/tmux','-L',socket,'list-panes','-t','=workspace','-F','#{pane_dead}'], capture_output=True)
        chats.append({'name':name, 'agent':agent, 'running':pane.returncode == 0 and b'0' in pane.stdout.splitlines()})
    return {'chats':chats, 'agents':sorted(config.get('agents',{})), 'default_agent':config.get('default_agent')}


def select_chat(name, requested=None):
    chat_name(name)
    if requested:
        chat_name(requested)
    config = json.loads(AGENTS.read_text())
    bindings, locks = HOME / '.kuggis/chat-bindings', HOME / '.kuggis/chat-locks'
    bindings.mkdir(parents=True, exist_ok=True)
    locks.mkdir(parents=True, exist_ok=True)
    path = bindings / (name + '.json')
    with (locks / (name + '.lock')).open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        saved = json.loads(path.read_text()) if path.exists() else None
        agent = choose_agent(config, saved, requested)
        recipe = config['agents'][agent]
        argv = launch_argv(recipe['launch'], recipe['chat_launch'], name)
        if not shutil.which(argv[0]):
            raise ValueError(f'agent {agent} executable is not installed')
        if not saved:
            install_file(path, (json.dumps({'version':1, 'agent':agent}) + '\n').encode(), 0o600)
        return agent


def legacy_agent(agents, default, named):
    pair = {'launch':default, 'chat_launch':named}
    matches = [name for name, recipe in agents.items() if recipe == pair]
    if len(matches) != 1:
        raise ValueError('adopting legacy chats requires one recipe matching the existing launch and chat_launch')
    return matches[0]


def configure_agents(payload):
    config = {'agents':payload.get('agents', {}), 'default_agent':payload.get('default_agent')}
    old = json.loads(AGENTS.read_text()) if AGENTS.exists() else {}
    if old.get('agents') and not config['agents']:
        raise ValueError('cannot remove agent recipes from chats without explicit migration')
    old_launch, old_named = Path('/etc/kuggis/launch.json'), Path('/etc/kuggis/chat-launch.json')
    if config['agents'] and not old.get('agents') and old_launch.exists():
        previous = legacy_agent(config['agents'], json.loads(old_launch.read_text()),
                                json.loads(old_named.read_text()) if old_named.exists() else None)
        names = ['default'] + [p.name for p in (HOME / '.kuggis/chats').glob('*') if p.is_dir()]
        bindings = HOME / '.kuggis/chat-bindings'
        bindings.mkdir(parents=True, exist_ok=True)
        os.chown(bindings.parent, 1000, 1000)
        os.chown(bindings, 1000, 1000)
        for name in names:
            chat_name(name)
            path = bindings / (name + '.json')
            if not path.exists():
                install_file(path, (json.dumps({'version':1,'agent':previous})+'\n').encode(), 0o600)
                os.chown(path, 1000, 1000)
    install_file(AGENTS, (json.dumps(config, indent=2)+'\n').encode())


def terminal_unit(socket, argument=''):
    return f'''[Unit]
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
ExecStart=/usr/bin/tmux -L {socket} -f /etc/kuggis/tmux.conf new-session -d -s workspace -c /home/agent/workspace /usr/local/bin/kuggis-launch{argument}
ExecStop=/usr/bin/tmux -L {socket} kill-server
Restart=no
UMask=0077
'''


def configure_terminals(payload):
    """Also safe to apply in a running guest: writes files without restarting chats."""
    if os.geteuid() != 0:
        raise RuntimeError('terminal configuration requires guest root')
    configure_agents(payload)
    install_file('/etc/kuggis/launch.json', json.dumps(payload['launch']).encode())
    install_file('/etc/kuggis/chat-launch.json', json.dumps(payload.get('chat_launch')).encode())
    install_file('/usr/local/bin/kuggis-launch', b'''#!/usr/bin/python3
import os, sys
os.execv('/usr/bin/python3', ['/usr/bin/python3', '/run/kuggis-runtime/guest.py', 'launch', *(sys.argv[1:] or ['default'])])
''', 0o755)
    install_file('/etc/kuggis/tmux.conf', b'set -g remain-on-exit on\nset -g exit-empty off\nset -s extended-keys on\n')
    install_file('/etc/systemd/system/kuggis-terminal.service', terminal_unit('kuggis').encode())
    install_file('/etc/systemd/system/kuggis-chat@.service', terminal_unit('kuggis-chat-%i', ' %i').encode())


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
    configure_terminals(payload)
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
    elif len(args) == 2 and args[0] == 'launch':
        launch(args[1])
    elif len(args) == 2 and args[0] == 'revive-chat':
        revive_chat(args[1])
    elif args == ['list-chats']:
        print(json.dumps(list_chats()))
    elif len(args) in (2, 3) and args[0] == 'select-chat':
        print(json.dumps({'agent':select_chat(args[1], args[2] if len(args) == 3 else None)}))
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
