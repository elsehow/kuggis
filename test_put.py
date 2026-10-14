"""Tests for kuggis put's argument parsing: python3 -m unittest test_put.py

Runs the real script with ssh, scp and pbcopy stubbed on PATH; each stub logs
its arguments so the test can see what was uploaded and where it was typed.
"""
import os
import stat
import subprocess
import tempfile
import unittest

KUGGIS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "kuggis")

STUBS = {
    # The upload asks the VM for $HOME; everything else just succeeds.
    "ssh": 'echo "ssh $*" >>"$LOG"; case "$*" in *"echo \\$HOME"*) echo /home/box ;; esac',
    "scp": 'echo "scp $*" >>"$LOG"',
    "pbcopy": 'printf "pbcopy %s\\n" "$(cat)" >>"$LOG"',
    # No clipboard image, so a missing file argument falls back to screenshots.
    "osascript": "exit 1",
}


class PutArgs(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.tmp.name
        bin_ = os.path.join(root, "bin")
        os.mkdir(bin_)
        for name, body in STUBS.items():
            p = os.path.join(bin_, name)
            with open(p, "w") as f:
                f.write("#!/usr/bin/env bash\n" + body + "\n")
            os.chmod(p, os.stat(p).st_mode | stat.S_IEXEC)
        conf = os.path.join(root, "conf")
        os.mkdir(conf)
        with open(os.path.join(conf, "vms"), "w") as f:
            f.write("kuggis kuggis-vm elsehow/kuggis -\n")
        # The working directory holds a file named like the project: the bug.
        self.cwd = os.path.join(root, "checkout")
        os.mkdir(self.cwd)
        for name in ("kuggis", "notes.md"):
            with open(os.path.join(self.cwd, name), "w") as f:
                f.write(name + "\n")
        self.log = os.path.join(root, "log")
        open(self.log, "w").close()
        self.env = dict(os.environ, PATH=bin_ + os.pathsep + os.environ["PATH"],
                        KUGGIS_CONFIG=conf, LOG=self.log, HOME=root)

    def tearDown(self):
        self.tmp.cleanup()

    def put(self, *args):
        r = subprocess.run([KUGGIS, "put", *args], cwd=self.cwd, env=self.env,
                           capture_output=True, text=True, timeout=30)
        with open(self.log) as f:
            return r, f.read().splitlines()

    def test_project_named_like_a_file_in_cwd(self):
        r, log = self.put("kuggis", "my-session", "notes.md")
        self.assertEqual(r.returncode, 0, r.stderr)
        scp = [l for l in log if l.startswith("scp ")]
        self.assertEqual(len(scp), 1, log)
        self.assertIn(" notes.md kuggis-vm.exe.xyz:inbox/", scp[0])
        self.assertTrue(scp[0].endswith("-notes.md"), scp[0])
        typed = [l for l in log if "tmux send-keys" in l]
        self.assertEqual(len(typed), 1, log)
        self.assertIn("kuggis-vm.exe.xyz", typed[0])
        self.assertIn("-t '=my-session:'", typed[0])
        self.assertIn("/home/box/inbox/", typed[0])
        self.assertIn("typed into kuggis/my-session: /home/box/inbox/", r.stdout)

    def test_missing_file_is_named(self):
        r, log = self.put("kuggis", "my-session", "nope.md")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no such file: nope.md", r.stderr)
        self.assertEqual(log, [])

    def test_too_many_arguments(self):
        r, log = self.put("kuggis", "my-session", "notes.md", "extra")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("usage: kuggis put [project [session [file]]]", r.stderr)
        self.assertEqual(log, [])

    def test_two_arguments_are_project_and_session(self):
        # notes.md exists, but in second place it is a session name, not a file.
        r, log = self.put("kuggis", "notes.md")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("bad session name 'notes.md'", r.stderr)
        self.assertEqual(log, [])


if __name__ == "__main__":
    unittest.main()
