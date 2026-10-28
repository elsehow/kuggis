#!/usr/bin/python3
"""Git remote-helper: native Git pack protocol over an authenticated TLS gateway."""
import base64
import http.client
import os
import socket
import ssl
import struct
import sys
import threading
import urllib.parse
import urllib.request
from github_common import Failure, broker_url, repository
from github_wire import PROTOCOL, Wire, accept
from github_guest import credential_result


def connect(url, repo, service, credential):
    parsed = urllib.parse.urlsplit(broker_url(url))
    context = ssl.create_default_context()
    proxy = urllib.request.getproxies().get('https')
    if proxy and not urllib.request.proxy_bypass(parsed.hostname):
        via = urllib.parse.urlsplit(proxy)
        if via.scheme != 'http' or not via.hostname or via.username or via.password:
            raise Failure('Git gateway requires an HTTP CONNECT proxy without URL credentials')
        connection = http.client.HTTPSConnection(via.hostname, via.port or 80, timeout=300, context=context)
        connection.set_tunnel(parsed.hostname, parsed.port or 443)
    else:
        connection = http.client.HTTPSConnection(parsed.hostname, parsed.port or 443, timeout=300, context=context)
    key = base64.b64encode(os.urandom(16)).decode()
    try:
        connection.request('GET', '/git/' + repository(repo) + '.git?service=' + service,
            headers={'Authorization': 'Bearer ' + credential, 'Connection': 'Upgrade', 'Upgrade': 'websocket',
                     'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Protocol': PROTOCOL})
        response = connection.getresponse()
        if (response.status != 101 or response.getheader('Sec-WebSocket-Accept') != accept(key)
                or response.getheader('Sec-WebSocket-Protocol') != PROTOCOL
                or response.getheader('Upgrade', '').lower() != 'websocket'):
            raise Failure('Git gateway refused the connection (HTTP %d)' % response.status)
        return connection, response, Wire(response.fp, connection.sock, client=True)
    except BaseException:
        connection.close()
        raise


def relay(repo, service, source, destination):
    if service not in ('git-upload-pack', 'git-receive-pack'):
        raise Failure('unsupported Git service')
    issued = credential_result(repo)
    if issued.get('transport') != 'ssh-gateway':
        raise Failure('this remote requires an SSH Git gateway connection')
    connection, response, wire = connect(issued['url'], repo, service, issued['token'])
    # Only acknowledge connect after the gateway has authenticated and authorized it.
    destination.write(b'\n')
    destination.flush()
    errors = []
    def send():
        try:
            while True:
                data = os.read(source.fileno(), 65536)
                wire.send(data)
                if not data:
                    break
        except Exception:
            errors.append(True)
            try:
                connection.sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
    thread = threading.Thread(target=send, daemon=True)
    thread.start()
    try:
        while True:
            opcode, data = wire.receive()
            if opcode == 8:
                if len(data) < 2 or struct.unpack('!H', data[:2])[0] != 1000 or errors:
                    raise Failure('Git gateway transfer failed or access was revoked')
                return
            destination.write(data)
            destination.flush()
    finally:
        try:
            connection.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        response.close()
        connection.close()


def main(args, source=None, destination=None):
    source, destination = source or sys.stdin.buffer, destination or sys.stdout.buffer
    if len(args) != 2:
        raise Failure('expected Git remote name and repository')
    value = args[1]
    if value.endswith('.git'):
        value = value[:-4]
    repo = repository(value)
    while True:
        line = source.readline(1025)
        if len(line) > 1024:
            raise Failure('invalid remote-helper command')
        if line == b'capabilities\n':
            destination.write(b'connect\n\n')
            destination.flush()
        elif line.startswith(b'connect '):
            relay(repo, line[8:].strip().decode('ascii'), source, destination)
            return
        elif line in (b'', b'\n'):
            return
        else:
            raise Failure('unsupported remote-helper command')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as error:
        print('kuggis git: ' + (str(error) if isinstance(error, Failure) else 'gateway connection failed'), file=sys.stderr)
        sys.exit(1)
