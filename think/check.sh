#!/bin/bash
# The Thinker room's OS-level boundary check. Runs as the thinker user
# (`kuggis think --check` does: sudo -u thinker /opt/kuggis-thinker/check <owner>).
# Then paste check-prompt.md into a Thinker session for the tool-level half.
owner=${1:?owner}
fails=0
if [ -t 1 ]; then G=$'\e[32m' R=$'\e[31m' B=$'\e[1m' N=$'\e[0m'; else G='' R='' B='' N=''; fi
ok() { echo "  ${G}✓${N} $*"; }
bad() { echo "  ${R}✗ $*${N}"; fails=$((fails + 1)); }
[ "$(id -un)" = thinker ] || { echo "check.sh: run as thinker" >&2; exit 2; }

echo "The owner's files are out of reach:"
for p in "/Users/$owner" "/Users/$owner/vault" "/Users/$owner/.ssh" "/Users/$owner/.claude" \
  "/Users/$owner/Projects" "/Users/$owner/.config" "/Users/$owner/Library/Keychains"; do
  if ls "$p" >/dev/null 2>&1; then bad "can list $p"; else ok "can't list $p"; fi
done

echo "The room can't be changed from inside:"
for f in /opt/kuggis-thinker /opt/kuggis-thinker/launch /opt/kuggis-thinker/settings.json \
  /opt/kuggis-thinker/fetch-guard /opt/kuggis-thinker/mcp.json /opt/kuggis-thinker/claude \
  /etc/sudoers.d/kuggis-thinker; do
  if [ -w "$f" ]; then bad "can write $f"; else ok "can't write $f"; fi
done
if sudo -n true 2>/dev/null; then bad "thinker can sudo"; else ok "thinker can't sudo"; fi
if id -Gn | tr ' ' '\n' | grep -qx admin; then bad "thinker is an admin"; else ok "thinker isn't an admin"; fi

echo "Its own credential is private:"
t=/Users/thinker/.config/claude-token
if [ "$(stat -f %Lp "$t" 2>/dev/null)" = 600 ]; then ok "$t is 600"; else bad "$t isn't 600"; fi

echo
if [ "$fails" = 0 ]; then echo "${G}${B}✓ All OS-level checks pass.${N}"; else echo "${R}${B}✗ $fails check(s) failed.${N}"; fi
exit "$fails"
