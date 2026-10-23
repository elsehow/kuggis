# Kuggis chat

React + assistant-ui talks to the Rust `cell chat` server through the
browser-safe `@assistant-ui/react-pi` HTTP/SSE client. Rust launches Pi in
RPC mode. There is no Node server or Assistant Cloud dependency at runtime.
Kuggis supplies a short workspace system prompt, while Pi still loads workspace
context files and supplies its tool schemas.
Resumed Pi 1.3 sessions can retain the old prompt in their internal system
records. Before opening them, the bridge backs up the original JSONL and updates
only identifiable stock Pi instruction sections. User/assistant records, entry
IDs, branches, workspace instructions, and tool definitions are preserved.

Serde structs define operator responses, chat snapshots, session summaries, and
model readiness. The operator rejects malformed session lists before restarting
a cell or creating its first chat. `cell/src/pi/protocol.rs` defines outgoing
commands and the runtime/model responses the bridge consumes; host-UI replies
must match their pending request. Provider-specific message bodies and remaining
Pi event payloads still pass through as JSON.
Operator and Kuggis-owned settings contracts are generated with ts-rs from the
Rust Serde types into `src/generated/`. `npm run build` checks generated-file
drift; run `npm run generate-contracts --prefix web` after changing a contract.
The upstream `@assistant-ui/react-pi` package supplies its own Pi client types.

## Run

```sh
npm ci --prefix web
npm run build --prefix web
mkdir -p .kuggis/tree
cargo run --manifest-path cell/Cargo.toml -- ui .kuggis/tree \
  --bin cell/target/x86_64-unknown-linux-musl/release/cell
```

Open http://127.0.0.1:8080. The tree comes from declarations and is shown in
the first sidebar pane; the second lists the selected cell's chats, with its
settings gear. There is no separate cell index: a missing or stale selection
opens the first available cell. The gateway uses
SSH and private Unix sockets; Pi runs persistently on the cell VM. `--bin`
enables configuration review/apply; omit it for chatting without deployment.
Use `--listen`, `--state`, and `--assets` to override the local UI defaults.
Keep the live tree, identity pins, and operator state in ignored local storage
or outside the source checkout. The public tree examples are synthetic.

For local development, `cell chat --local WORKSPACE` runs Pi with your user
permissions. `--pi PATH` selects a fixture or alternate local Pi executable.
Local mode has a visible Local marker and does not claim VM isolation.
`cell ui TREE --socket-dir DIR` connects directly to per-cell Unix sockets
for integration tests or local development; it is also marked Local.

Vite development mode (`npm run dev --prefix web`) proxies `/api` to port 8080.

## Create a cell

Use **＋ Cell** in the tree, or **＋** beside a parent. Choose a name,
repository access, and providers, then review and create. Subcells receive a
parent-prefixed name; the Rust tree validates that their grants fit within the
parent's. Connections and provider sign-in use the new cell's settings after
its first chat opens. Credentials are never inherited from the parent.

Creation uses the same plan/apply reconciler as the CLI. It provisions the new
VM, repository integrations and runtime, connects the parent's door, and opens
one saved chat. It refuses existing VM names and unrelated pending parent
changes. Existing cells are not restarted to add a child.

The operator stores approved creation plans and typed progress in its state
directory. Closing or reloading the page leaves setup running. If the operator
stops, setup is shown as interrupted on restart; **Retry setup** observes what
already exists and continues within the original approval. It never overwrites
a changed declaration or recreates a completed VM/chat. A ready cell is added
to the gateway's routing table immediately, without restarting the UI server.

## Cell settings

Providers use Pi's own sign-in infrastructure in an isolated cell-local
process. The shared UI relays Pi's choices: browser or copy-code for Anthropic,
browser or device-code for OpenAI. For browser login on a remote cell, paste
the final redirect URL into Pi's prompt. API keys also go through Pi's prompts and credential
writer. Typed progress, prompts and cancellation are the only HTTP auth
messages; credentials stay in the selected cell. Changes require idle sessions.
Closing settings cancels an unfinished login; Disconnect uses Pi's logout.
Saved credentials, account verification, and the latest model request are shown
separately. Check asks Pi to resolve credentials and verifies the Claude account;
it makes no model request. Account observations are dated and are invalidated
when the saved credential changes.

If a cell has no available models, **Choose model** explains the missing setup
and opens provider settings. Enable and sign in to a provider, close settings,
then select a model. Catalog failures offer a retry without losing the draft.

Paper Instruments uses the same API-key form and Pi credential store. Granting
it seeds `ultramarine-1-flash` in Pi's `models.json`, preserving existing custom
configuration. Its grant allows only `api.paperinstruments.com`. The adapter
uses Pi's OpenAI-compatible streaming, tools, and reasoning support.

Git repositories declare read/write access and an optional fork. BigBrain and
Portal accept HTTPS MCP URLs and optional bearer tokens. Saving adds required
host grants to the proposed configuration; a child still cannot exceed its
parent. A preview is bound to the tree, binary, and action hash. Apply checks
for active runs and drift before deployment. If apply fails, the declaration
retains the desired configuration so the operator can inspect and retry it.

Connections live in the cell's `mcp.json`; Pi loads them when sessions reopen.
Existing tokens are retained only when the endpoint URL is unchanged. Blank
URLs remove managed connections; unrelated Pi MCP entries are preserved.
The API uses explicit `credential` operations (`keep`, `clear`, or `replace`),
and responses return credential references without secret values.

## Sessions and transport

Each chat presents a typed `Session`, owned by its cell (`local` in explicit
local workspace mode). It has a random, stable ID passed to Pi with `--session-id` and a
dedicated `--session-dir`. Rust owns process lifetimes; closing a browser
does not stop Pi. SIGINT/SIGTERM clears pending prompts, aborts active work,
and stops the children. Server restarts preserve the transcript but do not
automatically resume unfinished work or recover the in-memory prompt queue.

Pi's JSONL files are authoritative for conversation history, model
configuration, and compaction. On reopening, Rust follows the active
branch of `get_entries`, preserving messages before compaction. Kuggis's
`threads/*.json` files store only `id`, `cell`, `title`, and `archived`.
Live status and queues are reconstructed, not persisted there. Prototype
metadata is migrated on startup with a `.json.bak` copy. A state
directory is bound to one canonical workspace and locked against a second
server. Back up the whole directory; do not edit its Pi files while running.

The bridge implements the adapter's thread, prompt, cancel, queue, model,
thinking, archive, and extension-dialog routes. Permanent deletion and
branch navigation are not exposed. SSE begins with an authoritative
snapshot, even when `snapshot=false` is requested, to close the race between
fetching a thread and subscribing. Sequence numbers deduplicate events.
Reconnects and slow subscribers recover through snapshots; this is not a
durable event-replay API. Pi's delta-only RPC records are reconstructed into
the cumulative messages expected by react-pi.

`POST /api/sessions/:id/commands` accepts the typed command envelope, for example:

```json
{
  "request": "00000000000000000000000000000001",
  "session": { "cell": "local", "id": "00000000000000000000000000000002" },
  "action": { "prompt": { "content": "Inspect the code" } }
}
```

`"action": "cancel"` uses the same envelope. Session and request IDs are
32 lowercase hex characters. The assistant-ui message and cancel routes
translate into these commands, using `X-Request-ID` if supplied; the
frontend generates this header. A retry must reuse its original request ID.
The `requests/` journal survives restarts: an accepted retry returns 204,
changed content under the same ID returns 409, and an interrupted dispatch
returns 409 with an unknown outcome. Inspect the transcript before choosing
to submit new work. Other adapter actions do not use this retry journal.

The UI renders Markdown, tool calls and results, model selection, steering,
stop, pending queue, and blocking extension dialogs. Extension notifications,
custom terminal widgets, and fork controls are not surfaced. Image attachments
are supported: paste or drop images into the composer; they are sent to Pi as
base64 image content. Guards are real, not arbitrary: 5 MiB per image (the
Anthropic image-block hard limit) and ~16 MiB per request body (the HTTP
ingress and Pi RPC frame caps in `cell/src/chat.rs` and `cell/src/pi.rs`).
Archived chats remain on disk and can be restored. Opened sessions currently
keep their Pi process until the server stops. An idle browser does not follow
runs started by a separate client until refreshed (a react-pi limitation).

The server is for a single local operator: loopback binding, same-origin
requests, no CORS, and a fixed workspace. It is not an authenticated remote
service. Pi itself handles credentials and reports provider errors; the
model list is not a credentials test. In cell mode the gateway delegates to
the VM daemon, which enforces ownership and grants and reports run state
through the signed outbox. No credentials are copied to the laptop.

## Verify without model calls

```sh
cargo build --manifest-path cell/Cargo.toml --locked
cargo test --manifest-path cell/Cargo.toml --locked
npm run build --prefix web
npm test --prefix web
KUGGIS_TEST_PI="$(command -v pi)" npm test --prefix web
```

The integration test uses the installed `createPiHttpClient` and
`PiThreadController` against the actual Rust server and a deterministic fake
Pi executable. A second suite runs two isolated cell daemons through the
operator gateway, including shutdown with active SSE and persistent remote
work. It checks streaming and tools, reconnect without duplication,
steering, cancel, extension dialogs, model changes, archive/restore, process
failure, origin/Host checks, and persistence across a server restart.
It also checks cell ownership, request replay, cancellation replay,
uncertain outcomes, and migration of prototype metadata.
The installed-Pi auth tests exercise real login prompts without submitting
OAuth codes. The Paper Instruments test sends a prompt only to a local mock
HTTP server, checking streaming reasoning and a tool round trip; it makes no
paid provider requests.

A browser preview with simulated responses:

```sh
cargo run --manifest-path cell/Cargo.toml -- chat --local . \
  --state /tmp/kuggis-chat-demo --pi "$PWD/web/tests/fake-pi.mjs"
```

The fake runtime recognizes `slow`, `[confirm]`, and `[crash]` in prompts
for exercising streaming, a dialog, or a subprocess failure. Use a separate
state directory from real Pi sessions.

The browser model-selection regression uses the built frontend and fake Pi.
With Playwright and Chromium already installed, run:

```sh
KUGGIS_TEST_PLAYWRIGHT=/path/to/playwright/index.mjs \
  node --test web/tests/model-selection.test.mjs
```

Set `KUGGIS_TEST_BROWSER` to a Chromium executable if it is outside Playwright's
default installation. The test delays the model write to reproduce the
new-chat race, then checks the header, draft, first response, reload, and
failed model changes. It makes no provider requests.
It also covers an empty catalog, opening provider settings, retrying a failed
catalog request, and sending the first message without a default model.
