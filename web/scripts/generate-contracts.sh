#!/bin/sh
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
(cd "$repo_root/cell" && TS_RS_EXPORT_DIR="$repo_root/web/src/generated" TS_RS_LARGE_INT=number cargo test --locked contracts::tests::generate_types)
