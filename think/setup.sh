#!/bin/bash
# Build or update the Thinker's room. Run as root by `kuggis think --setup`:
#   setup.sh <owner> <claude-binary>   (the Claude token on stdin)
# Idempotent: rerun it to update the room or the Claude Code binary.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "setup.sh: run as root (kuggis think --setup)" >&2; exit 1; }
owner=${1:?owner} bin=${2:?claude binary}
[ "$owner" != root ] && [ -d "/Users/$owner" ] || { echo "setup.sh: $owner is not a person with a home in /Users" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
room=/opt/kuggis-thinker user=thinker home=/Users/thinker
token=$(tr -d '[:space:]')
[[ $token == sk-ant-* ]] || { echo "setup.sh: no Claude token on stdin" >&2; exit 1; }
[ -x "$bin" ] || { echo "setup.sh: $bin is not executable" >&2; exit 1; }

# 1. The owner's home: no other local user may read it (it was 755: vault,
#    projects and transcripts were readable by any account on the machine).
chmod 700 "/Users/$owner"
echo "home: /Users/$owner is now 700"

# 2. The user: standard (not admin), hidden from the login window.
if ! id "$user" >/dev/null 2>&1; then
  sysadminctl -addUser "$user" -fullName "Kuggis Thinker" -shell /bin/zsh -home "$home" \
    -password "$(openssl rand -base64 24)" 2>&1 | grep -v -e '^$' -e 'SecureToken' || true
  dscl . -create "/Users/$user" IsHidden 1
  createhomedir -c -u "$user" >/dev/null
  echo "user: created $user"
fi
id "$user" >/dev/null

# 3. The room: root-owned, so nothing the Thinker writes changes how it runs.
install -d -o root -g wheel -m 755 "$room"
install -o root -g wheel -m 755 "$bin" "$room/claude"
install -o root -g wheel -m 755 "$here/check.sh" "$room/check"
install -o root -g wheel -m 755 "$here/launch" "$room/launch"
for f in settings.json prompt.md check-prompt.md door.py room.sb; do install -o root -g wheel -m 644 "$here/$f" "$room/$f"; done
# The two loopback ports the wall lets through: the kuggis proxy, BigBrain's door.
printf 'PROXY=%s\nBB=%s\n' "${KUGGIS_PROXY_PORT:-4749}" "${BIGBRAIN_API_PORT:-4748}" >"$room/ports"
chmod 644 "$room/ports"
# cron would run the Thinker's jobs outside the wall.
grep -qx "$user" /usr/lib/cron/cron.deny 2>/dev/null || echo "$user" >>/usr/lib/cron/cron.deny
# mcp.json: the room's two doors. kuggis's (think/door.py) over stdio, and
# BigBrain's over HTTP on loopback, with the Thinker's own credential read at
# launch from BIGBRAIN_TOKEN in secrets.env (mint it with
# `bigbrain connect --name Thinker`; no secret sits in this file). Every agent
# reads this one list: Claude and pi take it as is, Codex gets it converted.
install -o root -g wheel -m 644 "$here/mcp.json" "$room/mcp.json"
/usr/bin/python3 -I - "$room/mcp.json" "${BIGBRAIN_API_PORT:-4748}" <<'PY'
import json, sys
path, port = sys.argv[1], sys.argv[2]
data = json.load(open(path))
data.setdefault("mcpServers", {})
data["mcpServers"]["kuggis"] = {"command": "/usr/bin/python3", "args": ["-I", "/opt/kuggis-thinker/door.py", "serve"]}
data["mcpServers"]["bigbrain"] = {"type": "http", "url": f"http://127.0.0.1:{port}/mcp",
                                  "headers": {"Authorization": "Bearer ${BIGBRAIN_TOKEN}"}}
json.dump(data, open(path, "w"), indent=2)
PY
echo "room: $room ($("$room/claude" --version 2>/dev/null | head -1))"

# 4. Its home: a workspace with an outbox the owner can read, its token, and
#    no first-run login picker (token auth).
chmod 711 "$home"
install -d -o "$user" -g staff -m 755 "$home/workspace" "$home/workspace/outbox"
# Its inbox is yours: kuggis writes session and PR status there; it only reads.
install -d -o "$owner" -g staff -m 755 "$home/workspace/inbox"
install -d -o "$user" -g staff -m 700 "$home/.config"
(umask 077; printf '%s\n' "$token" >"$home/.config/claude-token")
chown "$user:staff" "$home/.config/claude-token"
# Keys for the other agents' providers, if the owner keeps any (KEY=value lines).
secrets=${3:-}
if [ -n "$secrets" ] && [ -f "$secrets" ]; then
  install -o "$user" -g staff -m 600 "$secrets" "$home/.config/secrets.env"
  echo "secrets: $(grep -cE '^[A-Z_]+=.+' "$secrets") key(s) from $secrets"
else
  rm -f "$home/.config/secrets.env"
fi
# The other agents run from Homebrew; the room only needs to be able to run them.
for a in codex pi; do
  if sudo -u "$user" test -x "/opt/homebrew/bin/$a"; then echo "agent: $a (kuggis think --agent $a)"; else echo "agent: $a not available to $user"; fi
done
[ -f "$home/.claude.json" ] || {
  echo '{"hasCompletedOnboarding": true}' >"$home/.claude.json"
  chown "$user:staff" "$home/.claude.json"; chmod 600 "$home/.claude.json"
}

# 5. The owner enters without a password, and only through the launcher (or
#    the read-only check).
rule=/etc/sudoers.d/kuggis-thinker
printf '%s ALL=(%s) NOPASSWD: %s/launch, %s/check\n' "$owner" "$user" "$room" "$room" >"$rule.tmp"
chmod 440 "$rule.tmp"
visudo -cqf "$rule.tmp" && mv "$rule.tmp" "$rule"
echo "sudo: $owner may run $room/launch and $room/check as $user"
echo "done. Enter with: kuggis think    Check with: kuggis think --check"
