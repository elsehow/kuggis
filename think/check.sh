#!/bin/bash
# The Thinker room's OS-level boundary check. Runs as the thinker user
# (`kuggis think --check` does: sudo -u thinker /opt/kuggis-thinker/check <owner>).
# Then paste check-prompt.md into a Thinker session for the tool-level half.
owner=${1:?owner}
room=/opt/kuggis-thinker
PROXY=4749 BB=4748
[ ! -f "$room/ports" ] || . "$room/ports"
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
for f in $room $room/launch $room/settings.json $room/room.sb $room/door.py $room/mcp.json $room/claude \
  /etc/sudoers.d/kuggis-thinker; do
  if [ -w "$f" ]; then bad "can write $f"; else ok "can't write $f"; fi
done
if sudo -n true 2>/dev/null; then bad "thinker can sudo"; else ok "thinker can't sudo"; fi
if id -Gn | tr ' ' '\n' | grep -qx admin; then bad "thinker is an admin"; else ok "thinker isn't an admin"; fi
if grep -qx thinker /usr/lib/cron/cron.deny 2>/dev/null; then ok "thinker can't use cron"; else bad "thinker may use cron (runs outside the wall)"; fi

echo "Its own credential is private:"
t=/Users/thinker/.config/claude-token
if [ "$(stat -f %Lp "$t" 2>/dev/null)" = 600 ]; then ok "$t is 600"; else bad "$t isn't 600"; fi

echo "Inside the wall (room.sb) there is no network but the two doors:"
# Each probe runs as the agent would: under the sandbox. A python one-liner
# says what the kernel answered.
inside() { /usr/bin/sandbox-exec -D PROXY="$PROXY" -D BB="$BB" -f "$room/room.sb" /usr/bin/python3 -I -c "$1" 2>&1 | tail -1; }
r=$(inside 'import socket
try: socket.create_connection(("1.1.1.1", 443), timeout=3); print("connected")
except OSError as e: print("refused" if e.errno == 1 else f"other: {e}")')
[ "$r" = refused ] && ok "a direct connection to the internet is refused" || bad "direct connection: $r"
r=$(inside 'import socket
try: socket.getaddrinfo("example.com", 443); print("resolved")
except OSError: print("refused")')
[ "$r" = refused ] && ok "a name lookup is refused (names carry data out)" || bad "name lookup: $r"
r=$(inside 'import socket
try: socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b"x", ("1.1.1.1", 53)); print("sent")
except OSError as e: print("refused" if e.errno == 1 else f"other: {e}")')
[ "$r" = refused ] && ok "UDP is refused" || bad "UDP: $r"
r=$(inside 'import socket
try: socket.socket().bind(("127.0.0.1", 0)); print("bound")
except OSError as e: print("refused" if e.errno == 1 else f"other: {e}")')
[ "$r" = refused ] && ok "listening is refused" || bad "listen: $r"
r=$(inside 'import socket
try: socket.create_connection(("127.0.0.1", 631), timeout=3); print("connected")
except OSError as e: print("refused" if e.errno == 1 else "closed")')
[ "$r" = refused ] && ok "another loopback port (631) is refused by the wall, not merely closed" || bad "other loopback port: $r"
r=$(inside "import socket
try: socket.create_connection(('127.0.0.1', $PROXY), timeout=3); print('connected')
except OSError as e: print('refused' if e.errno == 1 else 'closed')")
case $r in
  connected) ok "the proxy port ($PROXY) is reachable" ;;
  closed) ok "the proxy port ($PROXY) is allowed (nothing listening now: kuggis watch starts it)" ;;
  *) bad "proxy port: $r" ;;
esac
if [ "$r" = connected ]; then
  r=$(inside "import http.client
c = http.client.HTTPConnection('127.0.0.1', $PROXY, timeout=10); c.request('CONNECT', 'example.com:443'); print(c.getresponse().status)")
  [ "$r" = 403 ] && ok "the proxy refuses a host off its list" || bad "proxy to example.com: $r"
  r=$(inside "import http.client
c = http.client.HTTPConnection('127.0.0.1', $PROXY, timeout=10); c.request('CONNECT', 'api.anthropic.com:443'); print(c.getresponse().status)")
  [ "$r" = 200 ] && ok "the proxy tunnels to the model API" || bad "proxy to api.anthropic.com: $r"
fi

echo
if [ "$fails" = 0 ]; then echo "${G}${B}✓ All OS-level checks pass.${N}"; else echo "${R}${B}✗ $fails check(s) failed.${N}"; fi
exit "$fails"
