# kuggis

Kuggis runs coding agents in cells on exe.dev VMs. Each cell declares its
grants, runs behind a network wall, and speaks through a signed statement
log. Cells form a tree: a child can hold only grants its parent covers.

The implementation is the Rust crate in [cell/](cell/README.md). Its
`cell` binary is the CLI on the laptop and the runtime on each VM.

## Build and test

From the repository root, with Rust installed:

```sh
cargo build --manifest-path cell/Cargo.toml --release --locked
cargo test --manifest-path cell/Cargo.toml --locked
cargo install --path cell --locked
```

The tests exercise parsing, signatures, tree invariants, approval drift,
provisioning against a fake edge, and the door and proxy over local sockets.
They do not provision VMs or call models.

## Open the cell UI

Build the frontend and start the operator UI:

```sh
npm ci --prefix web
npm run build --prefix web
mkdir -p .kuggis/tree
cargo run --manifest-path cell/Cargo.toml -- ui .kuggis/tree \
  --bin cell/target/x86_64-unknown-linux-musl/release/cell
```

Open **http://127.0.0.1:8080**. Choose a cell, then create or resume a chat.
The sidebar supports archive/restore; click a chat title to rename it.
The gear opens provider sign-in, Git scopes, and BigBrain/Portal MCP connections.
Configuration changes are previewed and applied through the existing plan and
approval mechanism, including parent/child grant containment.

Use **＋ Cell** to create a cell or subcell, review its grants, and provision it.
In a cell's settings, **Delete cell…** asks you to type its name before permanently
removing its VM, chats, workspace, credentials, declaration, and pinned key.
Delete subcells first. Deletion disconnects the cell's attachments and removes an
unused parent door; shared integrations remain. Progress survives a browser or
gateway restart, and interrupted deletion can be retried from the cell tree.

The operator serves React and forwards the cell-scoped API over SSH to a private
Unix socket on each VM. The cell runs Pi as `agent` in a persistent Ubuntu container with passwordless
sudo. A host firewall restricts the container's network interface, regardless
of which guest user or process makes a request. Closing the browser or stopping the
operator UI leaves cell runs alive. A cell-service restart preserves transcripts
and metadata but does not automatically repeat unfinished work.

Opening a cell checks chat health independently of that socket. A cleanly
stopped chat service starts automatically; healthy services and failed startup
loops are not restarted by opening a page. Startup failures show a diagnosis
and an explicit recovery action. Workspace repair restores the saved chat
workspace, backs up the service unit, and leaves conversation files untouched.
It refuses custom startup overrides or missing workspaces that need manual
attention. Recovery never replays a prompt or restarts the cell container.
Setup preserves an existing chat workspace when repository grants change, and
newly configured chat services limit automatic starts to five within a minute.

For access without a running laptop, use a dedicated
[hosted supervisor](deploy/README.md) behind exe.dev's private authenticated HTTPS
proxy. The supervisor owns deployment state; each cell retains its chats.

Setup seeds Pi 1.3.0 from a SHA-256-verified bundle. The agent controls its
guest userspace and can install or update tools.
Each cell keeps its own credentials, MCP configuration, and sessions in the
agent's home. Anthropic and OpenAI use Pi’s sign-in and API-key flows;
OpenAI subscription sign-in uses Pi’s headless device-code flow. Connections require an HTTPS MCP endpoint and, when
needed, a bearer token. The UI never returns saved credentials.

For an explicit local workspace with your user permissions:

```sh
cargo run --manifest-path cell/Cargo.toml -- chat --local .
```

This requires a local Pi 1.2+ installation. Local chats remain separate from VM
cells. See [web/README.md](web/README.md) for development and test commands.

## Declare and run cells

Keep live declarations and identity pins in an ignored local directory such
as `.kuggis/tree/`, or outside the source checkout. They describe your
repositories, permissions, providers, and deployment topology. Operator state,
chat data, and raw run evidence are private local state too.
`python3 scripts/check-private-state.py` checks the Git index for these paths;
add `--history` before publishing. `scripts/pre-push` provides the same check
for every outgoing commit when installed as a Git hook. Review content before
publishing reports or fixtures; the check enforces paths, not arbitrary secrets.

[examples/tree/](examples/tree/README.md) contains synthetic declarations for
learning the format. A cell file is `<name>.json`; its children live in the
`<name>/` directory beside it. Development happens in a cell's own workspace;
optional subcells provide separate environments for delegated work.

After creating a cell named `project` in the UI:

```sh
cell tree .kuggis/tree
cell allow .kuggis/tree/project.json
```

Provisioning uses the operator's SSH access to exe.dev. Build a Linux
binary to ship to the VMs; cross-compiling from macOS requires
cargo-zigbuild, Zig, and the Rust target:

```sh
cargo zigbuild --manifest-path cell/Cargo.toml --release --locked --target x86_64-unknown-linux-musl
cell plan .kuggis/tree --bin cell/target/x86_64-unknown-linux-musl/release/cell
```

Inspect the plan, then repeat its approval hash with the same binary:

```sh
cell apply .kuggis/tree --bin cell/target/x86_64-unknown-linux-musl/release/cell --approve HASH
```

Apply observes and replans between actions. It stops if the required changes
exceed the approval. Setup installs the wall and services, runs boundary
probes, and signs the result. Keys are pinned on first contact in
`.kuggis/tree/.pins/`.

Sign in, send a brief, and read the verified log:

```sh
cell login .kuggis/tree project
session_id=$(cell session new)
cell brief .kuggis/tree project "$session_id" "Inspect the code and propose a cleanup."
cell pull .kuggis/tree project
```

Another brief to the same session ID resumes its conversation. For a retry
of the same brief, pass `--request REQUEST_ID` using the ID printed before
submission. A new request ID represents new work.

The provider grants and session statements use a new wire format. Setup upgrades existing VMs while preserving signed logs and their signatures.
A read-only compatibility reader verifies the old wire format. Named Claude
conversations are imported as archived Pi chats; original files are retained. See
[the type migration notes](cell/README.md#type-migration).

## Code map

- `cell/src/prim.rs`, `grant.rs`, `cell.rs`, `tree.rs`: declarations,
  parsed values, and authority checks.
- `plan.rs`, `apply.rs`, `exe.rs`: observation, approval, and reconciliation
  with exe.dev.
- `wall.rs`, `proxy.rs`: VM setup, network restrictions, and boundary probes.
- `said.rs`, `log.rs`, `ship.rs`, `door.rs`: signed statements, the agent's
  outbox, and messages from the parent.
- `operator.rs`: the cell tree, SSH gateway, and configuration review/apply API.
- `migration.rs`, `legacy.rs`: conversation import and signed-log compatibility.
- `session.rs`, `request.rs`: durable session identity, scoped commands, and retry receipts.
- `chat.rs`, `chat/`, `pi.rs`: cell session service, settings, and Pi RPC bridge.
- `web/`: the React chat frontend and bridge integration tests.
- `cell/tests/`: CLI and socket integration tests.

See [cell/README.md](cell/README.md) for the command reference and boundary
design.

## Migration and scope

Setup saves the previous binary, declaration, service unit, and named-session
map under `/var/lib/cell/migrations/pi-v1/`. Signed logs and Claude transcript
files stay untouched. Imported conversations are archived; Claude tool exchanges
are represented as text in their Pi history. The inbox continues through the
same cell-scoped Pi service, and run state is shipped to the signed log.

The UI uses the local tree you select. It does not invent projects or automatically
connect external accounts. Add cells in the UI or through declarations; enter each
connection's MCP URL/token in its settings. Provider credentials belong to the
selected cell.

## Environment autonomy

`cell setup` now migrates an existing cell into a persistent, user-namespaced
systemd-nspawn environment. The agent can use `sudo`, install packages, edit
`/etc`, and run localhost services. The VM host retains the authoritative
declaration, CONNECT proxy, firewall, signing key, inbox, and signed log. Guest
root maps to host UID 524288. Package downloads require explicit host grants
for the registries and release hosts a cell needs. Child declarations are not
automatically expanded.

Before migrating, setup refuses active runs and saves the previous runtime,
units, declaration, firewall and home archive under the root-only directory
`/var/lib/cell/migrations/environment-v1/`. The original `/home/agent` is
retained; the live home is inside `/var/lib/machines/cell-env`. Reapplying setup
preserves installed software and guest data. The guest chat service is
`cell-chat`; the host `cell-agent` service supervises the whole environment.
The operator's SSH relay still uses `/run/cell-chat/api.sock`.

See [the environment bench](bench/environment/README.md) for architecture and
production acceptance tests. The shared kernel and nspawn runtime remain part
of the trusted host boundary.
