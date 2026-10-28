"""Bounded binary WebSocket framing for the kuggis-git-v1 byte-stream protocol.

Frames are unfragmented, at most 64 KiB. An empty binary frame half-closes the
client's input; the server finishes its output and closes with status 1000.
"""
import base64
import hashlib
import os
import struct
import threading
from github_common import Failure

PROTOCOL = 'kuggis-git-v1'
MAX_FRAME = 65536


def accept(key):
    try:
        if len(base64.b64decode(key, validate=True)) != 16:
            raise ValueError()
    except (ValueError, TypeError):
        raise Failure('invalid WebSocket key') from None
    return base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()


class Wire:
    def __init__(self, stream, socket, client=False):
        self.stream, self.socket, self.client = stream, socket, client
        self.lock = threading.Lock()

    def exact(self, length):
        chunks = bytearray()
        while len(chunks) < length:
            chunk = self.stream.read(length - len(chunks))
            if not chunk:
                raise EOFError('Git gateway disconnected')
            chunks.extend(chunk)
        return bytes(chunks)

    def send(self, data, opcode=2):
        if len(data) > MAX_FRAME or (opcode != 2 and len(data) > 125):
            raise Failure('Git gateway frame too large')
        flag = 128 if self.client else 0
        if len(data) < 126:
            header = bytes([128 | opcode, flag | len(data)])
        elif len(data) < 65536:
            header = bytes([128 | opcode, flag | 126]) + struct.pack('!H', len(data))
        else:
            header = bytes([128 | opcode, flag | 127]) + struct.pack('!Q', len(data))
        if self.client:
            mask = os.urandom(4)
            header += mask
            data = bytes(value ^ mask[i % 4] for i, value in enumerate(data))
        with self.lock:
            self.socket.sendall(header + data)

    def receive(self):
        while True:
            first, second = self.exact(2)
            opcode = first & 15
            if first & 0xf0 != 0x80 or opcode not in (2, 8, 9, 10) or bool(second & 128) == self.client:
                raise Failure('unsupported Git gateway frame')
            size = second & 127
            if opcode != 2 and size > 125:
                raise Failure('invalid control frame')
            if size == 126:
                size = struct.unpack('!H', self.exact(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.exact(8))[0]
            if size > MAX_FRAME:
                raise Failure('Git gateway frame too large')
            mask = self.exact(4) if second & 128 else None
            data = self.exact(size)
            if mask:
                data = bytes(value ^ mask[i % 4] for i, value in enumerate(data))
            if opcode == 9:
                self.send(data, 10)
            elif opcode != 10:
                return opcode, data

    def close(self, success):
        self.send(struct.pack('!H', 1000 if success else 1011), 8)
