"""Local filesystem/protocol and executor tests. These do not prove namespaces."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'crates/kuggis/runtime' / (name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


g = module('guest')
h = module('host')


def frame(data):
    return json.dumps({'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}).encode() + b'\n' + data


class Transfers(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.patcher = patch.object(g, 'WORK', self.root)
        self.patcher.start()

    def tearDown(self):
        self.patcher.stop()
        self.directory.cleanup()

    def test_binary_round_trip_and_permissions(self):
        data = bytes(range(256)) * 1000
        g.put('nested/data.bin', io.BytesIO(frame(data)))
        self.assertEqual((self.root / 'nested/data.bin').stat().st_mode & 0o777, 0o600)
        result = io.BytesIO()
        g.get('nested/data.bin', result)
        self.assertEqual(result.getvalue(), frame(data))

    def test_corrupt_truncated_and_excess_uploads_do_not_publish(self):
        valid = frame(b'\0\xffbinary')
        for invalid in [valid[:-1], valid[:-1] + b'X', valid + b'X']:
            with self.assertRaises(ValueError):
                g.put('file', io.BytesIO(invalid))
            self.assertFalse((self.root / 'file').exists())
            self.assertEqual(list(self.root.iterdir()), [])

    def test_no_overwrite_unless_explicit(self):
        g.put('file', io.BytesIO(frame(b'old')))
        with self.assertRaises(FileExistsError):
            g.put('file', io.BytesIO(frame(b'new')))
        self.assertEqual((self.root / 'file').read_bytes(), b'old')
        g.put('file', io.BytesIO(frame(b'new')), replace=True)
        self.assertEqual((self.root / 'file').read_bytes(), b'new')

    def test_absolute_and_parent_paths_rejected(self):
        for value in ['/etc/passwd', '../outside', 'a/../outside', 'a\nfile', 'a//file']:
            with self.assertRaises(ValueError):
                g.put(value, io.BytesIO(frame(b'bad')))

    def test_read_rejects_symlinks_and_fifos(self):
        (self.root / 'target').write_bytes(b'value')
        (self.root / 'link').symlink_to(self.root / 'target')
        with self.assertRaises(OSError):
            g.get('link', io.BytesIO())
        os.mkfifo(self.root / 'fifo')
        with self.assertRaises(ValueError):
            g.get('fifo', io.BytesIO())

    def test_guest_program_cannot_accidentally_run_on_host(self):
        with patch.object(g.Path, 'exists', return_value=False):
            with self.assertRaisesRegex(RuntimeError, 'inside the guest'):
                g.main(['get', 'anything'])


class HostExecutor(unittest.TestCase):
    def test_ipv4_forwarding_is_enabled_only_for_internet_mode(self):
        for mode in ['restricted', 'internet']:
            with patch.object(h, 'load', return_value={'egress':mode}), patch.object(h, 'run') as run:
                run.return_value.returncode = 0
                h.network()
                calls = [c.args for c in run.call_args_list]
                self.assertEqual(('sysctl','-w','net.ipv4.ip_forward=1') in calls, mode == 'internet')

    def test_workspace_packages_wait_for_guest_apt_sources(self):
        # fd-find/python3-venv require universe, which the minimal debootstrap
        # index does not include. First install must use the same guest package
        # installation path as a reapply.
        class Bootstrapping(Exception):
            pass
        calls = []
        def run(*args, **kwargs):
            calls.append(args)
            if args[0] == 'debootstrap':
                raise Bootstrapping()
            return subprocess.CompletedProcess(args, 1)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(h, 'DATA', root), patch.object(h, 'ROOT', root/'rootfs'), patch.object(h, 'run', run):
                with self.assertRaises(Bootstrapping):
                    h.initialize({'packages': ['fd-find', 'python3-venv']})
        bootstrap = calls[-1]
        included = next(arg for arg in bootstrap if str(arg).startswith('--include='))
        self.assertNotIn('fd-find', included)
        self.assertNotIn('python3-venv', included)
        self.assertIn('systemd', included)

    def test_helper_commands_do_not_consume_caller_input(self):
        # Entry may have a host PTY or protocol stream as stdin. Background
        # checks must get closed input unless a payload is explicitly supplied.
        script = (
            f'import runpy, sys; h = runpy.run_path({str(ROOT / "crates/kuggis/runtime/host.py")!r}); '
            'sys.stdout.buffer.write(h["run"](sys.executable, "-c", '
            '"import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())").stdout)'
        )
        result = subprocess.run([sys.executable, '-c', script], input=b'caller-owned-input',
                                capture_output=True, check=True, timeout=10)
        self.assertEqual(result.stdout, b'')

    def test_guest_arguments_are_argv_not_host_shell(self):
        text = "guest path'; echo HOST; $(printf BAD)"
        argv = h.guest_command(['python3', '/run/kuggis-runtime/guest.py', 'get', text])
        self.assertIn('--machine=kuggis-v2', argv)
        self.assertIn('--uid=agent', argv)
        self.assertIn('--expand-environment=no', argv)
        self.assertEqual(argv[-1], text)
        self.assertNotIn('sh', argv)

    def test_initialized_guest_is_not_traversed_or_rebootstrapped(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'initialized').write_text('2\n')
            (root / 'rootfs').mkdir()
            with patch.object(h, 'DATA', root), patch.object(h, 'ROOT', root / 'rootfs'), patch.object(h, 'run') as run:
                h.initialize({'packages': []})
                run.assert_not_called()

    def test_partial_initial_rootfs_is_not_silently_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'rootfs.build').mkdir()
            with patch.object(h, 'DATA', root), patch.object(h, 'ROOT', root / 'rootfs'), patch.object(h, 'run') as run:
                with self.assertRaisesRegex(RuntimeError, 'partial'):
                    h.initialize({'packages': []})
                run.assert_not_called()

    def test_guard_rejects_firewall_drift(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(h, 'ETC', Path(directory)), \
             patch.object(h, 'normalized', return_value=b'changed'), patch.object(h, 'trusted_bytes', return_value=b'expected'):
            with self.assertRaisesRegex(RuntimeError, 'firewall differs'):
                h.guard()

    def test_crashed_update_blocks_entry_and_boot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'updating').write_text('failed')
            (root / 'setup.lock').write_text('')
            with patch.object(h, 'ETC', root):
                with self.assertRaisesRegex(RuntimeError, 'incomplete'):
                    h.guard()
                with self.assertRaisesRegex(RuntimeError, 'no active installer'):
                    h.guard(installing=True)

    def test_compiled_bundle_agrees_with_executor_artifact_allowlist(self):
        binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
        result = subprocess.run([binary, 'bundle', str(ROOT / 'examples/workspace.json')], check=True, capture_output=True)
        bundle = json.loads(result.stdout)
        h.verify_bundle(bundle)
        bundle['files']['/etc/passwd'] = {'mode': 0o644, 'content': 'bad'}
        with self.assertRaisesRegex(ValueError, 'artifact'):
            h.verify_bundle(bundle)


class ClientInterop(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / 'bin').mkdir()
        (self.root / 'guest').mkdir()
        shutil.copyfile(ROOT / 'tests/fake_ssh.py', self.root / 'bin/ssh')
        (self.root / 'bin/ssh').chmod(0o755)
        self.env = {**os.environ, 'PATH': str(self.root / 'bin') + ':' + str(Path(sys.executable).parent) + ':' + os.environ['PATH'],
                    'KUGGIS_TEST_GUEST': str(ROOT / 'crates/kuggis/runtime/guest.py'),
                    'KUGGIS_TEST_WORKSPACE': str(self.root / 'guest')}
        self.binary = os.environ.get('KUGGIS_BINARY', str(ROOT / 'target/debug/kuggis'))
        self.config = str(ROOT / 'examples/workspace.json')

    def tearDown(self):
        self.directory.cleanup()

    def cli(self, verb, *args, ok=True, **env):
        result = subprocess.run([self.binary, verb, self.config, *map(str, args)], capture_output=True, env={**self.env, **env})
        self.assertEqual(result.returncode == 0, ok, result.stderr.decode(errors='replace'))
        return result

    def test_rust_client_and_python_guest_binary_round_trip(self):
        source = self.root / 'input.bin'
        source.write_bytes(bytes(range(256)) * 1000)
        self.cli('put', source, "files/a'b.bin")
        output = self.root / 'output.bin'
        self.cli('get', "files/a'b.bin", output)
        self.assertEqual(source.read_bytes(), output.read_bytes())

    def test_nonzero_ssh_status_after_valid_data_does_not_publish(self):
        (self.root / 'guest/file').write_bytes(b'complete data')
        output = self.root / 'output.bin'
        self.cli('get', 'file', output, ok=False, KUGGIS_TEST_EXIT_STATUS='1')
        self.assertFalse(output.exists())
        self.assertFalse(list(self.root.glob('.kuggis-transfer-*')))

    def test_client_replacement_is_explicit_and_never_prompts(self):
        source = self.root / 'input'
        source.write_bytes(b'old')
        self.cli('put', source, 'file')
        source.write_bytes(b'new')
        self.cli('put', source, 'file', ok=False)
        self.assertEqual((self.root / 'guest/file').read_bytes(), b'old')
        self.cli('put', source, 'file', '--replace')
        self.assertEqual((self.root / 'guest/file').read_bytes(), b'new')

    def test_installation_framing_and_wrong_platform_rejection(self):
        binary = self.root / 'fixture-elf'
        binary.write_bytes(b'\x7fELF\x02\x01' + bytes(range(256)) * 1000)
        result = self.cli('setup', '--bin', binary)
        self.assertTrue(json.loads(result.stdout)['test_transport_only'])
        binary.write_bytes(b'not a Linux executable')
        self.cli('setup', '--bin', binary, ok=False)

    def test_exe_visibility_is_private_before_installation_and_errors_stop_setup(self):
        config = json.loads(Path(self.config).read_text())
        config['ssh'] = 'test-box.exe.xyz'
        config['ingress'] = {'kind': 'exe', 'vm': 'test-box', 'web': 'preview'}
        self.config = str(self.root / 'config.json')
        Path(self.config).write_text(json.dumps(config))
        binary = self.root / 'fixture-elf'
        binary.write_bytes(b'\x7fELF\x02\x01' + bytes(range(256)))
        log = self.root / 'operations.jsonl'
        self.cli('setup', '--bin', binary, KUGGIS_TEST_LOG=str(log))
        operations = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(operations, [['share', 'set-private', 'test-box'], ['share', 'port', 'test-box', '8000'], ['install']])
        log.write_text('')
        self.cli('setup', '--bin', binary, ok=False, KUGGIS_TEST_LOG=str(log), KUGGIS_TEST_EXE_FAIL='1')
        self.assertEqual(len(log.read_text().splitlines()), 1)


if __name__ == '__main__':
    unittest.main()
