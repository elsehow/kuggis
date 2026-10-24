#!/usr/bin/env python3
"""Local CLI -> signed requests -> TLS gateway -> private-site verification.

Requires Linux (the artifact packer uses descriptor-relative /proc traversal).
Uses a fixture signer in place of the production separate-UID bridge (covered by
Rust and test-publishing-live.py). All hosts resolve through a local CONNECT
proxy; no DNS changes, sudo, downloads, or external network are needed.
"""
import http.client
import http.server
import json
import os
from pathlib import Path
import select
import socket
import socketserver
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time

native = os.environ.get('KUGGIS_TEST_NATIVE_PORTS') == '1'
binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'cell/target/debug/cell').resolve()

def run(*args, **kwargs):
    result = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True, **kwargs)
    if result.returncode: raise RuntimeError(f'{args}: {result.stderr}\n{result.stdout}')
    return result.stdout.strip()

def port():
    with socket.socket() as stream:
        stream.bind(('127.0.0.1', 0))
        return stream.getsockname()[1]

def background(server):
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server

with tempfile.TemporaryDirectory(prefix='private-site-https-', dir='/tmp') as directory:
    work = Path(directory)
    secret = work / 'secret'
    public_key = run('key', 'new', secret)
    registry = work / 'registry'
    audience = run('publisher', 'init', registry)
    private_config = work / 'private.json'
    private_config.write_text(json.dumps({'domain': 'publisher.example', 'ports': {'first': 4900, 'last': 4901}, 'users': ['reader']} if native else {'domain': 'private.example', 'users': ['reader']}))
    run('publisher', 'configure-private', registry, private_config)
    cells = [{'cell': {'name': 'author', 'grants': [{'publish': 'private'}, {'host': 'publisher.example'}]}, 'parent': None, 'key': public_key}]
    reply = json.loads(run('publisher', 'resource-rpc', registry, input=json.dumps({'reconcile_publishing': {'provider': audience, 'endpoint': 'https://publisher.example', 'cells': cells}})))
    config = reply['publishing']['author']['config']
    config['socket'] = str(work / 'bridge.sock')
    config_path = work / 'publisher.json'
    config_path.write_text(json.dumps(config))
    cert, key = work / 'cert.pem', work / 'tls.key'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=publisher.example', '-addext', 'subjectAltName=DNS:publisher.example,DNS:*.private.example', '-keyout', str(key), '-out', str(cert)], check=True, capture_output=True)
    control, private = port(), 4900 if native else port()
    corrupt_asset = False
    expose_anonymous = False

    class Gateway(http.server.BaseHTTPRequestHandler):
        def forward(self):
            host = self.headers['Host']
            is_control = host == 'publisher.example'
            if native and not is_control:
                token = self.headers.get('X-Exedev-Authorization')
                proof = self.headers.get('X-Cell-Site-Verification')
                assert not token or proof, 'Credential leaked into anonymous check'
                if proof and token != 'Bearer fixture-only':
                    self.send_response(403); self.end_headers(); return
            data = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            headers = {'Host': host, 'X-Forwarded-Host': host, 'X-Forwarded-Proto': 'https', 'Content-Type': 'application/json'}
            if 'X-Cell-Site-Verification' in self.headers:
                headers['X-Cell-Site-Verification'] = self.headers['X-Cell-Site-Verification']
            if expose_anonymous is True or (expose_anonymous == "asset" and self.path.endswith("/data.json")): headers['X-ExeDev-UserID'] = 'reader'
            upstream = http.client.HTTPConnection('127.0.0.1', control if is_control else int(host.rsplit(':', 1)[1]) if native else private, timeout=60)
            upstream.request(self.command, self.path, data, headers)
            response = upstream.getresponse()
            body = response.read()
            if corrupt_asset and not is_control and self.path.endswith('/data.json') and response.status == 200:
                body = b'{"stale":true}'
            self.send_response(response.status)
            for name, value in response.getheaders():
                if name.lower() not in {'connection', 'transfer-encoding', 'content-length'}: self.send_header(name, value)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            upstream.close()
        do_GET = forward
        do_POST = forward
        def log_message(self, *_): pass

    tls = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    tls.socket = context.wrap_socket(tls.socket, server_side=True)
    background(tls)

    class Connect(socketserver.StreamRequestHandler):
        def handle(self):
            line = self.rfile.readline().decode().strip().split()
            if len(line) != 3 or line[0] != 'CONNECT': return
            host, _, target_port = line[1].rpartition(':')
            if target_port not in (['443', '4900', '4901'] if native else ['443']) or not (host == 'publisher.example' or host.endswith('.private.example')): return
            while self.rfile.readline().strip(): pass
            with socket.create_connection(('127.0.0.1', tls.server_port)) as upstream:
                self.wfile.write(b'HTTP/1.1 200 Connection established\r\n\r\n'); self.wfile.flush()
                while True:
                    readable, _, _ = select.select([self.connection, upstream], [], [], 15)
                    if not readable: return
                    for source in readable:
                        data = source.recv(65536)
                        if not data: return
                        (upstream if source is self.connection else self.connection).sendall(data)
    class Proxy(socketserver.ThreadingTCPServer):
        daemon_threads = True
    proxy = background(Proxy(('127.0.0.1', 0), Connect))
    env = dict(os.environ, HTTPS_PROXY=f'http://127.0.0.1:{proxy.server_address[1]}', https_proxy=f'http://127.0.0.1:{proxy.server_address[1]}', HTTP_PROXY='', ALL_PROXY='', NO_PROXY='', no_proxy='', CURL_CA_BUNDLE=str(cert))

    if native:
        credential = work / 'ingress.headers'
        credential.write_text('X-Exedev-Authorization: Bearer fixture-only\n'); credential.chmod(0o600)
        env['CELL_PUBLISH_INGRESS_HEADERS'] = str(credential)

    class Signer(socketserver.StreamRequestHandler):
        def handle(self):
            length = struct.unpack('>I', self.rfile.read(4))[0]
            command = self.rfile.read(length).decode()
            signed = json.loads(run('publisher', 'sign', '--secret', secret, '--audience', audience, input=command))
            data = json.dumps({'Ok': signed}).encode()
            self.wfile.write(struct.pack('>I', len(data)) + data)
    signer = background(socketserver.ThreadingUnixStreamServer(config['socket'], Signer))
    processes = []
    try:
        for service, listen in [('serve', control), ('private-host', private)]:
            log = open(work / f'{service}.log', 'w')
            process = subprocess.Popen([str(binary), 'publisher', service, str(registry), '--listen', f'127.0.0.1:{listen}'], stdout=log, stderr=log, env=env)
            log.close(); processes.append(process)
            for _ in range(100):
                if process.poll() is not None: raise RuntimeError((work / f'{service}.log').read_text())
                try:
                    socket.create_connection(('127.0.0.1', listen), timeout=.1).close(); break
                except OSError: time.sleep(.03)
            else: raise RuntimeError(f'{service} did not start')
        dist = work / 'dist'; dist.mkdir()
        (dist / 'index.html').write_text('<!doctype html><h1>private artifact</h1><script src="./app.js"></script>')
        (dist / 'app.js').write_text('fetch("./data.json")')
        (dist / 'data.json').write_text('{"private":true}')
        def publish(*args): return json.loads(run('publish', *args, '--config', config_path, env=env))
        first = publish(dist, '--site', 'demo')
        assert first['published']['visibility'] == 'private'
        assert first['verification']['error'] is None and first['verification']['checked_files'] == 3
        url = first['published']['url']
        assert url == 'https://publisher.example:4900/' if native else url.startswith('https://s') and url.endswith('.private.example/')
        second = publish(dist, '--site', 'demo')
        assert second['published']['url'] == url
        restored = publish('rollback', 'demo', first['published']['version'])
        assert restored['verification']['publication']['version'] == first['published']['version']
        for failure in ['asset', 'anonymous', 'anonymous-asset']:
            corrupt_asset = failure == 'asset'
            expose_anonymous = 'asset' if failure == 'anonymous-asset' else failure == 'anonymous'
            result = subprocess.run([str(binary), 'publish', 'verify', 'demo', '--config', str(config_path)], capture_output=True, text=True, env=env)
            assert result.returncode != 0, result.stdout
            report = json.loads(result.stdout)
            assert report['verified']['error'] and report['verification_error']
        corrupt_asset = expose_anonymous = False
        assert publish('verify', 'demo')['verified']['error'] is None
        publish('unpublish', 'demo')
        assert publish('status', 'demo')['status']['active'] is None
        print('PASS: default-private CLI publishing, full HTTPS verification, stable URLs, rollback, wrong-asset rejection, anonymous-exposure rejection, and unpublish.')
    finally:
        for process in processes:
            process.terminate()
            process.wait(timeout=5)
        for server in [signer, proxy, tls]:
            server.shutdown(); server.server_close()
