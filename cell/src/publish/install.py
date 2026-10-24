# Invoked only by the trusted operator over SSH, as root. Never reads guest files.
import json, os, pathlib, subprocess, sys, tempfile
context = json.load(sys.stdin)
if pathlib.Path('/etc/cell/key.pub').read_text().strip() != context['key']:
    raise RuntimeError('Cell key changed; refusing publishing configuration')

def atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.publish-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            os.fchmod(stream.fileno(), 0o644)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try: os.fsync(directory_fd)
        finally: os.close(directory_fd)
    finally:
        if os.path.exists(temporary): os.unlink(temporary)

publishing = context.get('publishing')
if publishing is not None:
    path = pathlib.Path('/etc/cell/publisher.json')
    marker = pathlib.Path('/etc/cell/publisher-managed')
    config = publishing.get('config')
    if config is not None:
        if config['socket'] != '/run/cell-publish/bridge.sock':
            raise RuntimeError('Invalid publisher socket')
        data = json.dumps(config, sort_keys=True).encode()
        changed = not path.exists() or path.read_bytes() != data
        atomic(marker, b'operator-managed\n')
        if changed: atomic(path, data)
        active = subprocess.run(['systemctl', 'is-active', '--quiet', 'cell-publish-bridge.service']).returncode == 0
        try:
            if changed or not active:
                subprocess.run(['systemctl', 'restart', 'cell-publish-bridge.service'], check=True, timeout=15, stdout=subprocess.DEVNULL)
            subprocess.run(['systemctl', 'is-active', '--quiet', 'cell-publish-bridge.service'], check=True, timeout=5)
            advertised = json.loads(pathlib.Path('/run/cell-publish/publisher.json').read_text())
            if advertised != config: raise RuntimeError('Bridge configuration is stale')
        except Exception:
            publishing['config'] = None
            publishing['error'] = 'Publisher bridge unavailable; update the cell runtime and retry'
    elif marker.exists():
        subprocess.run(['systemctl', 'stop', 'cell-publish-bridge.service'], check=True, timeout=15)
        path.unlink(missing_ok=True)
        pathlib.Path('/run/cell-publish/publisher.json').unlink(missing_ok=True)
        marker.unlink()
atomic(pathlib.Path('/usr/local/lib/cell/context.json'), json.dumps(context).encode())
