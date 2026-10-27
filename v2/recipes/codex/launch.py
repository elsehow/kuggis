#!/usr/bin/env python3
"""Native Codex state per chat. Kuggis never interprets Codex conversations."""
import json
import os
from pathlib import Path
import re
import sys


def chat_home(home, name):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,47}', name) or name.endswith('-'):
        raise ValueError('invalid chat name')
    return home / '.kuggis/agents/codex' / name


def prepare(home, name, work=None):
    work = work or Path.cwd().resolve()
    state = chat_home(home, name)
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    config = state / 'config.toml'
    if not config.exists():
        # Exclusive creation preserves the user's subsequent /model choices.
        with config.open('x') as output:
            output.write((home / '.local/share/kuggis/codex/config.toml').read_text())
            # Native onboarding reads persisted trust, not CLI overrides.
            output.write('\n[projects.'+json.dumps(str(work))+']\ntrust_level = "trusted"\n')
        config.chmod(0o600)
    return state


def command(home, state):
    argv = [str(home / '.local/bin/codex')]
    if any((state / 'sessions').rglob('rollout-*.jsonl')):
        argv += ['resume', '--last', '--all', '--include-non-interactive']
    return argv


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: kuggis-codex CHAT')
    home = Path.home()
    state = prepare(home, sys.argv[1])
    argv = command(home, state)
    os.execvpe(argv[0], argv, {**os.environ, 'CODEX_HOME':str(state)})
