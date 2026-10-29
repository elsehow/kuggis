#!/bin/sh
# Package the local client and matching Linux helper together; recipes are embedded.
set -eu
if [ "$#" -ne 1 ]; then
    echo 'usage: sh scripts/package.sh OUTPUT_DIRECTORY' >&2
    exit 2
fi
kuggis_package_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$kuggis_package_root/Cargo.toml" --locked --release
sh "$kuggis_package_root/scripts/build-linux.sh" x86_64-unknown-linux-musl
mkdir -p "$1"
python3 "$kuggis_package_root/scripts/package-artifacts.py" "$kuggis_package_root/target" "$1"
echo "Packaged in $1. Keep both executables together. Requires Python 3.11+, OpenSSH and curl."
