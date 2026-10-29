#!/usr/bin/env python3
"""Shared native Codex authentication, explicit session bindings per chat.

Keep legacy SQLite stores in place: paginated histories cannot be migrated by
copying JSONL alone. Profiles and sqlite_home preserve those native stores while
CODEX_HOME (and authentication) is shared with ordinary shells.
"""
import fcntl
from contextlib import closing
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import sys
import tempfile
import tomllib
import uuid


def chat_home(home, name):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,47}', name) or name.endswith('-'):
        raise ValueError('invalid chat name')
    return home / '.kuggis/agents/codex' / name


def atomic(path, data, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix='.kuggis-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data if isinstance(data, bytes) else data.encode())
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def root_values(text, values):
    """Edit simple root keys, retaining comments and every unrelated TOML value."""
    original = tomllib.loads(text)
    lines = text.splitlines(keepends=True)
    boundary = next((i for i, line in enumerate(lines) if line.lstrip().startswith('[')), len(lines))
    root, rest = ''.join(lines[:boundary]), ''.join(lines[boundary:])
    for key, value in values.items():
        root = re.sub(r'(?m)^\s*' + re.escape(key) + r'\s*=.*\n?', '', root)
        if value is not None:
            root += '\n' + key + ' = ' + json.dumps(value) + '\n'
    result = root + '\n' + rest
    expected = {**original, **{k: v for k, v in values.items() if v is not None}}
    for key, value in values.items():
        if value is None:
            expected.pop(key, None)
    if tomllib.loads(result) != expected:
        raise ValueError('cannot safely update Codex configuration; original was preserved')
    return result


def shared_home(home, work=None):
    state = home / '.codex'
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    config = state / 'config.toml'
    if not config.exists():
        template = (home / '.local/share/kuggis/codex/config.toml').read_text()
        template += '\n[projects.' + json.dumps(str(work or home / 'workspace')) + ']\ntrust_level = "trusted"\n'
        atomic(config, template)
    else:
        # A shell login can create a minimal native config before Kuggis runs.
        # Authentication must not silently replace the guest recipe's execution
        # defaults with Codex's laptop sandbox defaults. Keep explicit choices.
        text = config.read_text()
        current = tomllib.loads(text)
        template = tomllib.loads((home / '.local/share/kuggis/codex/config.toml').read_text())
        missing = {key: template[key] for key in ('sandbox_mode', 'approval_policy')
                   if key not in current and key in template}
        if missing:
            updated = root_values(text, missing)
            backup = state / 'config.before-kuggis-defaults.toml'
            if not backup.exists():
                atomic(backup, text)
            atomic(config, updated)
    return state


def legacy_session(state):
    database = state / 'state_5.sqlite'
    if database.exists():
        with closing(sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)) as connection:
            row = connection.execute("SELECT id FROM threads WHERE archived=0 AND source IN "
                "('cli','vscode','exec','mcp','appServer') ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT 1").fetchone()
        if row:
            return str(uuid.UUID(row[0]))
    for rollout in sorted((state / 'sessions').rglob('rollout-*.jsonl'), key=lambda p: p.stat().st_mtime_ns, reverse=True):
        with rollout.open() as stream:
            first = json.loads(stream.readline())
        meta = first.get('payload', {})
        if first.get('type') == 'session_meta' and meta.get('source') in ('cli', 'vscode', 'exec', None):
            return str(uuid.UUID(meta['id']))
    return None


def prepare(home, name, work=None):
    common = shared_home(home, work)
    state = chat_home(home, name)
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (common / '.kuggis-migration.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        binding = state / 'kuggis-session.json'
        if not binding.exists():
            session_id = legacy_session(state)
            atomic(binding, json.dumps({'version': 1, 'session_id': session_id, 'legacy_pending': bool(session_id)}) + '\n')
        profile = common / ('kuggis-' + name + '.config.toml')
        if not profile.exists():
            legacy = state / 'config.toml'
            text = (legacy if legacy.exists() else common / 'config.toml').read_text()
            text = root_values(text, {'model_provider': None, 'cli_auth_credentials_store': None,
                                     'model_catalog_json': None})
            command = shlex.join(['/usr/bin/python3', str(home / '.local/bin/kuggis-codex'), '--bind', name])
            text += '\n[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = "command"\ncommand = ' + json.dumps(command) + '\ntimeout = 10\n'
            tomllib.loads(text)
            atomic(profile, text)
    return state


def command(home, state):
    common = shared_home(home)
    config = tomllib.loads((common / 'config.toml').read_text())
    argv = [str(home / '.local/bin/codex'), '--no-daemon', '--enable', 'hooks',
            '--dangerously-bypass-hook-trust', '-p', 'kuggis-' + state.name,
            '-c', 'sqlite_home=' + json.dumps(str(state)),
            '-c', 'log_dir=' + json.dumps(str(state / 'log')),
            '-c', 'model_provider=' + json.dumps(config.get('model_provider', 'openai')),
            '-c', 'cli_auth_credentials_store="file"']
    binding = json.loads((state / 'kuggis-session.json').read_text())
    if binding.get('legacy_pending'):
        # A legacy chat may have continued or used /new after login was set up.
        # Re-read its own store once at handover, then the hook owns the binding.
        binding['session_id'] = legacy_session(state) or binding.get('session_id')
    if binding.get('session_id'):
        argv += ['resume', str(uuid.UUID(binding['session_id']))]
    return argv


def bind(home, name, payload):
    # Native SessionStart supplies the exact id, including /new and forks.
    # Never guess from shared --last or concurrent rollout timestamps.
    if payload.get('hook_event_name') != 'SessionStart':
        raise ValueError('expected a native SessionStart hook')
    session_id = str(uuid.UUID(payload['session_id']))
    state = chat_home(home, name)
    atomic(state / 'kuggis-session.json', json.dumps({'version': 1, 'session_id': session_id}) + '\n')


def activate(home):
    common = shared_home(home)
    path = common / 'config.toml'
    text = path.read_text()
    updated = root_values(text, {'model_provider': 'openai', 'cli_auth_credentials_store': 'file',
                                 'model_catalog_json': None})
    backup = common / 'config.before-kuggis-login.toml'
    if not backup.exists():
        atomic(backup, text)
    atomic(path, updated)


def main():
    home = Path.home()
    if len(sys.argv) == 3 and sys.argv[1] == '--bind':
        bind(home, sys.argv[2], json.load(sys.stdin))
        return
    if len(sys.argv) != 2:
        raise SystemExit('usage: kuggis-codex CHAT')
    state = prepare(home, sys.argv[1])
    lock = (state / '.kuggis-launch.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    os.set_inheritable(lock.fileno(), True)
    argv = command(home, state)
    os.execvpe(argv[0], argv, {**os.environ, 'CODEX_HOME': str(home / '.codex')})


if __name__ == '__main__':
    main()
