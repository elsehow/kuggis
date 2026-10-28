#!/usr/bin/env python3
"""Install a complete, checksum-pinned Codex package inside the guest."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
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
    parent = Path.home() / '.local/lib/codex'
    parent.mkdir(parents=True, exist_ok=True)
    target = parent / version
    with Path(archive).open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != expected:
            raise ValueError('Codex release checksum mismatch')
        source.seek(0)
        if not target.exists():
            with tempfile.TemporaryDirectory(prefix='.install-', dir=parent) as staging:
                unpacked = Path(staging) / 'package'
                unpacked.mkdir()
                with tarfile.open(fileobj=source, mode='r:gz') as bundle:
                    bundle.extractall(unpacked, filter='data')
                observed = subprocess.check_output([str(unpacked/'bin/codex'), '--version'], timeout=30).decode().strip()
                if observed != 'codex-cli ' + version or not (unpacked/'bin/codex-code-mode-host').is_file():
                    raise ValueError('incomplete or unexpected Codex package')
                (unpacked/'.release-sha256').write_text(expected+'\n')
                unpacked.rename(target)
        elif (target/'.release-sha256').read_text().strip() != expected:
            raise RuntimeError('existing Codex installation differs from release lock')
    bin_dir = Path.home() / '.local/bin'
    bin_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.codex-link-', dir=bin_dir) as staging:
        link = Path(staging) / 'codex'
        link.symlink_to(target/'bin/codex')
        os.replace(link, bin_dir/'codex')
    shared = Path.home() / '.local/share/kuggis/codex'
    shared.mkdir(parents=True, exist_ok=True, mode=0o700)
    for name in ('config.toml', 'models.json'):
        shutil.copyfile(recipe/name, shared/name)
    shutil.copyfile(recipe/'launch.py', bin_dir/'kuggis-codex')
    (bin_dir/'kuggis-codex').chmod(0o755)
    print(json.dumps({'ok':True,'version':version,'executable':str(bin_dir/'codex')}))


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: python3 install.py CODEX_PACKAGE')
    install(sys.argv[1])
