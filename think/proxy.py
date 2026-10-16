#!/usr/bin/python3 -I
"""The room's only road out: an HTTPS CONNECT proxy on 127.0.0.1 that tunnels
to the hosts in its allow-list and refuses everything else.

    proxy.py PORT HOSTFILE [HOSTFILE...]

Each host file lists one host per line (`#` comments). The room's sandbox lets
the agent reach only this port, and TLS runs end to end through the tunnel, so
the proxy sees hostnames, never content. It runs outside the room, as the
person, started by `kuggis watch`.
"""
import http.server
import select
import socket
import socketserver
import sys


def hosts_in(paths):
    allowed = set()
    for path in paths:
        try:
            with open(path, encoding="utf-8") as f:
                allowed |= {l.strip().lower() for l in f if l.strip() and not l.lstrip().startswith("#")}
        except OSError:
            pass
    return allowed


class Tunnel(http.server.BaseHTTPRequestHandler):
    allowed = frozenset()
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # one line per decision, no bodies
        sys.stderr.write("proxy: " + fmt % args + "\n")

    def do_CONNECT(self):
        host, _, port = self.path.rpartition(":")
        if host.lower() not in self.allowed or not port.isdigit() or int(port) not in (443, 8443):
            self.send_error(403, f"{self.path} is not on the room's allow-list")
            return
        try:
            upstream = socket.create_connection((host, int(port)), timeout=15)
        except OSError as e:
            self.send_error(502, f"{host}: {e}")
            return
        self.send_response(200, "Connection established")
        self.end_headers()
        self.close_connection = True  # the socket carries TLS from here on, never another request
        self.pipe(self.connection, upstream)

    def pipe(self, a, b):
        pairs = {a: b, b: a}
        try:
            while pairs:
                ready, _, _ = select.select(list(pairs), [], [], 300)
                if not ready:
                    break
                for s in ready:
                    data = s.recv(65536)
                    if not data:
                        return
                    pairs[s].sendall(data)
        except OSError:
            pass
        finally:
            b.close()

    def do_GET(self):  # plain HTTP never leaves the room
        self.send_error(405, "the room's proxy only tunnels CONNECT to allowed hosts")

    do_POST = do_PUT = do_HEAD = do_DELETE = do_OPTIONS = do_PATCH = do_GET


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(port, hostfiles):
    Tunnel.allowed = frozenset(hosts_in(hostfiles))
    with Server(("127.0.0.1", port), Tunnel) as server:
        sys.stderr.write(f"proxy: 127.0.0.1:{port} → {', '.join(sorted(Tunnel.allowed)) or 'nothing'}\n")
        server.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) < 3 or not sys.argv[1].isdigit():
        sys.exit("usage: proxy.py PORT HOSTFILE...")
    serve(int(sys.argv[1]), sys.argv[2:])
