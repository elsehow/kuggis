# Kuggis v2

Flat, persistent workspaces with deployment-configured authority. Use an agent's
own TUI over SSH. No nested cells, runtime approvals, chat gateway, provider
login UI, or agent conversation protocol.

The [implementation plan](../docs/v2-plan.md) records the scope. The
[testing guide](TESTING.md) covers local checks and the disposable exe trial.

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
and native Pi sessions to a fresh v2 host with a verified manifest. Migration
stays outside the Rust runtime; the installer refuses hosts containing a v1 cell.
On exe images it disables the bundled Shelley socket/service when present;
agent workloads belong inside the guest, and the host remains trusted.

## Create an exe workspace

The initial wizard installs Pi, grants access to one GitHub repository and uses
exe's existing managed model integration. It handles the VM, SSH trust, complete
configuration, agent files, repository clone and model/tool checks in one run:

```sh
v2/target/debug/kuggis create bigbrain
v2/target/debug/kuggis connect bigbrain
```

The wizard asks for the repository and read/write scope; read is the default.
This first recipe uses exe, Pi and managed models. It creates `kuggis-bigbrain`
as the provider VM name, leaving any VM named `bigbrain` alone. Defaults are
2 CPUs, 8 GiB VM memory and a 6 GiB guest limit. It configures Pi's initial model
as `exe/gpt-6.1-sol` without forcing a model on every launch or overwriting the
user's subsequent selection.

For an agent or script, supply the same inputs without a wizard:

```sh
kuggis create bigbrain --repo elsehow/BigBrain --github-access write --json
```

`--file setup.json` accepts the same fields (`name`, `repo`, `github_access`,
`host`, `agent`, `model`, `cpus`, `memory_gib`, `version`). `--plan` validates and
prints the resolved request/declaration without provisioning anything. `--json`
and non-TTY input never prompt. Progress goes to stderr; stdout contains one
structured result, including the failed step and receipt on an error. Missing
fields and unsupported options fail validation. Runtime approval prompts are
not an option.

Prerequisites: Python 3.11+, OpenSSH, curl, working batch SSH to `exe.dev`, its
trusted host key, GitHub account/repository linking in exe, and the managed `llm`
integration. The wizard reuses an exact repository/access integration or creates
one; it does not change an integration used elsewhere. It rejects additional
gateway integrations that would broaden the requested authority. Grant and
model checks run before reporting readiness; Git read access and configured
write scope are reported separately, without pushing to the repository.
Public repositories may also be readable through the provider gateway; the
repository grant describes authenticated authority, not a content filter for
public GitHub data.

The generated SSH configuration uses `HostKeyAlias exe.dev` for the new VM,
reusing the operator's trusted provider identity with strict checking. See
[exe's host identity](https://hub.exe.dev/docs/faq/host-key). It includes the
operator's normal SSH config, or the standalone file passed via `--ssh-config`.
No private key or management credential enters the guest.

State is ordinary private files in `.kuggis/workspaces/NAME/`: `setup.json`, the
generated `workspace.json`, `ssh-config`, and `receipt.json`. Set
`KUGGIS_STATE_DIR` to use another directory. Name-based connect/shell/check/exec/
put/get/web resolve there; path-based commands remain supported. Rerun the same
create command to resume an interrupted attempt. Retries check the host identity
and preserve existing project files; conflicting names or changed requests fail
without overwriting them. An ambiguous create that cannot be reconciled does
not automatically issue another create.

Development builds resolve the matching helper in
`v2/target/x86_64-unknown-linux-musl/release/kuggis`; build it once with
`sh v2/scripts/build-linux.sh`. `--bin` remains an optional override. To distribute
a ready pair, run `sh v2/scripts/package.sh OUTPUT_DIRECTORY` and keep `kuggis`
and `kuggis-linux-x86_64` together. Recipes and their checksums are embedded in
the client. The wizard detects a missing helper before allocating a VM.

Other agents, native provider login profiles, a deferred-login mode, editable
setup forms and custom preview-port options remain in the
[onboarding roadmap](../docs/v2-onboarding.md). The initial recipe installs base
workspace utilities and Pi; project-specific language toolchains, dependency
endpoints and deployment resources remain environment configuration.

## Commands

From the repository root, after building:

```sh
cargo build --manifest-path v2/Cargo.toml --locked
v2/target/debug/kuggis validate v2/examples/workspace.json
v2/target/debug/kuggis render v2/examples/workspace.json
v2/target/debug/kuggis bundle v2/examples/workspace.json
```

`render` describes the environment. `bundle` prints the exact generated files as
JSON. Both are pure descriptions: neither says a live boundary passed checks.

On a dedicated Ubuntu 24.04 host with noninteractive SSH/sudo configured:

```sh
kuggis setup workspace.json --bin path/to/linux/kuggis
kuggis connect workspace.json
kuggis shell workspace.json
kuggis check workspace.json
kuggis exec workspace.json -- git status --short
kuggis put workspace.json ./design.png inputs/design.png
kuggis get workspace.json output/report.pdf ./report.pdf
```

`connect` starts or attaches to a guest tmux server owned by a separate guest
service. Detaching does not kill that server. The service is not enabled at boot;
restart never automatically replays the launch command. A dead pane remains
available until explicitly replaced. `shell` opens a separate guest shell.

For Ghostty, `connect` and `shell` use `xterm-256color` compatibility because
Ubuntu 24.04's base terminfo does not include `xterm-ghostty`. This keeps normal
terminal interaction and 256 colors; Ghostty-specific terminal capabilities need
its terminfo installed separately. See [Ghostty's terminfo guidance](https://ghostty.org/docs/help/terminfo).

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
- `private_web`: explicit guest/host port pairs; host relays bind loopback only.
- `ingress`: defaults to `{"kind":"tunnel"}`. For exe, use
  `{"kind":"exe","vm":"example-vm","web":"preview"}`, where `preview`
  names a declared web mapping and `ssh` is `example-vm.exe.xyz`.

An exe setup first runs the provider's private-visibility and port-selection
commands from the operator machine, then installs the guest/relay. Exe account
credentials never enter the guest. VM creation remains outside this installer.
A tunnel configuration supplies loopback relays. From your own computer, run:

```sh
kuggis web my-workspace
# Existing path-based configurations work too:
kuggis web .kuggis/merrillfront-v2.json
# Select a mapping and a different local port; useful for agents/headless clients:
kuggis web my-workspace preview --port 18080 --no-open
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
boundary addresses and dials the checked IP without resolving again. HTTP clients
use HTTPS CONNECT; outbound SSH uses a guest OpenSSH ProxyCommand through the same
policy. Direct guest networking is blocked by the host firewall.

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
