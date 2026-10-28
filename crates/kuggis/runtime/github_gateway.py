"""Bounded Git streams. Only the broker opens the fixed upstream SSH service."""
import os
import re
import signal
import socket
import threading
import time
from github_common import Failure
from github_wire import PROTOCOL, Wire, accept


def terminate(process, force=False):
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGKILL if force else signal.SIGTERM)
    except ProcessLookupError:
        pass
    except PermissionError:
        # Some process sandboxes allow signalling only the direct child.
        if force:
            process.kill()
        else:
            process.terminate()


def transfer(handler, process, token, repo, service):
    wire = Wire(handler.rfile, handler.connection)
    done = threading.Event()
    failed = threading.Event()
    active = [time.monotonic()]

    def stop():
        failed.set()
        terminate(process)
        try:
            handler.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    def receive():
        try:
            while not done.is_set():
                opcode, data = wire.receive()
                active[0] = time.monotonic()
                if opcode == 8:
                    stop()
                    return
                if not data:
                    process.stdin.close()
                    return
                process.stdin.write(data)
                process.stdin.flush()
        except Exception:
            if not done.is_set():
                stop()

    def watch():
        while not done.wait(0.5):
            try:
                handler.server.broker.git_grant(token, repo, service)
                if time.monotonic() - active[0] > 300:
                    raise Failure('idle Git session')
            except Exception:
                stop()
                return

    receiver = threading.Thread(target=receive, daemon=True)
    watcher = threading.Thread(target=watch, daemon=True)
    receiver.start()
    watcher.start()
    try:
        while True:
            data = os.read(process.stdout.fileno(), 65536)
            if not data:
                break
            active[0] = time.monotonic()
            wire.send(data)
        code = process.wait(timeout=5)
        wire.close(code == 0 and not failed.is_set())
    except Exception:
        stop()
    finally:
        done.set()
        try:
            handler.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        terminate(process, force=True)
        process.wait()
        receiver.join(timeout=2)
        watcher.join(timeout=2)
        process.stdout.close()
        process.stdin.close()


def serve_git(handler):
    server = handler.server
    reserved = False
    upgraded = False
    process = None
    cell = None
    handler.close_connection = True
    try:
        match = re.fullmatch(r'/git/([^/?]+/[^/?]+)\.git\?service=(git-upload-pack|git-receive-pack)', handler.path)
        if not match:
            raise Failure('unknown Git endpoint', 404)
        repo, service = match.groups()
        auth = handler.headers.get('Authorization', '')
        if not auth.startswith('Bearer ') or len(auth) > 1024:
            raise Failure('unauthorized', 401)
        token = auth[7:]
        if (handler.headers.get('Upgrade', '').lower() != 'websocket'
                or 'upgrade' not in [p.strip().lower() for p in handler.headers.get('Connection', '').split(',')]
                or handler.headers.get('Sec-WebSocket-Version') != '13'
                or handler.headers.get('Sec-WebSocket-Protocol') != PROTOCOL
                or handler.headers.get('Transfer-Encoding')
                or handler.headers.get('Content-Length') not in (None, '0')):
            raise Failure('invalid Git stream upgrade')
        accepted = accept(handler.headers.get('Sec-WebSocket-Key', ''))
        cell, grant = server.broker.git_grant(token, repo, service)
        with server.git_lock:
            if server.git_cells.get(cell, 0) >= 4 or not server.git_slots.acquire(blocking=False):
                raise Failure('too many Git sessions; retry later', 429)
            server.git_cells[cell] = server.git_cells.get(cell, 0) + 1
            reserved = True
        process = server.broker.github.open(grant['upstream_repo'], service)
        handler.connection.settimeout(300)
        handler.send_response(101)
        handler.send_header('Upgrade', 'websocket')
        handler.send_header('Connection', 'Upgrade')
        handler.send_header('Sec-WebSocket-Accept', accepted)
        handler.send_header('Sec-WebSocket-Protocol', PROTOCOL)
        handler.end_headers()
        handler.wfile.flush()
        upgraded = True
        transfer(handler, process, token, repo, service)
    except Exception as error:
        if not upgraded:
            try:
                handler.send_error(error.status if isinstance(error, Failure) else 502,
                                   'Git gateway request denied or unavailable')
            except OSError:
                pass
    finally:
        if process is not None:
            terminate(process, force=True)
            process.wait()
            process.stdout.close()
            process.stdin.close()
        if reserved:
            with server.git_lock:
                server.git_cells[cell] -= 1
                if server.git_cells[cell] == 0:
                    del server.git_cells[cell]
                server.git_slots.release()
