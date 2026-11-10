#!/bin/sh
# Build kuggis with its matching Linux helper and install the pair locally.
#
#   sh scripts/install.sh [DIRECTORY]
#
# DIRECTORY defaults to $KUGGIS_INSTALL_DIR, then to wherever kuggis is already
# installed on PATH, then to ~/.config/kuggis/bin.
set -eu
if [ "$#" -gt 1 ]; then
    echo 'usage: sh scripts/install.sh [DIRECTORY]' >&2
    exit 2
fi
kuggis_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# The Linux helper needs rustup's toolchain (it has the musl target). A
# system or Homebrew cargo earlier on PATH would shadow it.
if command -v rustup >/dev/null 2>&1 && kuggis_cargo=$(rustup which cargo 2>/dev/null); then
    PATH="$(dirname -- "$kuggis_cargo"):$PATH"
    export PATH
fi

if [ "$#" -eq 1 ]; then
    kuggis_dest=$1
elif [ -n "${KUGGIS_INSTALL_DIR:-}" ]; then
    kuggis_dest=$KUGGIS_INSTALL_DIR
elif kuggis_existing=$(command -v kuggis 2>/dev/null) && [ -f "$kuggis_existing" ] && [ ! -L "$kuggis_existing" ]; then
    kuggis_dest=$(dirname -- "$kuggis_existing")
else
    kuggis_dest=$HOME/.config/kuggis/bin
fi
mkdir -p "$kuggis_dest"
kuggis_dest=$(CDPATH= cd -- "$kuggis_dest" && pwd)

kuggis_stage=$(mktemp -d "$kuggis_dest/.install.XXXXXX")
trap 'rm -rf "$kuggis_stage"' EXIT
sh "$kuggis_root/scripts/package.sh" "$kuggis_stage" >&2

# Check the new client starts before it replaces a working one.
"$kuggis_stage/kuggis" --help >/dev/null

# Rename into place rather than overwrite: on Apple Silicon, rewriting a signed
# executable's contents in place leaves a stale signature and macOS kills it.
for kuggis_file in kuggis-linux-x86_64 checksums.json kuggis; do
    mv -f "$kuggis_stage/$kuggis_file" "$kuggis_dest/$kuggis_file"
done

echo "Installed kuggis and kuggis-linux-x86_64 in $kuggis_dest"
case ":$PATH:" in
    *":$kuggis_dest:"*) ;;
    *) echo "Add $kuggis_dest to PATH to run it as kuggis." ;;
esac
