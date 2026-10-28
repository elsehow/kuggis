#!/usr/bin/env python3
"""Copy declared guest data, never a rootfs or host control-plane state.

Run export/restore only as agent inside a container. Archives may contain private
project data: keep them outside Git. No credentials are inferred or provisioned.
"""
from dataclasses import dataclass
import fnmatch
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import stat
import sys
import tarfile

MANIFEST = '.kuggis-migration-manifest.json'


def relative(value):
    if (not isinstance(value, str) or not value or value.startswith('/')
            or '\\' in value or any(ord(c) < 32 for c in value)
            or any(p in ('', '.', '..') for p in value.split('/'))):
        raise ValueError('expected a relative path without traversal')
    return value


@dataclass(frozen=True)
class Selection:
    roots: tuple[str, ...]
    exclude: tuple[str, ...]

    @classmethod
    def parse(cls, value):
        if (not isinstance(value, dict) or set(value) != {'version', 'roots', 'exclude'}
                or type(value['version']) is not int or value['version'] != 1):
            raise ValueError('unsupported selection')
        if not isinstance(value['roots'], list) or not value['roots']:
            raise ValueError('expected nonempty roots')
        roots = tuple(relative(p) for p in value['roots'])
        if len(set(roots)) != len(roots) or any(
                a.startswith(b + '/') for a in roots for b in roots if a != b):
            raise ValueError('overlapping roots')
        if not isinstance(value['exclude'], list):
            raise ValueError('expected exclusion list')
        excluded = tuple(relative(p) for p in value['exclude'])
        return cls(roots, excluded)

    def includes(self, name):
        return not any(fnmatch.fnmatchcase(name, p) for p in self.exclude)


def entries(root, selection):
    found = []
    def visit(name):
        relative(name)
        if name == MANIFEST:
            raise ValueError('reserved manifest name')
        if not selection.includes(name):
            return
        path = root / name
        mode = path.lstat().st_mode
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISLNK(mode)):
            raise ValueError('unsupported file type: ' + name)
        found.append(name)
        if stat.S_ISDIR(mode):
            for child in sorted(path.iterdir()):
                visit(name + '/' + child.name)
    for name in selection.roots:
        # A selected root must not traverse a symlink before its final component.
        for parent in PurePosixPath(name).parents:
            if (root / parent).is_symlink():
                raise ValueError('symlink in selected root')
        visit(name)
    return sorted(found)


def fingerprint(path):
    s = path.lstat()
    return (s.st_dev, s.st_ino, s.st_mode, s.st_size, s.st_mtime_ns, s.st_ctime_ns)


def export(root, selection, output):
    names = entries(root, selection)
    before = {name: fingerprint(root / name) for name in names}
    records = []
    with tarfile.open(fileobj=output, mode='w|gz', compresslevel=3) as archive:
        for name in names:
            path = root / name
            member = archive.gettarinfo(str(path), arcname=name)
            if member.islnk():
                member.type = tarfile.REGTYPE
                member.linkname = ''
                member.size = path.lstat().st_size
            member.uid = member.gid = 1000
            member.uname = member.gname = 'agent'
            record = {'path': name, 'mode': member.mode & 0o777}
            if member.isfile():
                with path.open('rb') as source:
                    digest = hashlib.file_digest(source, 'sha256').hexdigest()
                    source.seek(0)
                    archive.addfile(member, source)
                record.update(kind='file', size=member.size, sha256=digest)
            elif member.isdir():
                record.update(kind='directory')
                archive.addfile(member)
            elif member.issym():
                record.update(kind='symlink', target=member.linkname)
                archive.addfile(member)
            else:
                raise ValueError('unsupported archive member: ' + name)
            records.append(record)
        if names != entries(root, selection) or any(
                fingerprint(root / name) != before[name] for name in names):
            raise RuntimeError('source changed during export; retry when idle')
        manifest = json.dumps({'version': 1, 'entries': records}, sort_keys=True).encode()
        member = tarfile.TarInfo(MANIFEST)
        member.size = len(manifest)
        member.mode = 0o600
        archive.addfile(member, io.BytesIO(manifest))
    return {'entries': len(records), 'manifest_sha256': hashlib.sha256(manifest).hexdigest()}


def restore(archive_path, destination):
    # A new staging directory makes failure/retry explicit and prevents merging
    # with an existing workspace. Activation is a separate deployment action.
    destination.mkdir(mode=0o700)
    seen = {}
    manifest = None
    directory_times = []
    options = {'fileobj': archive_path} if hasattr(archive_path, 'read') else {'name': archive_path}
    with tarfile.open(mode='r|gz', **options) as archive:
        for member in archive:
            name = relative(member.name)
            if name == MANIFEST:
                if manifest is not None or not member.isfile() or member.size > 32 * 1024**2:
                    raise ValueError('invalid manifest')
                manifest = archive.extractfile(member).read()
                continue
            if manifest is not None or name in seen:
                raise ValueError('duplicate member or member after manifest')
            path = destination / name
            for parent in reversed(PurePosixPath(name).parents):
                directory = destination / parent
                if directory.is_symlink():
                    raise ValueError('symlink ancestor')
                directory.mkdir(exist_ok=True)
            record = {'path': name, 'mode': member.mode & 0o777}
            if member.isdir():
                path.mkdir(exist_ok=True)
                record.update(kind='directory')
            elif member.isfile():
                digest = hashlib.sha256()
                size = 0
                with path.open('xb') as target, archive.extractfile(member) as source:
                    while data := source.read(1024 * 1024):
                        target.write(data)
                        digest.update(data)
                        size += len(data)
                record.update(kind='file', size=size, sha256=digest.hexdigest())
            elif member.issym():
                # Absolute links retain guest paths (e.g. venv Python). No later
                # member may traverse them, and this never runs on the host.
                path.symlink_to(member.linkname)
                record.update(kind='symlink', target=member.linkname)
            else:
                raise ValueError('unsupported archive member')
            if member.isfile():
                path.chmod(record['mode'])
            if member.isdir():
                directory_times.append((path, member.mtime, record['mode']))
            else:
                os.utime(path, (member.mtime, member.mtime), follow_symlinks=False)
            seen[name] = record
    wanted = json.loads(manifest) if manifest is not None else None
    if (not isinstance(wanted, dict) or set(wanted) != {'version', 'entries'}
            or type(wanted['version']) is not int or wanted['version'] != 1
            or wanted['entries'] != [seen[n] for n in sorted(seen)]):
        raise ValueError('manifest mismatch')
    for path, modified, mode in reversed(directory_times):
        path.chmod(mode)
        os.utime(path, (modified, modified), follow_symlinks=False)
    (destination / MANIFEST).write_bytes(manifest)
    return {'ok': True, 'entries': len(seen),
            'manifest_sha256': hashlib.sha256(manifest).hexdigest()}


def main(args):
    if os.geteuid() != 1000 or not Path('/run/systemd/container').is_file():
        raise RuntimeError('run only as agent inside the guest')
    if len(args) == 2 and args[0] == 'export':
        selection = Selection.parse(json.loads(args[1]))
        receipt = export(Path('/home/agent'), selection, sys.stdout.buffer)
        print(json.dumps(receipt), file=sys.stderr)
    elif len(args) == 3 and args[0] == 'restore':
        target = Path(args[2])
        if target.parent != Path('/home/agent') or not target.name.startswith('.kuggis-v2-restore-'):
            raise ValueError('restore requires a fresh guest-home staging directory')
        source = sys.stdin.buffer if args[1] == '-' else Path(args[1])
        print(json.dumps(restore(source, target)))
    else:
        raise ValueError('expected export SELECTION_JSON or restore ARCHIVE STAGING')


if __name__ == '__main__':
    main(sys.argv[1:])
