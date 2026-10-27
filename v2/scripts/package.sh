#!/bin/sh
# Package the local client and matching Linux helper together; recipes are embedded.
set -eu
if [ "$#" -ne 1 ]; then
    echo 'usage: sh v2/scripts/package.sh OUTPUT_DIRECTORY' >&2
    exit 2
fi
kuggis_package_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$kuggis_package_root/Cargo.toml" --locked --release
sh "$kuggis_package_root/scripts/build-linux.sh" x86_64-unknown-linux-musl
mkdir -p "$1"
cp "$kuggis_package_root/target/release/kuggis" "$1/kuggis"
cp "$kuggis_package_root/target/x86_64-unknown-linux-musl/release/kuggis" "$1/kuggis-linux-x86_64"
python3 -c 'import hashlib,json,sys; from pathlib import Path; p=Path(sys.argv[1]); (p/"checksums.json").write_text(json.dumps({n:hashlib.sha256((p/n).read_bytes()).hexdigest() for n in ["kuggis","kuggis-linux-x86_64"]},indent=2)+"\n")' "$1"
echo "Packaged in $1. Keep both executables together. Requires Python 3.11+, OpenSSH and curl."
