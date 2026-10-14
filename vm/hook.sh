#!/usr/bin/env bash
# Agent hook. For Claude (wired in ~/.claude/settings.json): records the
# session's state (working | idle | waiting) for `box` and pings ntfy when
# it waits on you: a permission prompt or a question it asks with
# AskUserQuestion. A finished turn is idle; an agent that needs you after
# one says so with needs-you. For Codex (`notify` in ~/.codex/config.toml, invoked as
# `hook.sh ping <json>` after each finished turn): pings only.
# Pings are skipped while you're typing in the session. Prints nothing; exits 0.
name=${BOX_SESSION:-}
[ -n "$name" ] || exit 0
state=$1

# The user typed into the session: whatever it needed from them (needs-you), it has.
if [ "$state" = prompt ]; then
  rm -f ~/.box/needs/"$name"
  state=working
fi

if [ "$state" = ping ]; then
  input=${2:-}
  field='."last-assistant-message"'
  touch ~/.box/state/"$name"
else
  input=$(cat)
  field=.message
  mkdir -p ~/.box/state
  prev=$(cat ~/.box/state/"$name" 2>/dev/null)
  echo "$state" >~/.box/state/"$name"
  # Other sessions' status bars show who's waiting; redraw them now.
  if [ "$state" != "$prev" ]; then
    tmux list-clients -F '#{client_name}' 2>/dev/null |
      while read -r c; do tmux refresh-client -S -t "$c"; done >/dev/null 2>&1
  fi
fi

case $state in waiting | ping) ;; *) exit 0 ;; esac
# A sleeping laptop leaves its client attached for a while, so "attached"
# says nothing; a keystroke in the last two minutes means you're there.
last=$(tmux list-clients -t "=$name" -F '#{client_activity}' 2>/dev/null | sort -n | tail -1)
[ -z "$last" ] || [ $(($(date +%s) - last)) -ge 120 ] || exit 0
. ~/.box/config 2>/dev/null
[ -n "${BOX_NTFY:-}" ] || exit 0
msg=$(jq -r "$field // empty" <<<"$input" 2>/dev/null | head -c 300)
curl -fsS -m 5 -H "Title: $BOX_PROJECT/$name" -d "${msg:-waiting on you}" \
  "https://ntfy.sh/$BOX_NTFY" >/dev/null 2>&1 &
exit 0
