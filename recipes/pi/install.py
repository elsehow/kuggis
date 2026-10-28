#!/usr/bin/env python3
"""Install the pinned Pi archive as the guest agent; no host or model credentials."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile


def install(archive):
    if not Path('/run/systemd/container').exists() or os.geteuid() != 1000:
        raise RuntimeError('run this recipe as agent inside the Kuggis guest')
    recipe = Path(__file__).resolve().parent
    release = json.loads((recipe / 'release.json').read_text())
    version = release['version']
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', version):
        raise ValueError('expected an exact release version')
    expected = release['archives'][platform.machine()]['sha256']
    with Path(archive).open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != expected:
            raise ValueError('Pi release checksum mismatch')
        source.seek(0)
        parent = Path.home() / '.local/lib/pi'
        parent.mkdir(parents=True, exist_ok=True)
        target = parent / version
        if not target.exists():
            with tempfile.TemporaryDirectory(prefix='.install-', dir=parent) as staging:
                with tarfile.open(fileobj=source, mode='r:gz') as bundle:
                    if any(member.name.split('/')[0] != 'pi' for member in bundle.getmembers()):
                        raise ValueError('unexpected archive layout')
                    bundle.extractall(staging, filter='data')
                unpacked = Path(staging) / 'pi'
                observed = subprocess.check_output([str(unpacked / 'pi'), '--version'], timeout=30).decode().strip()
                if observed != version:
                    raise ValueError('Pi executable version differs from release lock')
                (unpacked / '.release-sha256').write_text(expected + '\n')
                unpacked.rename(target)
        elif (target / '.release-sha256').read_text().strip() != expected:
            raise RuntimeError('existing Pi installation differs from release lock')
    bin_dir = Path.home() / '.local/bin'
    bin_dir.mkdir(parents=True, exist_ok=True)
    # Ubuntu calls the fd executable fdfind; Pi looks for fd.
    if not Path('/usr/bin/fdfind').is_file() or not Path('/usr/bin/rg').is_file():
        raise RuntimeError('declare packages fd-find and ripgrep before installing Pi')
    fd = bin_dir / 'fd'
    if not fd.exists():
        fd.symlink_to('/usr/bin/fdfind')
    with tempfile.TemporaryDirectory(prefix='.pi-link-', dir=bin_dir) as staging:
        link = Path(staging) / 'pi'
        link.symlink_to(target / 'pi')
        os.replace(link, bin_dir / 'pi')
    agent_dir = Path.home() / '.pi/agent'
    agent_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    settings_path = agent_dir / 'settings.json'
    settings = json.loads(settings_path.read_text()) if settings_path.exists() else {}
    settings.update(json.loads((recipe / 'settings.json').read_text()))
    with tempfile.NamedTemporaryFile(mode='w', dir=agent_dir, delete=False) as output:
        json.dump(settings, output, indent=2)
        output.write('\n')
        temporary = Path(output.name)
    try:
        os.replace(temporary, settings_path)
    finally:
        temporary.unlink(missing_ok=True)
    print(json.dumps({'ok': True, 'version': version, 'executable': str(bin_dir / 'pi')}))


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: python3 install.py PI_ARCHIVE')
    install(sys.argv[1])
