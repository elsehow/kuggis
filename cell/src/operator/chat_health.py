"""Runs inside the guest namespaces. Never traverses guest files from the host.

No transcripts, provider credentials or raw journal output leave the guest.
Recovery only touches the chat unit; the environment and other services stay up.
"""
import http.client
import json
from pathlib import Path
import re
import shlex
import socket
import subprocess
import sys
import time

STATE = Path('/home/agent/.kuggis/chat')
UNIT = Path('/etc/systemd/system/cell-chat.service')
WORKSPACE = re.compile(r'(--workspace\s+)("(?:[^"\\]|\\.)*"|[^\s]+)')


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=8).stdout


def service():
    return dict(line.split('=', 1) for line in run(
        'systemctl', 'show', 'cell-chat.service',
        '--property=LoadState,ActiveState,SubState,Result,ExecMainStatus,NRestarts').splitlines() if '=' in line)


def responding():
    connection = http.client.HTTPConnection('cell.internal', timeout=2)
    try:
        connection.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.sock.settimeout(2)
        connection.sock.connect('/run/cell-chat/api.sock')
        connection.request('GET', '/api/info')
        response = connection.getresponse()
        return response.status == 200 and json.loads(response.read(16384)).get('runtime') == 'Pi'
    except (OSError, ValueError, http.client.HTTPException):
        return False
    finally:
        connection.close()


def workspace_repair():
    """Only repair the generated command, respecting unrelated custom overrides."""
    saved = json.loads((STATE / 'workspace.json').read_text())
    path = Path(saved)
    if not path.is_absolute() or not path.is_dir() or str(path.resolve()) != saved:
        raise ValueError('Saved chat workspace is missing or has moved. Restore it before restarting chat.')
    # An operator may customize ExecStart. Do not silently discard those changes.
    command = None
    command_file = UNIT
    source_file = UNIT
    source = run('systemctl', 'cat', 'cell-chat.service')
    for line in source.splitlines():
        if line.startswith('# /'):
            source_file = Path(line[2:])
        if line.startswith('ExecStart='):
            command = line
            command_file = source_file
    if not command or not command.startswith('ExecStart=/usr/local/bin/cell chat '):
        raise ValueError('Chat has a custom startup command. Review its workspace configuration.')
    matches = list(WORKSPACE.finditer(command))
    if len(matches) != 1:
        raise ValueError('Chat startup command has no unambiguous workspace setting.')
    match = matches[0]
    configured = shlex.split(match.group(2))[0].replace('%%', '%').replace('$$', '$')
    if configured == saved:
        return None
    if command_file != UNIT:
        raise ValueError('Chat startup is overridden by a custom service file. Review its workspace configuration.')
    # systemd specifier and environment expansion must remain literal.
    quoted = '"' + saved.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$') + '"'
    if any(ord(c) < 32 for c in saved):
        raise ValueError('Saved chat workspace contains unsupported control characters.')
    fixed = command[:match.start(2)] + quoted + command[match.end(2):]
    original = UNIT.read_text()
    if original.splitlines().count(command) != 1:
        raise ValueError('Chat startup configuration changed. Check health again before repairing.')
    return original, original.replace(command, fixed, 1)


def report(state, message, recovery=None):
    return dict(state=state, message=message, recovery=recovery)


def health():
    if responding():
        return report('ready', 'Chat is ready.')
    props = service()
    if props.get('LoadState') != 'loaded':
        return report('failed', 'Chat service is not installed. Reapply the cell setup.')
    # A healthy process may be binding its socket. Never restart it on a GET.
    if props.get('ActiveState') == 'active':
        return report('starting', 'Chat is running but its API is not responding yet.')
    try:
        if workspace_repair():
            return report('failed', 'Chat cannot start because its configured workspace differs from the saved chats. Repair restores the saved workspace and preserves conversations.', 'repair')
    except FileNotFoundError:
        pass  # A newly provisioned cell has no workspace binding yet.
    except (ValueError, TypeError) as error:
        return report('failed', str(error))
    state, sub = props.get('ActiveState'), props.get('SubState')
    if state == 'inactive' and props.get('Result') == 'success':
        return report('stopped', 'Chat is stopped.', 'start')
    if state in ('failed', 'inactive') or sub == 'auto-restart':
        return report('failed', f"Chat failed to start (result: {props.get('Result', 'unknown')}, exit: {props.get('ExecMainStatus', 'unknown')}).", 'restart')
    return report('starting', 'Waiting for the chat service to finish starting or stopping.')


def recover(action):
    before = health()
    if before['state'] == 'ready':
        return before  # Duplicate opens/recovery requests cannot interrupt runs.
    if before['recovery'] != action:
        return before  # Revalidate immediately before any mutation.
    if action == 'repair':
        repair = workspace_repair()
        if repair is None:
            return health()
        original, fixed = repair
        # Edit only the workspace argument, retaining a backup. A full ExecStart
        # override would pin old runtime arguments across future setup upgrades.
        UNIT.with_suffix('.service.before-workspace-repair').write_text(original)
        temporary = UNIT.with_suffix('.service.new')
        temporary.write_text(fixed)
        temporary.chmod(UNIT.stat().st_mode & 0o777)
        temporary.replace(UNIT)
        run('systemctl', 'daemon-reload')
    # Only inactive/failed services reach here. systemctl start is idempotent if
    # systemd's own restart happens concurrently; never stop a recovered process.
    run('systemctl', 'reset-failed', 'cell-chat.service')
    run('systemctl', 'start', 'cell-chat.service')
    for _ in range(12):
        result = health()
        if result['state'] == 'ready':
            return result
        time.sleep(.25)
    return result


if __name__ == '__main__':
    action = sys.argv[1]
    try:
        result = health() if action == 'health' else recover(action)
    except Exception:
        # Do not expose raw subprocess output, which may contain guest secrets.
        result = report('failed', 'Unable to inspect or recover the chat service. Check the cell service logs.')
    print(json.dumps(result))
