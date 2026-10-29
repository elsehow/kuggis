"""Publish release artifacts using fresh inodes (including on macOS)."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

target, destination = map(Path, sys.argv[1:])
destination.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='.package-', dir=destination) as staging:
    stage = Path(staging)
    sources = {'kuggis': target / 'release/kuggis',
               'kuggis-linux-x86_64': target / 'x86_64-unknown-linux-musl/release/kuggis'}
    for name, source in sources.items():
        shutil.copy2(source, stage / name)
    checksums = {name: hashlib.sha256((stage / name).read_bytes()).hexdigest() for name in sources}
    (stage / 'checksums.json').write_text(json.dumps(checksums, indent=2) + '\n')
    for name in [*sources, 'checksums.json']:
        os.replace(stage / name, destination / name)
