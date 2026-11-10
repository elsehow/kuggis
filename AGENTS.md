# Kuggis architecture

Kuggis v2 uses agents' native terminal UIs over SSH. The v1 browser chat UI,
React frontend, chat gateway, and tree control plane have been removed. Do not
describe them as current features or reintroduce a browser chat frontend.
Browser-based provider sign-in and previews of apps inside cells remain valid
features; neither is a Kuggis chat UI.

The root Cargo workspace is authoritative. CLI source is in
`crates/kuggis/src/`, runtime helpers are in `crates/kuggis/runtime/`, and command
documentation is in `docs/cli.md`. Old checkouts containing `cell/` and `web/`
are historical v1 code. Check the branch and source before discussing features.

## Building, installing and deploying

- `sh scripts/install.sh [DIRECTORY]` builds the client and its matching Linux
  helper and installs the pair (default: where `kuggis` already is on `PATH`,
  else `~/.config/kuggis/bin`). Use it instead of `cargo install`, which omits
  the helper, and instead of copying binaries by hand: overwriting an installed
  executable in place makes macOS kill it. `scripts/package.sh DIR` builds the
  pair without installing.
- The Linux helper needs rustup's toolchain with the
  `x86_64-unknown-linux-musl` target. A Homebrew `cargo` earlier on `PATH`
  lacks it; the scripts above handle that.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D
  warnings`, `cargo test --locked`, and `python3 -m unittest discover -s tests
  -p 'test_*.py' -q` on Python 3.12 with the newest stable Rust.
- `kuggis CELL --upgrade --bin LINUX_HELPER --plan`, then `--restart`, updates
  an existing cell: runtime, built-in agents at their pinned releases, and
  registration of agents the cell lacked. It restarts the cell.

## Agents and authentication

- Built-in agents are Codex and Claude Code, with recipes in `recipes/`. Pi is a
  manual recipe only. Each agent authenticates with its own native login
  (`kuggis login CELL --agent codex|claude`); do not add a managed model gateway
  or Kuggis-supplied model credentials.
- Agent releases are pinned by checksum in `recipes/*/release.json`.
  `python3 scripts/check-agent-releases.py` reports stale pins and `--update`
  rewrites them; a bumped pin needs a live test on a cell before merging.
