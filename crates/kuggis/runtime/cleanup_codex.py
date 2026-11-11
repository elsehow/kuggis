"""Explicit migration of duplicate pre--no-daemon Codex packages.

Never prune history, configuration, archives, or central releases. Missing
process visibility is a blocker. Not a boundary against malicious same-UID code.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys

HOME = Path('/home/agent')


def plain_path(path):
    if path.resolve() != path.absolute():
        raise ValueError('symlinked path: ' + str(path))


def manifest(root, exclusive=False):
    """Reject special files and external links; compare the complete package."""
    plain_path(root)
    result, allocated = {}, 0
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=raise_error):
        for name in sorted(dirs + files):
            path = Path(directory) / name
            info = path.lstat()
            key = str(path.relative_to(root))
            if stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                if Path(target).is_absolute() or not path.resolve().is_relative_to(root):
                    raise ValueError('external package link: ' + str(path))
                result[key] = ('link', target)
            elif stat.S_ISDIR(info.st_mode):
                result[key] = ('directory',)
            elif stat.S_ISREG(info.st_mode):
                if exclusive and info.st_nlink != 1:
                    raise ValueError('hard-linked package file: ' + str(path))
                with path.open('rb') as stream:
                    digest = hashlib.file_digest(stream, 'sha256').hexdigest()
                result[key] = ('file', info.st_mode & 0o777, digest)
                allocated += info.st_blocks * 512
            else:
                raise ValueError('special package file: ' + str(path))
    return result, allocated


def raise_error(error):
    raise error


def process_blockers(paths, proc=Path('/proc')):
    blockers = []
    for entry in proc.iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            argv = entry.joinpath('cmdline').read_bytes()
            evidence = [entry.joinpath('maps').read_bytes(), argv]
            for name in ('exe', 'cwd'):
                try:
                    target = os.readlink(entry / name)
                    evidence.append(target.encode())
                    # A shared CLI started outside our launcher can still load
                    # or recreate daemon packages after this inspection.
                    if (name == 'exe' and Path(target.removesuffix(' (deleted)')).name == 'codex'
                            and b'--no-daemon' not in argv.split(b'\0')):
                        blockers.append('process ' + entry.name + ' runs Codex without --no-daemon')
                except FileNotFoundError:
                    pass
            for fd in (entry / 'fd').iterdir():
                try:
                    evidence.append(os.readlink(fd).encode())
                except FileNotFoundError:
                    pass
            if any(str(path).encode() in value for path in paths for value in evidence):
                blockers.append('process ' + entry.name + ' references a legacy release')
        except OSError:
            if entry.exists():
                blockers.append('cannot fully inspect process ' + entry.name)
    return blockers


def candidates(home):
    states = [home / '.codex']
    chats = home / '.kuggis/agents/codex'
    if chats.exists():
        plain_path(chats)
        states += sorted(p for p in chats.iterdir() if re.fullmatch(r'[a-z][a-z0-9-]{0,47}', p.name))
    releases = []
    for state in states:
        parent = state / 'packages/app-server-daemon/releases'
        if parent.exists():
            plain_path(parent)
            releases.extend(sorted(parent.iterdir()))
    return releases


def inspect(home, expected_launcher, agents):
    installed = home / '.local/bin/kuggis-codex'
    if installed.read_bytes() != expected_launcher.read_bytes():
        raise ValueError('upgrade the installed Codex launcher before cleanup')
    config = json.loads(agents.read_text()).get('agents', {}).get('codex', {})
    if (config.get('launch') != [str(installed), 'default'] or
            config.get('chat_launch') != [str(installed), '{chat}']):
        raise ValueError('Codex is not configured with the shared Kuggis launcher')
    current = (home / '.local/bin/codex').resolve(strict=True)
    central = home / '.local/lib/codex'
    plain_path(central)
    if current.parent.name != 'bin' or current.parent.parent.parent != central:
        raise ValueError('Codex executable is outside the shared installation')
    shared = current.parent.parent
    if not (shared / 'bin/codex-code-mode-host').is_file():
        raise ValueError('shared Codex installation is incomplete')
    expected, _ = manifest(shared)
    expected.pop('.release-sha256', None)  # Kuggis receipt, not an upstream file.
    rows = []
    for path in candidates(home):
        row = {'path': str(path), 'status': 'skipped', 'allocated_bytes_upper_bound': 0}
        try:
            if path.name != shared.name + '-x86_64-unknown-linux-musl' or not path.is_dir():
                raise ValueError('not a duplicate of the selected shared version; retain for review')
            actual, allocated = manifest(path, exclusive=True)
            if actual != expected:
                raise ValueError('package contents differ from the shared installation')
            row.update(status='eligible', allocated_bytes_upper_bound=allocated)
        except (OSError, ValueError) as error:
            row['reason'] = str(error)
        rows.append(row)
    return rows


def cleanup(home, expected_launcher, agents, apply=False, proc=Path('/proc')):
    # Lock creation is the only persistent change made by a preview.
    plain_path(home / '.codex')
    fd = os.open(home / '.codex/.kuggis-runtime.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'a') as lock:
        if os.geteuid() == 0:
            os.fchown(lock.fileno(), home.stat().st_uid, home.stat().st_gid)
        fcntl.flock(lock, fcntl.LOCK_EX)
        rows = inspect(home, expected_launcher, agents)
        paths = [Path(row['path']) for row in rows if row['status'] == 'eligible']
        blockers = process_blockers(paths, proc) if paths else []
        report = {'ok': not blockers, 'mode': 'apply' if apply else 'plan',
                  'releases': rows, 'blockers': blockers, 'removed_bytes_upper_bound': 0}
        if apply and not blockers:
            if not shutil.rmtree.avoids_symlink_attacks:
                raise ValueError('platform lacks symlink-safe directory removal')
            if inspect(home, expected_launcher, agents) != rows:
                raise ValueError('package inventory changed; rerun preview')
            for row in rows:
                if row['status'] == 'eligible':
                    shutil.rmtree(row['path'])
                    row['status'] = 'removed'
                    report['removed_bytes_upper_bound'] += row['allocated_bytes_upper_bound']
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--plan', action='store_true', help='preview (default)')
    mode.add_argument('--apply', action='store_true', help='remove verified unused legacy duplicates')
    args = parser.parse_args()
    try:
        if sys.platform != 'linux' or not Path('/run/systemd/container').exists():
            raise ValueError('run inside a Kuggis cell')
        report = cleanup(HOME, Path(__file__).with_name('codex-launch.py'),
                         Path('/etc/kuggis/agents.json'), args.apply)
    except (OSError, ValueError) as error:
        report = {'ok': False, 'error': str(error)}
    print(json.dumps(report, indent=2))
    return 0 if report['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
