#!/usr/bin/env python3
"""Install the checksum-pinned native Claude Code binary inside the guest."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile


def replace(path, data, mode=0o600):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        output.write(data)
        temporary = Path(output.name)
    try:
        temporary.chmod(mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def install(package):
    if not Path('/run/systemd/container').exists() or os.geteuid() != 1000:
        raise RuntimeError('run this recipe as agent inside the Kuggis guest')
    recipe = Path(__file__).resolve().parent
    release = json.loads((recipe / 'release.json').read_text())
    version = release['version']
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', version):
        raise ValueError('expected an exact release version')
    expected = release['archives'][platform.machine()]['sha256']
    home = Path.home()
    parent = home / '.local/lib/claude'
    parent.mkdir(parents=True, exist_ok=True)
    target = parent / version
    with Path(package).open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != expected:
            raise ValueError('Claude Code release checksum mismatch')
        source.seek(0)
        if not target.exists():
            with tempfile.TemporaryDirectory(prefix='.install-', dir=parent) as staging:
                unpacked = Path(staging) / 'package'
                unpacked.mkdir()
                with (unpacked / 'claude').open('wb') as binary:
                    shutil.copyfileobj(source, binary)
                (unpacked / 'claude').chmod(0o755)
                observed = subprocess.check_output([str(unpacked / 'claude'), '--version'], timeout=60,
                                                   env={**os.environ, 'DISABLE_UPDATES': '1'}).decode().strip()
                if observed != version + ' (Claude Code)':
                    raise ValueError('Claude Code executable version differs from release lock')
                (unpacked / '.release-sha256').write_text(expected + '\n')
                unpacked.rename(target)
        elif (target / '.release-sha256').read_text().strip() != expected:
            raise RuntimeError('existing Claude Code installation differs from release lock')
    bin_dir = home / '.local/bin'
    bin_dir.mkdir(parents=True, exist_ok=True)
    # A launcher Kuggis owns: the native updater leaves it in place, so the
    # pinned release stays selected until the recipe changes.
    with tempfile.TemporaryDirectory(prefix='.claude-link-', dir=bin_dir) as staging:
        link = Path(staging) / 'claude'
        link.symlink_to(target / 'claude')
        os.replace(link, bin_dir / 'claude')
    state = home / '.claude'
    state.mkdir(exist_ok=True, mode=0o700)
    path = state / 'settings.json'
    settings = json.loads(path.read_text()) if path.exists() else {}
    recipe_settings = json.loads((recipe / 'settings.json').read_text())
    settings['env'] = {**settings.get('env', {}), **recipe_settings.pop('env', {})}
    settings.update(recipe_settings)
    replace(path, (json.dumps(settings, indent=2) + '\n').encode())
    # Kuggis writes cell guidance to ~/AGENTS.md; Claude Code reads CLAUDE.md.
    memory = state / 'CLAUDE.md'
    if not memory.exists():
        replace(memory, b'@~/AGENTS.md\n')
    replace(bin_dir / 'kuggis-claude', (recipe / 'launch.py').read_bytes(), 0o755)
    print(json.dumps({'ok': True, 'version': version, 'executable': str(bin_dir / 'claude')}))


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: python3 install.py CLAUDE_BINARY')
    install(sys.argv[1])
