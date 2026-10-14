#!/usr/bin/env bash
# tmux status bar for box sessions (see tmux.conf). Prints tmux style markup.
#   status.sh left <session>   project · session in the project's colour, then
#                              the other sessions on this VM waiting on you
#   status.sh right <session>  the worktree's uncommitted and unpushed work:
#                              what makes `box done` refuse
. ~/.box/config 2>/dev/null
STATE=$HOME/.box/state
AGENT=$HOME/.box/agent
WT=$HOME/${BOX_REPO##*/}/.claude/worktrees

# One fixed colour per project, picked from its name, so a wrong VM looks wrong.
colour() {
  local palette=(33 99 36 172 167 70 162 136 31 133)
  local h; h=$(printf %s "$1" | cksum | cut -d' ' -f1)
  echo "colour${palette[h % ${#palette[@]}]}"
}

ago() {
  local s=$1
  if [ "$s" -lt 60 ]; then echo "${s}s"
  elif [ "$s" -lt 3600 ]; then echo "$((s / 60))m"
  elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h"
  else echo "$((s / 86400))d"
  fi
}

left() {
  local self=$1 f name live now st
  printf '#[fg=colour16,bg=%s,bold] %s · %s #[default]' "$(colour "${BOX_PROJECT:-box}")" "${BOX_PROJECT:-box}" "$self"
  live=$(tmux list-sessions -F '#{session_name}' 2>/dev/null)
  now=$(date +%s)
  for f in "$STATE"/*; do
    [ -f "$f" ] || continue
    name=${f##*/}
    [ "$name" != "$self" ] || continue
    grep -qxF "$name" <<<"$live" || continue
    [ "$(cat "$AGENT/$name" 2>/dev/null || echo claude)" = claude ] || continue
    st=$(cat "$f")
    [ "$st" = waiting ] || { [ "$st" = idle ] && [ -f ~/.box/needs/"$name" ]; } || continue
    printf ' #[fg=colour16,bg=colour214] ⚑ %s %s #[default]' "$name" "$(ago $((now - $(stat -c %Y "$f"))))"
  done
}

right() {
  local wt=$WT/$1 out=() n
  [ -d "$wt" ] || return 0
  [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || out+=('✎ uncommitted')
  n=$(git -C "$wt" rev-list --count HEAD --not --remotes 2>/dev/null || echo 0)
  [ "$n" = 0 ] || out+=("↑$n unpushed")
  if [ ${#out[@]} -eq 0 ]; then
    printf '#[fg=colour244]✓ pushed '
  else
    local IFS='|'
    printf '#[fg=colour214]%s ' "${out[*]//|/ · }"
  fi
}

case ${1:-} in
  left) left "${2:-}" ;;
  right) right "${2:-}" ;;
esac
