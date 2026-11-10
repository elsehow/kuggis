#!/usr/bin/env python3
"""Shared native Claude Code login, one explicitly bound session per chat.

Authentication, settings and transcripts stay in the cell user's ~/.claude, so
chats and ordinary shells share one sign-in. Each chat records its own session
id; nothing relies on a shared `--continue`.
"""
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import uuid


def chat_home(home, name):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,47}', name) or name.endswith('-'):
        raise ValueError('invalid chat name')
    return home / '.kuggis/agents/claude' / name


def atomic(path, data, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data.encode())
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def prepare(home, name):
    state = chat_home(home, name)
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    hook = shlex.join(['/usr/bin/python3', str(home / '.local/bin/kuggis-claude'), '--bind', name])
    atomic(state / 'settings.json', json.dumps({'hooks': {'SessionStart': [
        {'hooks': [{'type': 'command', 'command': hook, 'timeout': 10}]}]}}, indent=2) + '\n')
    binding = state / 'kuggis-session.json'
    if not binding.exists():
        atomic(binding, json.dumps({'version': 1, 'session_id': str(uuid.uuid4())}) + '\n')
    return state


def saved(home, session_id):
    """A session can only be resumed once its transcript has been written."""
    return any((home / '.claude/projects').glob('*/' + session_id + '.jsonl'))


def command(home, state):
    session_id = str(uuid.UUID(json.loads((state / 'kuggis-session.json').read_text())['session_id']))
    # The cell boundary supplies authority, as it does for the other recipes.
    return [str(home / '.local/bin/claude'), '--settings', str(state / 'settings.json'),
            '--dangerously-skip-permissions',
            *(['--resume', session_id] if saved(home, session_id) else ['--session-id', session_id])]


def bind(home, name, payload):
    # Native SessionStart supplies the exact id, including /clear and forks.
    if payload.get('hook_event_name') != 'SessionStart':
        raise ValueError('expected a native SessionStart hook')
    session_id = str(uuid.UUID(payload['session_id']))
    atomic(chat_home(home, name) / 'kuggis-session.json',
           json.dumps({'version': 1, 'session_id': session_id}) + '\n')


def main():
    home = Path.home()
    if len(sys.argv) == 3 and sys.argv[1] == '--bind':
        bind(home, sys.argv[2], json.load(sys.stdin))
        return
    if len(sys.argv) != 2:
        raise SystemExit('usage: kuggis-claude CHAT')
    state = prepare(home, sys.argv[1])
    lock = (state / '.kuggis-launch.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    os.set_inheritable(lock.fileno(), True)
    argv = command(home, state)
    os.execv(argv[0], argv)


if __name__ == '__main__':
    main()
