# Kuggis

Flat, persistent workspaces with deployment-configured authority. Use an agent's
own TUI over SSH. No nested cells, runtime approvals, chat gateway, provider
login UI, or agent conversation protocol.

The [architecture guide](docs/architecture.md) records the scope. The
[testing guide](TESTING.md) covers local checks and the disposable exe trial.

The CLI has five groups: **cell, chat, access, file, admin**. Start with
`kuggis --help` or the [command guide](docs/cli.md). Repository and network grants
are applied with `kuggis access`; configuration changes report restart
requirements, and restarting an existing cell requires `--restart`.

Sign Codex into a cell using your laptop browser:

```sh
kuggis login fri --agent codex
kuggis login merrillfront --agent codex
```

Each cell shares one native ChatGPT login across its Codex chats and shells.
Kuggis handles the guest callback and provider network grants. Existing chats
keep their histories and model choices; exit and reopen running Codex processes
to adopt the login. See [agent login](docs/cli.md#agent-login) for status, plans,
and account switching. Device codes are not required.

## Build and install

The repository root is the Cargo workspace. Requires Rust 1.89 or newer;
workspace creation also uses Python 3.11+, OpenSSH, and curl.

```sh
cargo build --locked
cargo test --locked
cargo install --path crates/kuggis --locked
```

To package the local client with its matching Linux helper:

```sh
sh scripts/package.sh dist
```

Keep both executables together. Connect the [GitHub broker](docs/github.md)
before creating workspaces with repository grants.

The runtime lives in `crates/kuggis/`, with agent installation recipes in
`recipes/`, configuration examples in `examples/`, and integration tests in
`tests/`. The v1 runtime and browser UI have been removed. Existing configuration
version numbers, host service names, and paths retain their deployed values;
this repository reorganization does not require reinstalling running workspaces.

## Current status

The first implementation includes configuration validation, deterministic
plaintext deployment bundles, a Linux installer, a host-controlled CONNECT
proxy, persistent terminal entry, atomic file transfers, and private web relays.

Local Rust/Python tests exercise policy, real proxy sockets, transfer framing,
failure recovery, and the Rust client talking to the Python guest helper through
a test transport. A static x86-64 Linux binary builds from macOS.

**The first live exe trial passes** on a disposable Ubuntu 24.04 x86-64 host:
namespace/network checks, real SSH transfers, interactive terminal persistence,
reapply, full VM reboot, scoped resource changes, and authenticated private web
ingress. See the [test evidence and remaining scope](TESTING.md#current-evidence-and-remaining-gates).
The [Pi recipe](recipes/pi/README.md) launches its TUI and passed a real model/tool
round trip using the existing exe-managed LLM integration. The
[legacy duplication recipe](recipes/legacy/README.md) copies selected guest files
and native Pi sessions to a fresh Kuggis host with a verified manifest. Migration
stays outside the Rust runtime; the installer refuses hosts containing a v1 cell.
On exe images it disables the bundled Shelley socket/service when present;
agent workloads belong inside the guest, and the host remains trusted.

## Create an exe workspace

The wizard installs Pi and Codex, grants access to one GitHub repository and uses
exe's existing managed model integration. It handles the VM, SSH trust, complete
configuration, agent files, repository clone and model/tool checks in one run:

```sh
target/debug/kuggis cell create bigbrain
target/debug/kuggis chat open bigbrain main --agent codex
```

The wizard asks for the repository and read/write scope. Both agents use exe
managed models; select an agent on the first connection to each chat. It creates `kuggis-bigbrain`
as the provider VM name, leaving any VM named `bigbrain` alone. Defaults are
2 CPUs, 8 GiB VM memory and a 6 GiB guest limit. It configures Pi's initial model
as `exe/gpt-6.1-sol` without forcing a model on every launch or overwriting the
user's subsequent selection.

For an agent or script, supply the same inputs without a wizard:

```sh
kuggis cell create bigbrain --repo elsehow/BigBrain --github-access write --json
kuggis cell create another --repo owner/repo --json
kuggis chat open another main --agent codex
```

`--file setup.json` accepts the same fields (`name`, `repo`, `github_access`,
`host`, `default_agent`, `model`, `cpus`, `memory_gib`, `version`). `--plan` validates and
prints the resolved request/declaration without provisioning anything. `--json`
and non-TTY input never prompt. Progress goes to stderr; stdout contains one
structured result, including the failed step and receipt on an error. Missing
fields and unsupported options fail validation. Runtime approval prompts are
not an option.

Prerequisites: Python 3.11+, OpenSSH, curl, working batch SSH to `exe.dev`, its
trusted host key, a configured Kuggis GitHub broker, and the managed `llm`
integration. GitHub access uses the same broker on exe and other SSH hosts.
The wizard seeds one repository grant; `access grant/revoke/list` manage any number
of grants afterward, with independent permissions in each cell. See the
[GitHub setup and migration guide](docs/github.md). It rejects additional
exe gateway integrations that would broaden the requested authority. Grant and
model checks run before reporting readiness; Git read access and configured
write scope are reported separately, without pushing to the repository.

The generated SSH configuration uses `HostKeyAlias exe.dev` for the new VM,
reusing the operator's trusted provider identity with strict checking. See
[exe's host identity](https://hub.exe.dev/docs/faq/host-key). It includes the
operator's normal SSH config, or the standalone file passed via `--ssh-config`.
No private key or management credential enters the guest.

State is ordinary private files in `~/.config/kuggis/workspaces/NAME/`: `setup.json`,
the generated `workspace.json`, `ssh-config`, and `receipt.json`. An absolute
`XDG_CONFIG_HOME` changes the default to `$XDG_CONFIG_HOME/kuggis/workspaces`.
Name-based create/connect/shell/check/exec/put/get/web use this same location from
any working directory. Set `KUGGIS_STATE_DIR` to override the entire workspace
directory; relative overrides are resolved from the working directory. Empty
overrides are ignored. Path-based commands remain supported. Rerun the same
create command to resume an interrupted attempt. Retries check the host identity
and preserve existing project files; conflicting names or changed requests fail
without overwriting them. An ambiguous create that cannot be reconciled does
not automatically issue another create.

Earlier development builds stored state under the checkout's `.kuggis/workspaces/`.
Existing state is not moved automatically. To keep using it, set `KUGGIS_STATE_DIR`
to that directory's absolute path, or pass an explicit `workspace.json` path.
To adopt the new default, copy the workspace directories into the per-user location
without overwriting existing names. Update absolute local paths in `workspace.json`
(`ssh_config`) and the saved connection files if those files move, then check the
workspace before removing the old copy. This moves operator configuration only;
it does not move or recreate the remote workspace, files, credentials, or chats.

Development builds resolve the matching helper in
`target/x86_64-unknown-linux-musl/release/kuggis`; build it once with
`sh scripts/build-linux.sh`. `--bin` remains an optional override. To distribute
a ready pair, run `sh scripts/package.sh OUTPUT_DIRECTORY` and keep `kuggis`
and `kuggis-linux-x86_64` together. Recipes and their checksums are embedded in
the client. The wizard detects a missing helper before allocating a VM.

Claude Code, non-Codex provider login profiles, a deferred-login mode, editable
setup forms and custom preview-port options remain in the
[onboarding roadmap](docs/onboarding.md). The initial recipe installs base
workspace utilities and both agents; project-specific language toolchains, dependency
endpoints and deployment resources remain environment configuration.

## Commands

List chats, then explicitly select one or create a new name:

```sh
kuggis chat list my-workspace
kuggis chat open my-workspace research --agent codex
kuggis chat open my-workspace research
kuggis chat open my-workspace implementation --agent pi
```

`chat list` lists chats with their saved agent and running/stopped status,
then shows commands to open or create one. `chat list NAME --json` returns the
same typed inventory for scripts. Listing never starts an agent or creates a
chat. `--chat default` explicitly opens the original legacy terminal and history.
The first connection to another name starts a separate agent; using the same name
again attaches to that running agent. Detach with Ctrl-B then D. Each named chat
has its own persistent tmux service and, with the Pi recipe, its own history
directory. If the agent exits, reconnecting restarts it using that history.
After a guest reboot, chats start only when explicitly connected; prompts are
never replayed by Kuggis. Chat names use 1–48 lowercase letters, digits and
hyphens, starting with a letter and ending with a letter or digit.

When `agents` is configured, every chat saves its agent on first connection.
`--agent codex` selects an installed recipe for a new chat. Subsequent connects
remember it; omitting the flag never changes an existing chat's agent. Supplying
a different agent for an existing name fails with a migration explanation.
New chats without `--agent` use `default_agent`. Changing that default affects
only new chats. `--agent` requires `--chat`; there is no implicit default chat.

Archive a chat without deleting its binding or agent history:

```sh
kuggis chat archive my-workspace research
```

This stops its agent (disconnecting any attached terminals) and hides it from
`chat list`, including `--json`. Repeating the command is safe. Explicitly opening
`kuggis chat open my-workspace research` restores the same chat and history.
Other chats and workspace files are preserved. Updated host/guest helpers must
be installed with `admin upgrade CELL --bin LINUX_BINARY --restart` before using
this command on an existing cell that lacks archive support.

Bindings are ordinary versioned JSON in `~/.kuggis/chat-bindings/NAME.json` in
the guest. The Rust core validates agent IDs and recipe arguments; each recipe
owns native history and model/login configuration. Choose models inside the
agent's own UI. See the [Codex recipe](recipes/codex/README.md), including its
explicit Pi handoff procedure. The wizard installs both supported agents;
`--agent` on connect does not install software or grant new access. Explicit new
chats without `--agent` use Pi by default for compatibility; `create --default-agent codex`
sets a different fallback without adding a wizard question. The earlier create
flag/JSON field `agent` is retained as an alias for this default (and remains the
serialized setup-receipt field for compatibility).

Chats share the checkout, resources, credentials and servers. Model settings
are owned by the recipe (Codex keeps them per chat; Pi uses shared settings).
Use separate Git worktrees inside the cell for independent concurrent edits.
The left side of the tmux status bar shows the cell name.
In Pi, `/new` and `/resume` work within that named chat's history directory.
Changing a Pi session's display name does not rename its Kuggis chat.

The create wizard supplies the named-chat recipe automatically. Existing configs
need an optional `chat_launch` argv and an updated setup. For Pi:

```json
"chat_launch": [
  "/home/agent/.local/bin/pi", "--offline",
  "--session-dir", "/home/agent/.kuggis/chats/{chat}", "--continue"
]
```

The validated name replaces `{chat}` in arguments without shell evaluation.
Keep `launch` unchanged to preserve existing default history, and retain any
model flags needed by your environment in `chat_launch`. The recipe owns the
agent's history-selection flags; Kuggis sets `KUGGIS_CHAT` and manages terminal
lifetime. It does not translate agent conversation formats. A named request
without a recipe fails explicitly rather than attaching to the default chat.

From the repository root, after building:

```sh
cargo build --locked
target/debug/kuggis admin debug validate examples/workspace.json
target/debug/kuggis admin debug render examples/workspace.json
target/debug/kuggis admin debug bundle examples/workspace.json
```

`render` describes the environment. `bundle` prints the exact generated files as
JSON. Both are pure descriptions: neither says a live boundary passed checks.

For an initial installation on a dedicated Ubuntu 24.04 host with noninteractive
SSH/sudo configured (updating an existing cell additionally requires `--restart`):

```sh
kuggis setup workspace.json --bin path/to/linux/kuggis
kuggis chat open workspace.json default
kuggis cell shell workspace.json
kuggis cell status workspace.json --check
kuggis cell exec workspace.json -- git status --short
kuggis file put workspace.json ./design.png inputs/design.png
kuggis file get workspace.json output/report.pdf ./report.pdf
```

`chat open` starts or attaches to a guest tmux server owned by a separate guest
service. Detaching does not kill that server. The service is not enabled at boot;
restart never automatically replays the launch command. A dead pane remains
available until explicitly replaced. `shell` opens a separate guest shell.

For Ghostty, `connect` and `shell` use `xterm-256color` compatibility because
Ubuntu 24.04's base terminfo does not include `xterm-ghostty`. This keeps normal
terminal interaction and 256 colors; Ghostty-specific terminal capabilities need
its terminfo installed separately. See [Ghostty's terminfo guidance](https://ghostty.org/docs/help/terminfo).
Chat tmux servers enable extended keys and forward modified keys even without
application negotiation, so Shift–Enter can insert a newline in Codex from
terminals such as Ghostty that distinguish it from Enter.
After updating an already running tmux server's configuration, detach and
reconnect so the new terminal features are negotiated; the agent keeps running.

Uploads can omit the destination, insert a reference into a running chat, or
capture the local clipboard:

```sh
kuggis file put merrillfront ./design.png
kuggis file put merrillfront ./design.png --chat vt
kuggis file put merrillfront --chat vt --paste
# An explicit destination still works:
kuggis file put merrillfront ./design.png inputs/design.png --chat vt --replace
```

Without a destination, files land at the workspace root with their original
basename. Clipboard images and plain text become uniquely named files under
`.kuggis/uploads/` (PNG/JPEG or UTF-8 text). `--paste` uses the clipboard on the
computer running Kuggis: macOS uses AppKit without extra packages; Linux needs
`wl-paste` on Wayland or `xclip` on X11. Images take priority over text. An empty
or unsupported clipboard fails without uploading; local capture files are
removed after either success or failure. macOS captures are limited to 64 MiB.

`--chat` requires an already running chat with its composer open. After the
upload succeeds, Kuggis pastes the quoted absolute path at the cursor, using
bracketed paste. It never presses Enter. Codex recognizes image paths as
attachments; other files remain references that the agent can read. If the
chat disappears or restarts during transfer, the complete file stays uploaded
and the command reports that insertion failed. This needs an updated host and
guest helper as well as the updated local client.

Transfers operate relative to `/home/agent/workspace`, as the guest agent.
They stream a bounded length and SHA-256 checksum, then publish a complete file
atomically. Existing destinations fail unless `--replace` is explicit. There is
no confirmation prompt. Downloads also require a successful SSH exit before
publication. A lost upload acknowledgement can leave a complete file remotely;
retrying create-only will then report that it exists. Limits: one regular file,
16 GiB; directory/archive sync is not implemented.

`ssh-command CONFIG ...` remains available to inspect exact SSH process arguments.

## Declarative configuration

Use the [synthetic example](examples/workspace.json) for the schema, or the
[disposable shell fixture](examples/acceptance.json) for the first live test.

- `version`: exactly `2`.
- `name`: a flat workspace name. One dedicated host and one guest per workspace.
- `ssh`: SSH hostname or config alias.
- `ssh_config`: optional absolute path to a standalone **local** SSH configuration.
  Usernames, identities and pinned known hosts belong there. Authentication is
  batch-only; operator SSH-agent forwarding is disabled.
- `launch`: agent or shell argv, executed only inside the guest. It is never
  interpolated into a host shell or systemd unit.
- `memory_bytes`: positive integer limit for guest memory, defaulting to 4 GiB
  (`4294967296`). Setup renders this as the host service's `MemoryMax`. For
  example, `60129542144` permits 56 GiB when the deployment host has capacity.
- `packages`: optional Ubuntu package names to ensure are installed. Installations
  run inside the guest, including first setup, and need the appropriate network grants.
  Agent-installed packages are not removed by setup.
- `resources`: named bundles of exact endpoint pairs and credential sources.
- `egress`: defaults to `"restricted"` (only declared proxy endpoints). Set
  `"internet"` to allow all public destinations and ports: direct IPv4 traffic
  including DNS, TCP and UDP, plus arbitrary public HTTP/HTTPS through the proxy.
  Existing proxy environment variables can remain set. Private/link-local
  services still require exact pinned resource grants through the proxy, and
  host management access stays blocked. This does not expose inbound ports or
  change preview visibility. Direct IPv6 is not configured in the initial guest
  network; proxy connections can use IPv6 where the host supports it.
- `private_web`: explicit guest/host port pairs; host relays bind loopback only.
- `ingress`: defaults to `{"kind":"tunnel"}`. For exe, use
  `{"kind":"exe","vm":"example-vm","web":"preview"}`, where `preview`
  names a declared web mapping and `ssh` is `example-vm.exe.xyz`.

An exe setup first runs the provider's private-visibility and port-selection
commands from the operator machine, then installs the guest/relay. Exe account
credentials never enter the guest. VM creation remains outside this installer.
A tunnel configuration supplies loopback relays. From your own computer, run:

```sh
kuggis cell web my-workspace
# Existing path-based configurations work too:
kuggis cell web .kuggis/merrillfront-v2.json
# Select a mapping and a different local port; useful for agents/headless clients:
kuggis cell web my-workspace preview --port 18080 --no-open
```

The command opens a loopback-only SSH tunnel and your browser. Leave it running;
Ctrl-C closes the tunnel. The default local port is the mapping's host port. If
it is occupied, choose another with `--port`. With several mappings, supply the
preview name. `--plan` prints the selected route and SSH arguments as JSON without
connecting. The app must be running; the command establishes access, not an app
server. Browser-launch failure leaves the tunnel available at the printed URL.
SSH authentication and host-key failures fail without prompts.

New wizard workspaces include `preview`, guest port 3000 and host port 8000.
Setup writes `/etc/kuggis/web.json` and a managed block in
`/home/agent/AGENTS.md`, preserving other instructions and repository files.
Agents should inspect existing servers first, then serve on the declared guest
port at `0.0.0.0` or `10.205.0.2`. Loopback-only apps need an existing/configured
guest relay. Existing configurations retain their declared ports. A running agent
may need to reread `/home/agent/AGENTS.md` after setup updates its guidance.

Resources are concrete authority, not an approval policy. Public endpoint rules
exclude local, metadata and conservative special-use addresses; pins can allow
explicit private or IPv4 link-local platform destinations. These are TCP grants:
application authorization, including access through a shared gateway, belongs to
the destination. The live proxy also rejects host-owned and guest
boundary addresses and dials the checked IP without resolving again. HTTPS clients
use CONNECT; outbound SSH uses a guest OpenSSH ProxyCommand through the same
policy. Restricted mode blocks direct guest networking. Internet mode additionally
supports ordinary HTTP proxy requests and direct public IPv4 traffic.

Credential sources are protected root-owned files on the **deployment host**,
not implicit laptop credentials. Targets are relative to the guest agent's home.
Sources are read only by setup and installed with private permissions. Removed
managed credential files are removed on reapply, but an agent can copy credentials
it can read: actual revocation still belongs to the destination or credential
expiry. Configuration and management credentials stay outside the guest.

The basic example launches a shell. The [Pi deployment recipe](recipes/pi/README.md)
adds a pinned guest installation, declarative project trust and a TUI launch.
Provider authentication remains an explicitly configured resource; the core has
no agent enum.

## Implementation

| Area | Code |
|---|---|
| Validated configuration | `src/config.rs` |
| Pure endpoint/address decisions | `src/network.rs` |
| Pure bundle compiler | `src/deploy.rs`, `templates/` |
| Bounded CONNECT server | `src/proxy.rs` |
| SSH argument construction | `src/ssh.rs` |
| Client effects and atomic downloads | `src/client.rs`, `src/transfer.rs` |
| Linux effects and host checks | `runtime/host.py` |
| Guest-only setup and file operations | `runtime/guest.py` |

Paths above are relative to `crates/kuggis/`. Services and firewall rules are
ordinary plaintext templates. Rust validates inputs and compiles their concrete
values; Python performs Linux operations without maintaining another policy
engine. Host firewall comparison uses the Rust normalizer.

Setup is serialized, stops the old guest before changing its boundary, preserves
the initialized rootfs, and verifies the boundary before returning success.
Handled failures stop the guest and leave an incomplete-update marker. An
abruptly terminated installer leaves entry and subsequent boot blocked; an
already-started guest may continue behind the newly installed firewall until
stopped. It does not gain an approval/escalation path. Initial bootstrap failures
are retained for inspection rather than silently deleting a partial rootfs.

V1 remains in `../cell` and `../web`; there is no automatic migration or deletion
of old data. Live declarations, receipts and credentials belong in ignored
`.kuggis/` or outside the repository.
