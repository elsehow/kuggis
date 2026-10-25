#!/usr/bin/env python3
"""Offline guest snapshots without traversing a mutable guest as host root.

Run on Ubuntu as root. This intentionally does not move signing keys, change
bindings, or start/stop services. The operator must quiesce the cell first.
Archives and manifests contain private data; keep the whole work directory
private. Restore always creates a new, unbooted staging directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess as sp
import sys

BASE = 524288
COUNT = 65536
ROOT = Path('/var/lib/machines/cell-env')
SKIP = ('dev', 'proc', 'sys', 'run', 'var/lib/cell/inbox',
        'var/lib/cell/outbox', 'var/lib/cell/said')


def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def manifest(root):
    """Called only INSIDE the helper namespace (or a local test fixture)."""
    rows, links = [], {}

    def visit(path, relative):
        s = path.lstat()
        mode = stat.S_IFMT(s.st_mode)
        if mode not in (stat.S_IFDIR, stat.S_IFREG, stat.S_IFLNK, stat.S_IFIFO):
            raise RuntimeError('unsupported persistent file type')
        if s.st_uid >= COUNT or s.st_gid >= COUNT:
            raise RuntimeError('unmapped guest file owner')
        row = [relative, s.st_mode, s.st_uid, s.st_gid, s.st_mtime_ns]
        attrs = {k: os.getxattr(path, k, follow_symlinks=False).hex()
                 for k in sorted(os.listxattr(path, follow_symlinks=False))}
        row.append(attrs)
        if mode == stat.S_IFREG:
            row.extend([s.st_size, digest(path)])
            if s.st_nlink > 1:
                row.append(links.setdefault((s.st_dev, s.st_ino), relative))
        elif mode == stat.S_IFLNK:
            row.append(os.readlink(path))
        rows.append(row)
        if mode == stat.S_IFDIR and relative not in SKIP:
            for child in sorted(path.iterdir()):
                visit(child, child.name if relative == '.' else relative + '/' + child.name)
    visit(Path(root), '.')
    return rows


def run(args, **kw):
    return sp.run([str(a) for a in args], check=True, **kw)


def helper(work):
    root = work / 'helper'
    if root.exists():
        raise RuntimeError('helper already exists; use a new work directory')
    root.mkdir()
    for name in ('etc', 'usr/bin', 'dev', 'proc', 'sys', 'run', 'tmp', 'payload', 'archive'):
        (root / name).mkdir(parents=True, exist_ok=True)
    (root / 'etc/os-release').write_text('ID=ubuntu\nNAME="Kuggis migration helper"\n')
    (root / 'etc/passwd').write_text('root:x:0:0:root:/root:/bin/sh\n')
    (root / 'etc/group').write_text('root:x:0:\n')
    (root / 'archive/input.tar.gz').touch()
    shutil.copy2('/etc/localtime', root / 'etc/localtime', follow_symlinks=True)
    copied = set()

    def copy_binary(path):
        path = Path(path)
        if str(path) in copied:
            return
        copied.add(str(path))
        dest = root / str(path).lstrip('/')
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, dest, follow_symlinks=True)
        result = sp.run(['ldd', str(path)], text=True, capture_output=True, check=False)
        if result.returncode:
            raise RuntimeError('could not inspect trusted helper dependencies')
        for lib in re.findall(r'(?:=>\s+)?(/[^\s()]+)', result.stdout):
            target = root / lib.lstrip('/')
            target.parent.mkdir(parents=True, exist_ok=True)
            if not target.exists():
                shutil.copy2(lib, target, follow_symlinks=True)

    for executable in ('/usr/bin/tar', '/bin/sh', '/bin/gzip', '/usr/bin/python3'):
        copy_binary(executable)
    stdlib = Path('/usr/lib/python' + str(sys.version_info.major) + '.' + str(sys.version_info.minor))
    shutil.copytree(stdlib, root / str(stdlib).lstrip('/'), symlinks=False)
    for library in (stdlib / 'lib-dynload').glob('*.so'):
        copy_binary(library)
    shutil.copy2(__file__, root / 'snapshot.py')
    # Only this newly built, trusted helper is traversed from host root. nspawn
    # cannot combine automatic ownership shifting with a read-only root.
    for directory, dirs, files in os.walk(root):
        for path in [Path(directory), *(Path(directory) / n for n in files)]:
            s = path.lstat()
            os.chown(path, BASE + s.st_uid, BASE + s.st_gid, follow_symlinks=False)
    return root


def namespace(root, payload, command, *, writable=False, archive=None):
    args = ['systemd-nspawn', '--quiet', '--settings=no', '--register=no',
            '--console=pipe', '--directory=' + str(root), '--read-only',
            '--private-network', '--private-users=' + str(BASE) + ':' + str(COUNT),
            '--private-users-ownership=off', '--link-journal=no', '--resolv-conf=off',
            ('--bind=' if writable else '--bind-ro=') + str(payload) + ':/payload']
    if archive:
        args.append('--bind-ro=' + str(archive) + ':/archive/input.tar.gz')
    return args + command


def stopped():
    if sp.run(['systemctl', 'is-active', '--quiet', 'cell-agent']).returncode == 0:
        raise RuntimeError('stop the cell before taking a snapshot')
    if sp.run(['machinectl', 'show', 'cell-env'], stdout=sp.DEVNULL, stderr=sp.DEVNULL).returncode == 0:
        raise RuntimeError('guest is still registered')
    if sp.run(['findmnt', '--mountpoint', str(ROOT)], stdout=sp.DEVNULL).returncode == 0:
        raise RuntimeError('unexpected mounted guest root')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('operation', choices=['capture', 'restore'])
    p.add_argument('--work', required=True, type=Path)
    p.add_argument('--machine-id', required=True)
    p.add_argument('--key', required=True)
    p.add_argument('--archive', type=Path)
    p.add_argument('--manifest', type=Path)
    p.add_argument('--archive-sha256')
    args = p.parse_args()
    if os.geteuid() != 0:
        raise RuntimeError('host root required')
    if Path('/etc/machine-id').read_text().strip() != args.machine_id:
        raise RuntimeError('wrong host machine identity')
    if Path('/etc/cell/key.pub').read_text().strip() != args.key:
        raise RuntimeError('wrong current cell signing identity')
    spec = json.loads(Path('/etc/cell/environment.json').read_text())
    if (spec['uid_base'], spec['uid_count']) != (BASE, COUNT):
        raise RuntimeError('unsupported guest UID mapping')
    # No existing path components controlled by the guest; no recursive host
    # inspection or ownership changes to payloads, including after extraction.
    work = args.work
    if work.parent != Path('/var/lib/kuggis-migration') or not re.fullmatch(r'[a-z0-9-]+', work.name):
        raise RuntimeError('work must be a new /var/lib/kuggis-migration/NAME')
    work.parent.mkdir(mode=0o700, exist_ok=True)
    info = work.parent.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError('migration parent must be private and root owned')
    work.mkdir(mode=0o700)
    root = helper(work)
    if args.operation == 'capture':
        stopped()
        before = run(namespace(root, ROOT, ['/usr/bin/python3', '/snapshot.py', 'manifest']), capture_output=True).stdout
        (work / 'manifest.json').write_bytes(before)
        excludes = ['--exclude=./' + name + '/*' for name in SKIP]
        archive = work / 'guest.tar.gz'
        with archive.open('xb') as stream:
            run(namespace(root, ROOT, ['/usr/bin/tar', '--create', '--use-compress-program=/bin/gzip -1', '--format=pax',
                '--numeric-owner', '--acls', '--xattrs', '--xattrs-include=*', '--sparse',
                '--one-file-system', '--directory=/payload', *excludes, '.']), stdout=stream)
        after = run(namespace(root, ROOT, ['/usr/bin/python3', '/snapshot.py', 'manifest']), capture_output=True).stdout
        if before != after:
            raise RuntimeError('guest changed during offline snapshot')
        receipt = {'archive_sha256': digest(archive), 'manifest_sha256': hashlib.sha256(before).hexdigest(),
                   'machine_id': args.machine_id, 'key': args.key,
                   'cell_sha256': digest('/etc/cell/cell.json'),
                   'bytes': archive.stat().st_size, 'files': len(json.loads(before))}
    else:
        if not args.archive or not args.manifest or not args.archive_sha256:
            raise RuntimeError('restore requires --archive, --manifest and --archive-sha256')
        original, expected = args.archive.resolve(strict=True), args.manifest.read_bytes()
        if digest(original) != args.archive_sha256:
            raise RuntimeError('archive digest differs from the captured receipt')
        # A root-owned 0600 input is not readable by mapped guest root. Give the
        # helper a private, read-only copy, never loosen the original's mode.
        archive = work / 'input.tar.gz'
        shutil.copyfile(original, archive)
        archive.chmod(0o444)
        payload = work / 'guest'
        payload.mkdir()
        os.chown(payload, BASE, BASE)
        run(namespace(root, payload, ['/usr/bin/tar', '--extract', '--gzip', '--file=/archive/input.tar.gz',
            '--numeric-owner', '--same-owner', '--same-permissions', '--acls', '--xattrs',
            '--xattrs-include=*', '--delay-directory-restore', '--directory=/payload'],
            writable=True, archive=archive))
        actual = run(namespace(root, payload, ['/usr/bin/python3', '/snapshot.py', 'manifest']), capture_output=True).stdout
        (work / 'manifest.json').write_bytes(actual)
        if actual != expected:
            raise RuntimeError('restored filesystem differs; staging remains offline')
        receipt = {'archive_sha256': digest(archive), 'manifest_sha256': hashlib.sha256(actual).hexdigest(),
                   'files': len(json.loads(actual)), 'verified': True}
    (work / 'receipt.json').write_text(json.dumps(receipt, sort_keys=True) + '\n')
    # Only aggregate receipts go to stdout. Never log paths or file contents.
    print(json.dumps(receipt, sort_keys=True))


if __name__ == '__main__':
    if sys.argv[1:] == ['manifest']:
        if Path('/proc/self/uid_map').read_text().split() != ['0', str(BASE), str(COUNT)]:
            raise RuntimeError('manifest requires the isolated helper namespace')
        print(json.dumps(manifest('/payload'), sort_keys=True, separators=(',', ':')))
    else:
        main()
