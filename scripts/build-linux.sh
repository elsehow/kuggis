#!/bin/sh
# All runtime dependencies are Rust; musl and rust-lld come from the toolchain.
set -eu
kuggis_target=${1:-x86_64-unknown-linux-musl}
case "$kuggis_target" in
  x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
  *) echo 'expected x86_64-unknown-linux-musl or aarch64-unknown-linux-musl' >&2; exit 2 ;;
esac
kuggis_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
rustup target add "$kuggis_target"
kuggis_libdir=$(rustc --print target-libdir)
kuggis_linker="$kuggis_libdir/../bin/rust-lld"
case "$kuggis_target" in
  x86_64-unknown-linux-musl) export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$kuggis_linker" ;;
  aarch64-unknown-linux-musl) export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$kuggis_linker" ;;
esac
cargo build --manifest-path "$kuggis_root/Cargo.toml" --locked --release --target "$kuggis_target"
