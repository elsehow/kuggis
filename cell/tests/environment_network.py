"""Exercise the trusted namespace probe with simulated proxy responses."""
import ast
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


source = Path(__file__).parents[1] / 'src/environment/host.py'
module = ast.parse(source.read_text())
check = next(n for n in module.body if isinstance(n, ast.FunctionDef) and n.name == 'check')
probe = next(n for n in check.body if isinstance(n, ast.FunctionDef) and n.name == 'network_probes')
probe_code = compile(ast.Module(body=[probe], type_ignores=[]), str(source), 'exec')


class NetworkProbe(unittest.TestCase):
    def exercise(self, grants, internet_open):
        targets = []

        class ProxySocket:
            def settimeout(self, timeout):
                pass

            def sendall(self, request):
                self.target = request.decode().split()[1]
                targets.append(self.target)

            def recv(self, size):
                status = {
                    'example.com:443': 200 if internet_open else 403,
                    'example.com:80': 403,
                    '1.1.1.1:443': 400,
                    'door-nobody.int.exe.xyz:443': 200,
                }[self.target]
                return f'HTTP/1.1 {status} Response\r\n\r\n'.encode()

            def close(self):
                pass

        def connect(address, timeout):
            if address == ('10.203.0.1', 3128):
                return ProxySocket()
            raise OSError('direct route blocked')

        def run(*args, **kwargs):
            if args[0] == 'machinectl':
                return SimpleNamespace(stdout='123\n')
            self.assertEqual(args[:6], ('nsenter', '-t', '123', '-n', '/usr/bin/python3', '-c'))
            with patch('socket.create_connection', connect):
                exec(compile(args[6], '<namespace probe>', 'exec'), {})

        with tempfile.TemporaryDirectory() as directory:
            etc = Path(directory)
            (etc / 'cell.json').write_text(json.dumps({'name': 'research', 'grants': grants}))
            scope = dict(run=run, ETC=etc, MACHINE='cell-env', json=json)
            exec(probe_code, scope)
            scope['network_probes']()
        self.assertIn('example.com:80', targets)

    def test_default_and_public_and_explicit_host_policies(self):
        for grants, internet_open in [
            ([], False),
            ([{'network': 'public'}], True),
            ([{'host': 'example.com'}], True),
            ([{'host': 'other.example'}], False),
        ]:
            with self.subTest(grants=grants):
                self.exercise(grants, internet_open)

    def test_policy_drift_fails_the_probe(self):
        for grants, internet_open in [([], True), ([{'network': 'public'}], False)]:
            with self.subTest(grants=grants), self.assertRaises(AssertionError):
                self.exercise(grants, internet_open)


if __name__ == '__main__':
    unittest.main()
