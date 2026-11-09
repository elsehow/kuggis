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
