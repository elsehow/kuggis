#!/usr/bin/env bash
# Runs inside a box tmux session: set up a fresh worktree, then the agent.
# The session ends when the agent exits; `box <project> <name>` resumes the
# conversation that last ran in this worktree.
set -uo pipefail
. ~/.box/config
. ~/.box/env

name=$1 agent=$2 fresh=$3
export BOX_SESSION=$name

if [ "$fresh" = 1 ] && [ -f ~/.box/profile.sh ]; then
  . ~/.box/profile.sh
  if type box_worktree >/dev/null 2>&1; then
    box_worktree || echo "box: worktree setup failed; starting $agent anyway"
  fi
fi

# A project-local virtualenv (set up by the profile) is active for the agent.
if [ -f .venv/bin/activate ]; then . .venv/bin/activate; fi

case $agent in
  codex)
    cx=~/.codex/config.toml
    grep -qF "[projects.\"$PWD\"]" "$cx" 2>/dev/null ||
      printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$PWD" >>"$cx"
    # Codex rollouts record their cwd; `resume --last` filters by cwd.
    if grep -rqF "\"cwd\":\"$PWD\"" ~/.codex/sessions 2>/dev/null; then
      set -- codex resume --last
    else
      set -- codex
    fi ;;
  *)
    # Claude keeps transcripts under ~/.claude/projects/<cwd, non-alphanumerics as ->.
    if ls "$HOME/.claude/projects/$(pwd | sed 's/[^A-Za-z0-9]/-/g')"/*.jsonl >/dev/null 2>&1; then
      set -- claude --continue
    else
      set -- claude -n "$name"
    fi ;;
esac

# A brief from kuggis dispatch is the session's first message, given once.
first=~/.box/first/$name
if [ -f "$first" ]; then
  msg=$(cat "$first")
  rm -f "$first"
  if [ "${1:-} ${2:-}" = "codex resume" ]; then
    # `codex resume --last` reads a trailing argument as a session name, so the
    # brief is typed in once its screen is up.
    (sleep 8 && tmux send-keys -t "=$name:" -l -- "$msg" && sleep 0.3 && tmux send-keys -t "=$name:" Enter) &
  else
    set -- "$@" "$msg"
  fi
fi

"$@"
rc=$?
if [ $rc -ne 0 ]; then
  echo "box: $1 exited with $rc; press enter to close the session"
  read -r _
fi
