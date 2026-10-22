# cell

The Rust implementation of kuggis. A cell is a VM root, a set of grants
and a key. This crate holds the CLI, provisioning, the agent runner, and
the boundary services: the network wall and proxy, the door, and the
signed statement log. See the [repository README](../README.md) to build
and run it.

The core types separate authority, durable sessions, and runtime state.

| kind | types |
|---|---|
| parsed primitives | `Name`, `Host`, `Repo`, `Url`, `Commit`, `Hash`, `SessionId`, `RequestId` |
| the cell | `Provider`, `Grant`, `Cell`, `Tree` |
| conversations | `Session`, `SessionRef`, `SessionCommand`, `SessionAction`, `Prompt` |
| the plan | `Action`, `Plan`, `Approved` |
| what a cell says | `State`, `Said`, `Statement` |

Each primitive has one constructor, its parser, and deserializing goes
through it, so a bad value fails to load rather than loading wrong.
`Tree::new` is the only way to get a tree and refuses every forbidden
shape. `Statement::open` is the only way to read what a cell said, and it
verifies first. `Approved::new` is the only way to get something apply
will run, and it checks the hash the operator repeated back.

The same function derives a cell's allowlist on the laptop, to print the
plan, and on the box, to configure the proxy.

## The binary

    cell key new PATH               mint a secret (0600); print its public key
    cell key pub PATH               print the public key of a secret
    cell say CELL SEQ SECRET        sign the Said on stdin
    cell open KEY                   verify the Statement on stdin; print what it said
    cell allow CELLFILE             the hosts a cell's proxy may tunnel to
    cell tree DIR                   check a directory as one tree
    cell plan DIR [NAME] [--bin LINUX_CELL] [--have OBSERVED [--box HASH]]
    cell apply DIR [NAME] --approve HASH [--bin LINUX_CELL] [--pins DIR]
    cell adopt VM NAME              tag an existing VM as NAME's root
    cell observe [--from DIR]       read exe.dev's listings back as cells and doors
    cell proxy --listen ADDR --cell CELLFILE [--private-ok]
    cell setup CELLFILE
    cell check
    cell probe
    cell door --listen ADDR --cell CELLFILE --said DIR --inbox DIR
    cell ship --cell CELLFILE --secret FILE --outbox DIR --said DIR [--once]
    cell ui TREE [--bin LINUX_CELL] [--listen ADDR]
    cell knock --inbox DIR
    cell session new
    cell brief DIR NAME SESSION_ID TEXT [--request REQUEST_ID]
    cell pull DIR NAME [--since N]
    cell login DIR NAME
    cell chat --local DIR [--listen 127.0.0.1:8080] [--state DIR] [--assets DIR] [--pi PATH]

`chat` serves the React frontend and persistent Pi RPC sessions on the
operator's machine. It requires Pi 1.2+ and built assets at `web/dist`
(or `--assets`). It binds only to loopback and validates browser origins.
This local mode runs with the operator's permissions; it does not attest
a cell boundary or write the signed statement log. See
[the chat README](../web/README.md) for the transport and test details.

A cell file:

```json
{"name":"widget","grants":[
  {"provider":"anthropic"},
  {"code":{"repo":"example/widget","write":true}},
  {"code":{"repo":"upstream/widget","write":false}},
  {"host":"pypi.org"}
]}
```

`Provider` describes authority, independently of the executable. Pi is a
runtime, not a grant. Provider grants allow their API and sign-in hosts.
OpenAI uses `api.openai.com`, `auth.openai.com`, and `chatgpt.com`; no exe.dev
LLM integration is required. Paper Instruments allows `api.paperinstruments.com`.
VM setup installs a pinned Pi runtime independently
of provider grants.

The edge is one GitHub App for every cell, so GitHub cannot tell a parent
from a child. What tells them apart is which repository each carries. A
child's grant `{"code":{"repo":"example/kuggis","write":true,"via":"cells/kuggis"}}`
is authority on the upstream carried by a fork under an org you own: the
edge holds the fork, the child pushes there and reports a branch upward.
Its parent carries the upstream, opens the pull request and merges it.
Merging is writing the upstream, and nothing else enforces it. A parent
writing the upstream covers a child writing it through any fork, and a
codebase counts once toward the one-writable-repository rule, so the
parent may carry the fork too, to sync it and fetch the child's branches.
Clones land in `work/<owner>/<repo>`.

A cell does not know where it sits. The directory is the tree:
`.kuggis/tree/coordinator.json` is held by the human, `.kuggis/tree/coordinator/widget.json`
sits under coordinator, and re-parenting is `mv`. The box has no use for
the edge, a name is not a capability, and the hash must mean "this box's
configuration" and nothing else, so the edge lives in `Tree` alone.

The tree is applied as doors. A parent reaches a child through an exe.dev
peer integration named `door-<child>`, attached to the parent's VM alone,
so the gateway stamps every knock with the caller and serves the door to
nobody else. The plan opens and closes doors; observe reads them back; the
proxy lets any door hostname through to the gateway, which refuses the
ones not attached. Through a door the parent sends a `Said`, a brief or a
reply, trusted because only the parent can knock. Everything upward is a
signed `Statement`.

## Apply

`cell plan DIR` plans every cell in the tree against exe.dev and the
boxes, prints the plans and one hash; `cell apply DIR --approve HASH`
runs them. Four rules make it safe.

Apply is a loop, not a script. Before every action it observes again and
re-plans, and the fresh plan must sit inside the approval: the same
declaration, a subset of the actions. A done action vanishes, a failed one
is retried by running apply again, success is proven by observing rather
than by trusting the edge's reply, and any drift beyond the approval stops
the loop. Cells go leaves first, so a child's VM exists before its
parent's door is opened.

Observe reports reach, not configuration. A VM is a cell only by its
`cell-<name>` tag, so an existing VM is brought in by `cell adopt`, on
purpose. Every integration that reaches the VM counts, by tag or by name,
and a GitHub integration counts once per repository. The converse holds
too: apply attaches only an integration that is exactly one grant, and
makes one, `gh-<owner>-<repo>` or `-ro`, when a repository has none. An
integration that carries more comes off whole, and the rest waits for a
new approval. One by a tag other VMs share is refused, not detached.
exe.dev's default `llm` is attached to every VM and can only be detached
from all of them, so it is the one grant the edge cannot take away; the
wall check proves the proxy refuses it.

The box's word is `Checked`. Setup ends with a check the box signs: the
cell hash it runs, the sha256 of the binary that checked it, and which
providers have a credential on the box. The laptop pulls the log over
ssh, pins the box's key on first contact in `DIR/.pins`, verifies every
statement and takes the last passing check as what the box runs. A key
that changes is refused until the pin is removed by hand.

Apply never destroys. There is no destroy action, no `rm`, no retagging.
A cell file removed from the tree leaves its VM alone.

Setup ships a Linux build of this binary: `cargo zigbuild --release
--target x86_64-unknown-linux-musl`, with rustup's toolchain first on
`PATH`, gives a static one in twenty seconds. Give the same `--bin` to
`cell plan` and `cell apply`: a box whose last check names another
binary needs setup, and the approval binds the hash of the one shipped.

## The wall

The mutable environment is a persistent Ubuntu systemd-nspawn container with
private user, mount, PID and network namespaces. `BoundarySpec` describes the
host-controlled mapping and network interface, separately from `Cell` grants
and mutable guest state. Guest root maps to host UID 524288, never host root.
The agent has passwordless sudo and can modify its userspace, install packages
and run local services.

Host nftables input/forward rules restrict all traffic arriving on the veth,
independent of guest UID, routes, firewall changes or proxy variables. The only
exit is the host CONNECT proxy. Its allowlist comes from the host declaration;
changing a guest copy cannot enlarge it. The signing key is never mounted.
The inbox, signed log and declaration are read-only mounts. Only the outbox
and a dedicated chat socket directory are writable host mounts.

`cell check` observes namespace mappings and the complete live firewall from
the host, runs trusted network probes in the guest network namespace, checks
chat availability and guest sudo, and signs its report. An external startup
guard refuses to boot the environment unless the firewall matches the stored
host-approved rules. Reapplying setup updates managed runtime files inside
the guest's namespaces, never by executing or traversing guest paths as host
root. General kernel-escape resistance is not established by these probes.
`cell probe` retains the legacy host-user diagnostics; use host-side
`cell check` for container cells.

## The door

`cell door` listens on loopback where exe.dev's in-VM auth proxy delivers
the VM's web traffic. A parent reaches it at `door-<child>.int.exe.xyz`
through a peer integration attached to the parent's VM alone; the
platform sets `X-Exedev-Source-Vm` after stripping whatever the caller
sent, and the gateway answers everyone else with "not attached".
`GET /said?since=N` serves the signed log; `POST /say` files one brief
for this cell, or one reply, in the inbox the agent reads. The agent
cannot knock on its own door from inside: the wall allows it only the
proxy's port. Verified live between two VMs, spoofed header included.

## The voice

The agent cannot sign, by design, so it drops a `Said` in
`/var/lib/cell/outbox`, as `<name>.tmp` renamed to `<name>.json`, and
`cell ship` signs it as the cell and appends it to the log. The shipper
runs as `cell-say`, which owns the key and the log and is the only user
besides root that can read the key; root's wall check appends to the same
log under the same lock, so no two statements take one number. The outbox
is the agent's and the shipper's alone to write.

Everything in the outbox is the adversary's. A file is read without
following links, capped at 64 KiB, parsed strictly, and checked against
the cell: only the wall check says `Checked`, a session reference names
this cell as owner, a verdict names a repository the cell holds. What passes is
renamed to `<name>.<seq>`, the receipt, and the agent reads its own
statement back from the world-readable log. What fails is replaced by
`<name>.refused` holding the reason. The agent gets no other feedback and
needs none. The wall check sends a session statement through the outbox
and a forged `Checked`, and expects a receipt for the one and a refusal
for the other.

## A session

A `Session` has an opaque `SessionId`, an owning cell, a title, and an
archive flag. Renaming, archiving, changing models, and restarting a
runtime leave its identity intact. A chat is the UI for a session.
Live run status, provider/model selection, queues, and process handles
belong to the adapter and are not durable session metadata.

Commands carry a `RequestId` and a `SessionRef { cell, id }`. The receiver
checks the cell and session before dispatch. Prompt and cancel commands
have durable receipts: an accepted retry is a no-op; reuse of an ID for
different work is refused. A crash before acknowledgement leaves the
outcome unknown and requires inspection, rather than automatic resubmission.
This prevents duplicate dispatch; it does not promise exactly-once execution.
`Claim::Fresh` carries a single-use dispatch permit. Known pre-dispatch failures
and explicit Pi rejections retain their distinct outcomes across retries and
restarts. Model failures occur after acceptance and never cause redispatch.

`ExecutionMode` distinguishes a loopback local runtime from a cell runtime.
Cell execution requires its declaration, private socket, Pi directory, inbox,
and outbox together; the cell's identity comes from its declaration.

`cell-agent.service` supervises the environment on the VM host. Inside it,
`cell-chat.service` runs `cell chat` as `agent`, with passwordless sudo and a
private Unix socket shared at `/run/cell-chat/api.sock`. A default ACL grants
only the host operator's `cell-chat` group access to that socket. Guest
localhost is unrestricted. The host proxy, door and signer retain their own
unprivileged users and service hardening.

`cell ui TREE` serves the browser on loopback. SSH runs `cell relay`, carrying
HTTP on stdin/stdout to the private cell socket. Every cell route resolves its
name through the validated tree. The gateway does not own Pi processes or
transcripts, so restarting it leaves remote runs alive. The cell service
handles door/SSH briefs through the same session and request journal as chat.

`--exe-proxy FILE` selects authenticated hosted access on a dedicated trusted VM.
The file pins the HTTPS origin and permitted exe.dev user IDs. Local access remains
the default; both modes bind only to loopback. See the
[supervisor deployment guide](../deploy/README.md) for the trust boundary, systemd
service, state migration and acceptance checks.

`cell session new` generates an ID; `cell brief DIR NAME SESSION_ID TEXT
[--request REQUEST_ID]` files the brief. `cell login DIR NAME` opens Pi as
the agent for terminal provider sign-in. Settings runs an isolated Pi extension
that calls `ModelRuntime.login`/`logout` for all providers. Pi owns PKCE, device
codes, token exchanges, credential locking, storage, and refresh. The bridge
relays typed prompts/status only; it never runs a model or returns tokens.
OpenAI subscriptions use `openai-codex` with browser or device-code sign-in;
API keys use `openai`. Anthropic offers browser and copy-code sign-in. The shared
UI relays Pi's method selector and manual callback prompt for remote cells.
Paper Instruments uses a Pi-stored API key and the `ultramarine-1-flash` model
configured in `models.json`. `ProviderConnection.login_methods` tells the UI
which authentication methods a provider supports; the server enforces them.
Closing settings cancels an unfinished sign-in. Git scopes use the
existing plan/apply mechanism.
MCP connections use granted HTTPS hosts on ports 443 or 8443 and store tokens
only inside the selected cell.
`ProviderConnection` keeps credential state, Pi authentication resolution,
verified account observations, and inference results separate. Observations
include timestamps and are tied to the credential that produced them. A
provider's extra-usage error is not evidence of missing sign-in or exhausted
subscription allowance. Settings' Check action resolves credentials through Pi
and verifies Anthropic OAuth account identity without making a model request.

`ConnectionConfig` contains a validated `McpEndpoint` and a cell-local credential
reference. `ConnectionChanges` accepts unique names and explicit secret updates:
keep, clear, or replace. Tokens cannot be retained when the endpoint changes.

## Type migration

Declarations use `{"provider":"anthropic"}`, `{"provider":"openai"}`, or
`{"provider":"paperinstruments"}`.
Pi has no grant of its own. Session statements and briefs carry a scoped
session reference, and briefs require a request ID.

The read-only legacy decoder verifies old statements using their original
types and field order. It preserves statement hashes and signatures, while
exposing stable session references to the new code. Old log files are never
rewritten. Setup keeps the previous binary, declaration, unit, session map,
and inbox cursor in `/var/lib/cell/migrations/pi-v1/` (root-only).

The previous runner's conversations are imported into Pi as archived chats,
following the active Claude message branch. Tool calls/results become text;
original transcripts remain intact. Stable IDs are derived from the cell and
old session name, matching the log decoder. Import is idempotent. Existing
Anthropic credentials are adopted only within their original cell. Pi state
preparation and repository clones run as the unprivileged agent.

The local chat server also migrates its unsigned prototype metadata, preserving
IDs, titles, archive flags, and transcripts, with `.json.bak` metadata backups.
Local sessions keep the `local` owner; no local chat is reassigned to a VM cell.

## The proxy

Standard library only. It tunnels CONNECT to allowed hosts on 443 or 8443
and refuses everything else: other methods, malformed targets, address
literals, ports off the list, hosts off the list, oversized or slow
request heads. After resolving an allowed name it checks where the name
points: an exe.dev edge host must resolve to the platform gateway and
nowhere else, and every other host must resolve somewhere public. TLS runs
end to end, so the proxy sees hostnames and never content.

## Tests

`cargo test --manifest-path cell/Cargo.toml --locked`, from the repository
root, runs the unit tests, the proxy over real sockets with an
injected resolver, the apply loop against an in-memory exe.dev with
signing boxes, the shipper over a real outbox with a link and a
directory left in it, four writers racing for the log's lock, and the
binary as separate processes. The proxy tests
include a table of hostile request lines; one of them, a port written with
a leading zero, found a real hole while this was being written.
