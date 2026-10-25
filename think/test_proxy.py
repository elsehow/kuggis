"""The room's proxy: tunnels to allowed hosts, refuses the rest, speaks only CONNECT."""
import http.client
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import proxy  # noqa: E402


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Proxy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        hosts = Path(cls.tmp.name) / "hosts"
        hosts.write_text("# the model API\napi.anthropic.com\n\nExample.COM\n")
        cls.port = free_port()
        proxy.Tunnel.allowed = frozenset(proxy.hosts_in([str(hosts), "/nonexistent"]))
        cls.server = proxy.Server(("127.0.0.1", cls.port), proxy.Tunnel)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.tmp.cleanup()

    def connect(self, target):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request("CONNECT", target)
        r = c.getresponse()
        return r.status, c

    def test_allow_list_is_read_case_insensitively_and_missing_files_are_fine(self):
        self.assertEqual(proxy.Tunnel.allowed, {"api.anthropic.com", "example.com"})

    def test_refuses_hosts_off_the_list_and_odd_ports(self):
        self.assertEqual(self.connect("evil.example.net:443")[0], 403)
        self.assertEqual(self.connect("example.com:22")[0], 403)
        self.assertEqual(self.connect("127.0.0.1:4748")[0], 403)

    def test_plain_requests_never_leave(self):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request("GET", "http://example.com/")
        self.assertEqual(c.getresponse().status, 405)

    def test_tunnels_an_allowed_host(self):
        try:
            socket.create_connection(("example.com", 443), timeout=5).close()
        except OSError:
            self.skipTest("no network")
        import ssl
        sock = socket.create_connection(("127.0.0.1", self.port), timeout=10)
        sock.sendall(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
        head = b""
        while b"\r\n\r\n" not in head:
            head += sock.recv(1)
        self.assertTrue(head.startswith(b"HTTP/1.1 200"), head)
        # TLS end to end through the tunnel: the proxy sees the hostname, never this.
        tls = ssl.create_default_context().wrap_socket(sock, server_hostname="example.com")
        tls.sendall(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n")
        self.assertTrue(tls.recv(64).startswith(b"HTTP/1."))
        tls.close()


if __name__ == "__main__":
    unittest.main()
