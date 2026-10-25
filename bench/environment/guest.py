#!/usr/bin/env python3
"""Deterministic network probes. No external credentials or model calls."""
import argparse
import json
import socket
import struct
import sys


def main():
    p = argparse.ArgumentParser()
    p.add_argument("mode", choices=["tcp", "dns", "unix"])
    p.add_argument("target")
    p.add_argument("port", type=int, nargs="?", default=443)
    a = p.parse_args()
    detail = {"mode": a.mode, "target": a.target, "port": a.port}
    try:
        if a.mode == "unix":
            s = socket.socket(socket.AF_UNIX)
            s.settimeout(3)
            s.connect(a.target)
        elif a.mode == "tcp":
            s = socket.create_connection((a.target, a.port), timeout=3)
        else:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.settimeout(3)
            question = b"\x07example\x03com\x00" + struct.pack("!HH", 1, 1)
            query = struct.pack("!HHHHHH", 0x5321, 0x0100, 1, 0, 0, 0) + question
            s.sendto(query, (a.target, a.port))
            data, _ = s.recvfrom(4096)
            if len(data) < 12 or data[:2] != b"S!":
                raise RuntimeError("Invalid DNS response")
        detail["outcome"] = "connected"
        code = 0
    except (OSError, RuntimeError) as e:
        detail.update(outcome="blocked", reason=str(e))
        code = 10
    print(json.dumps(detail))
    return code


if __name__ == "__main__":
    sys.exit(main())
