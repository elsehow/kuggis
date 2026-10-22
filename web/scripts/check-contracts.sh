#!/bin/sh
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
generated=$(mktemp -d "${TMPDIR:-/tmp}/kuggis-contracts.XXXXXX")
trap 'rm -rf "$generated"' EXIT HUP INT TERM
(cd "$repo_root/cell" && TS_RS_EXPORT_DIR="$generated" TS_RS_LARGE_INT=number cargo test --locked contracts::tests::generate_types)
diff -ru "$repo_root/web/src/generated" "$generated"
