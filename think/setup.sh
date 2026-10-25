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
for f in launch fetch-guard; do install -o root -g wheel -m 755 "$here/$f" "$room/$f"; done
for f in settings.json prompt.md check-prompt.md door.py; do install -o root -g wheel -m 644 "$here/$f" "$room/$f"; done
# mcp.json: the kuggis door (think/door.py) always; BigBrain's door is added by
# hand once it's connected, so an existing file keeps its other entries.
[ -f "$room/mcp.json" ] || install -o root -g wheel -m 644 "$here/mcp.json" "$room/mcp.json"
/usr/bin/python3 -I - "$room/mcp.json" <<'PY'
import json, sys
path = sys.argv[1]
data = json.load(open(path))
data.setdefault("mcpServers", {})["kuggis"] = {"command": "/usr/bin/python3", "args": ["-I", "/opt/kuggis-thinker/door.py", "serve"]}
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
