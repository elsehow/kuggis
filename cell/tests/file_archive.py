"""Exercise the same descriptor-based archive helper used by file downloads."""
import io
import os
from pathlib import Path
import socket
import subprocess
import sys
import tarfile
import tempfile
import unittest

HELPER = Path(__file__).parents[1] / "src/chat/archive.py"


class FileArchive(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "results"
        self.root.mkdir()

    def download(self, name="results"):
        fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            return subprocess.run([sys.executable, "-I", str(HELPER), name],
                                  preexec_fn=lambda: os.fchdir(fd), pass_fds=(fd,),
                                  stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
        finally:
            os.close(fd)

    def test_archive_preserves_bytes_names_and_empty_directories(self):
        (self.root / "round 25").mkdir()
        (self.root / "empty").mkdir()
        data = bytes(range(256)) * 1024
        (self.root / "round 25" / "résults #1.csv").write_bytes(data)
        (self.root / "--checkpoint-action=exec=oops").write_text("literal filename")
        result = self.download("forecast run")
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:gz") as archive:
            self.assertEqual(archive.extractfile("forecast run/round 25/résults #1.csv").read(), data)
            self.assertTrue(archive.getmember("forecast run/empty").isdir())
            self.assertEqual(archive.extractfile("forecast run/--checkpoint-action=exec=oops").read(), b"literal filename")
            self.assertTrue(all(not m.name.startswith("/") and ".." not in Path(m.name).parts for m in archive))

    def test_skips_links_and_fifos_without_hanging(self):
        secret = self.root.parent / "private.txt"
        secret.write_text("outside the selected folder")
        (self.root / "outside").symlink_to(secret)
        (self.root / "cycle").symlink_to(self.root)
        (self.root / "missing").symlink_to("missing-target")
        os.mkfifo(self.root / "pipe")
        (self.root / "ok.txt").write_text("included")
        result = self.download()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:gz") as archive:
            self.assertEqual(archive.getnames(), ["results", "results/ok.txt"])

    def test_skips_sockets(self):
        sock = socket.socket(socket.AF_UNIX)
        self.addCleanup(sock.close)
        try:
            sock.bind(str(self.root / "socket"))
        except PermissionError:
            self.skipTest("sandbox forbids creating Unix sockets")
        result = self.download()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:gz") as archive:
            self.assertEqual(archive.getnames(), ["results"])

    @unittest.skipIf(os.geteuid() == 0, "root can read mode-000 files")
    def test_unreadable_file_fails_instead_of_silently_omitting_results(self):
        path = self.root / "unreadable"
        path.write_text("data")
        path.chmod(0)
        self.addCleanup(path.chmod, 0o600)
        result = self.download()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"PermissionError", result.stderr)


if __name__ == "__main__":
    unittest.main()
