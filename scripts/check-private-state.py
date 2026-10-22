#!/usr/bin/env python3
"""Reject deployment state and raw run evidence in the Git index.

Use --history to also check paths reachable from HEAD before publishing a
rewritten branch. This is a path-policy check, not a general secret scanner.
"""

import argparse
from pathlib import PurePosixPath
import subprocess
import sys


def private_path(name):
    parts = PurePosixPath(name).parts
    return (
        any(p in {".kuggis", ".pins"} for p in parts)
        or parts[:1] == ("tree",)
        or parts[:3] in {
            ("bench", "environment", "observations"),
            ("bench", "environment", "results"),
        }
        or name == "bench/environment/acceptance.json"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", action="store_true")
    parser.add_argument("--revision", help="Check this commit instead of the index")
    args = parser.parse_args()
    root = subprocess.check_output(["git", "rev-parse", "--show-toplevel"]).decode().strip()
    git = ["git", "-C", root]
    revision = "HEAD"
    if args.revision:
        revision = subprocess.check_output(
            git + ["rev-parse", "--verify", "--end-of-options", args.revision + "^{commit}"]
        ).decode().strip()
        command = ["ls-tree", "-r", "--name-only", "-z", revision]
    else:
        command = ["ls-files", "-z"]
    paths = set(subprocess.check_output(git + command).decode().split("\0"))
    if args.history:
        for line in subprocess.check_output(git + ["rev-list", "--objects", revision]).decode().splitlines():
            _, separator, name = line.partition(" ")
            if separator:
                paths.add(name)
    forbidden = sorted(p for p in paths if p and private_path(p))
    if forbidden:
        print("Keep deployment state and raw evidence in ignored local storage:", file=sys.stderr)
        for name in forbidden:
            print(f"  {name}", file=sys.stderr)
        return 1
    print("Private-state path check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
