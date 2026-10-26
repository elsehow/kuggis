"""Exercise the bootstrap protocol without root, SSH, or a provider account."""
import hashlib
import http.client
import ast
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch, mock_open

spec = importlib.util.spec_from_file_location('ssh_host', Path(__file__).parents[1] / 'src/ssh_host.py')
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)


class Bootstrap(unittest.TestCase):
    def request(self, **changes):
        binary = b'\x7fELF' + bytes(60)
        header = dict(size=len(binary), binary=hashlib.sha256(binary).hexdigest(),
                      before={'machine_id': 'original'}, cell={'name': 'worker', 'grants': []},
                      runtime={'backend': 'ssh', 'dns': ['127.0.0.53'], 'memory_max': 8589934592})
        header.update(changes)
        return json.dumps(header).encode() + b'\n' + binary

    def test_invalid_stream_never_reaches_host_mutations(self):
        streams = [b'', b'{}', self.request(size=-1), self.request(size=2**40),
                   self.request()[:-1], self.request() + b'extra', self.request(binary='0' * 64)]
        with patch.object(host, 'observe') as observe, patch.object(host.subprocess, 'run') as run:
            for stream in streams:
                with self.subTest(stream=stream[:30]), self.assertRaises(Exception):
                    host.apply(io.BytesIO(stream))
            observe.assert_not_called()
            run.assert_not_called()

    def test_rechecks_host_under_lock_before_staging_or_execution(self):
        with patch('builtins.open', mock_open()), patch.object(host.fcntl, 'flock') as lock, \
             patch.object(host, 'observe', return_value={'machine_id': 'replacement'}), \
             patch.object(host.tempfile, 'mkdtemp') as stage, patch.object(host.subprocess, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'changed after approval'):
                host.apply(io.BytesIO(self.request()))
            lock.assert_called_once()
            stage.assert_not_called()
            run.assert_not_called()

    def test_success_executes_exact_staged_files_without_a_shell(self):
        with tempfile.TemporaryDirectory() as root:
            stage = Path(root) / 'stage'; stage.mkdir()
            def execute(args, **kwargs):
                self.assertEqual(args, [str(stage / 'cell'), 'setup', str(stage / 'cell.json'), '--host-runtime', str(stage / 'host.json')])
                self.assertEqual(json.loads((stage / 'cell.json').read_text())['name'], 'worker')
                self.assertEqual((stage / 'cell').read_bytes(), b'\x7fELF' + bytes(60))
                self.assertEqual((stage / 'cell').stat().st_mode & 0o777, 0o700)
                self.assertNotIn('shell', kwargs)
            # Path.write_bytes uses io.open; only the host lock uses builtins.open.
            with patch('builtins.open', mock_open()), patch.object(host.fcntl, 'flock'), \
                 patch.object(host, 'observe', return_value={'machine_id': 'original'}), \
                 patch.object(host.tempfile, 'mkdtemp', return_value=str(stage)), \
                 patch.object(host.subprocess, 'run', side_effect=execute) as run:
                host.apply(io.BytesIO(self.request()))
                run.assert_called_once()
                self.assertFalse(stage.exists())

    def test_failure_is_not_retried_and_keeps_diagnostic_files(self):
        with tempfile.TemporaryDirectory() as root:
            stage = Path(root) / 'stage'; stage.mkdir()
            with patch('builtins.open', mock_open()), patch.object(host.fcntl, 'flock'), \
                 patch.object(host, 'observe', return_value={'machine_id': 'original'}), \
                 patch.object(host.tempfile, 'mkdtemp', return_value=str(stage)), \
                 patch.object(host.subprocess, 'run', side_effect=RuntimeError('setup failed')) as run:
                with self.assertRaisesRegex(RuntimeError, 'setup failed'):
                    host.apply(io.BytesIO(self.request()))
                run.assert_called_once()
                self.assertTrue((stage / 'cell.json').exists())

    def test_observation_reads_latest_check_and_not_credentials(self):
        with tempfile.TemporaryDirectory() as root:
            files = {'etc/os-release': 'ID=ubuntu\nVERSION_ID="24.04"\n',
                     'etc/machine-id': 'a'*32, 'proc/1/comm': 'systemd\n',
                     'etc/resolv.conf': '# stub\nnameserver 127.0.0.53\n',
                     'proc/meminfo': 'MemTotal: 16000000 kB\n',
                     'etc/cell/key.pub': 'public-key\n',
                     'var/lib/cell/said/9.json': json.dumps({'seq': 9, 'said': {'checked': {'ok': False}}}),
                     'var/lib/cell/said/10.json': json.dumps({'seq': 10, 'said': {'checked': {'ok': True}}}),
                     'var/lib/cell/said/11.json': json.dumps({'seq': 11, 'said': {'session': {}}})}
            for name, content in files.items():
                path = Path(root) / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_text(content)
            with patch.object(host, 'Path', side_effect=lambda p: Path(root) / str(p).lstrip('/')), \
                 patch.object(host.os, 'getuid', return_value=0), patch.object(host.platform, 'machine', return_value='x86_64'):
                observed = host.observe()
                self.assertEqual(observed['checked']['seq'], 10)
                self.assertEqual(observed['dns'], ['127.0.0.53'])
                self.assertEqual(observed['memory_total'], 16000000 * 1024)
                self.assertNotIn('secret', observed)


class EnvironmentBoundary(unittest.TestCase):
    def check(self, backend, actual_memory):
        # Execute the real check function and its generated probe source against
        # fake host tools and sockets. No namespaces or network are required.
        source = (Path(__file__).parents[1] / 'src/environment/host.py').read_text()
        tree = ast.parse(source)
        check = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'check')
        captured = []
        targets = []
        expected_memory = 8 * 1024**3 if backend == 'ssh' else 4 * 1024**3
        runtime = {'backend': backend}
        if backend == 'ssh': runtime['memory_max'] = expected_memory
        class Socket:
            def settimeout(self, _): pass
            def sendall(self, data): self.target = data.decode().split()[1]; targets.append(self.target)
            def close(self): pass
            def recv(self, _):
                if self.target == '1.1.1.1:443': status = 400
                elif self.target == 'door-nobody.int.exe.xyz:443': status = 200 if backend == 'exe_dev' else 403
                elif self.target == 'api.openai.com:443': status = 200
                else: status = 403
                return f'HTTP/1.1 {status} status\r\n\r\n'.encode()
        def connect(address, _):
            if address == ('10.203.0.1', 3128): return Socket()
            raise OSError('direct egress refused')
        def run(*args, **kwargs):
            if args[0] == 'systemctl': return SimpleNamespace(stdout=str(actual_memory))
            if args[0] == 'machinectl': return SimpleNamespace(stdout='123')
            if args[0] == '/usr/local/bin/cell': return SimpleNamespace(stdout='api.openai.com\n')
            if args[0] == 'nsenter':
                if 'import http.client' in args[-1]:
                    client = SimpleNamespace(request=lambda *a: None, getresponse=lambda: SimpleNamespace(status=403), close=lambda: None)
                    with patch.object(http.client, 'HTTPConnection', return_value=client):
                        exec(compile(args[-1], '<trusted-git-probes>', 'exec'), {})
                else:
                    with patch.dict('sys.modules', {'socket': SimpleNamespace(create_connection=connect)}):
                        exec(compile(args[-1], '<trusted-network-probes>', 'exec'), {})
            return SimpleNamespace(stdout='')
        with tempfile.TemporaryDirectory() as root:
            uid_map = Path(root) / 'proc/123/uid_map'; uid_map.parent.mkdir(parents=True)
            uid_map.write_text('0 524288 65536\n')
            etc = Path(root) / 'etc/cell'; etc.mkdir(parents=True)
            (etc / 'cell.json').write_text('{"grants":[]}')
            (etc / 'git-credentials.json').write_text('{}')
            original_stat = Path.stat
            def fake_stat(path, *args, **kwargs):
                if path == etc / 'git-credentials.json': return SimpleNamespace(st_uid=0, st_mode=0o600)
                return original_stat(path, *args, **kwargs)
            namespace = dict(run=run, Path=lambda p: Path(root) / str(p).lstrip('/'),
                             ETC=etc,
                             os=SimpleNamespace(readlink=lambda p: str(p)), json=json,
                             MACHINE='cell-env', BASE=524288, SPEC={'uid_count': 65536}, RUNTIME=runtime,
                             guard=lambda: None, guest=lambda *a, **k: None,
                             print=lambda s: captured.extend(json.loads(s)))
            exec(compile(ast.Module(body=[check], type_ignores=[]), '<environment-check>', 'exec'), namespace)
            with patch.object(Path, 'stat', fake_stat): namespace['check']()
        return captured, targets

    def test_ssh_runtime_checks_memory_public_egress_and_refuses_platform_doors(self):
        probes, targets = self.check('ssh', 8 * 1024**3)
        self.assertTrue(all(p['got'] == 'ok' for p in probes), probes)
        self.assertIn('api.openai.com:443', targets)
        self.assertIn('door-nobody.int.exe.xyz:443', targets)

    def test_memory_override_fails_boundary_check(self):
        probes, _ = self.check('ssh', 4 * 1024**3)
        failed = [p for p in probes if p['got'] != 'ok']
        self.assertEqual(len(failed), 1, probes)
        self.assertIn('memory ceiling', failed[0]['name'])

    def test_exe_runtime_keeps_default_memory_and_platform_probe(self):
        probes, targets = self.check('exe_dev', 4 * 1024**3)
        self.assertTrue(all(p['got'] == 'ok' for p in probes), probes)
        self.assertNotIn('api.openai.com:443', targets)


if __name__ == '__main__':
    unittest.main()
