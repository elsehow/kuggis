# V2 implementation and live acceptance

## Local validation

From the repository root:

```sh
cargo fmt --manifest-path v2/Cargo.toml --all -- --check
cargo clippy --manifest-path v2/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path v2/Cargo.toml --locked
python3 -m unittest discover -s v2/tests -p 'test_*.py' -v
sh v2/scripts/build-linux.sh
```

The Python tests use the built debug CLI and a fake SSH executable. They exercise
the real Rust/Python transfer protocol, including arbitrary binary bytes,
interrupted/corrupted streams, atomic replacement, nonzero SSH status after a
complete stream, and private ingress ordering. The fake transport never connects
to an external host or installs privileged files. Rust proxy tests use real local
sockets with injected DNS/dial effects, preserving production address policy.

The Linux build is `v2/target/x86_64-unknown-linux-musl/release/kuggis`. Use the
optional `aarch64-unknown-linux-musl` argument for an ARM host. Build the local CLI
and Linux binary from the same source; setup verifies that their compiled bundles
agree. No Zig, C cross-toolchain or web dependencies are needed for this crate.

## Disposable exe trial

Use a **new, dedicated Ubuntu 24.04 VM**. Setup installs host services and a
private nspawn guest. It refuses a v1 cell or a host registered to another v2
workspace. Do not test on an existing production or personal machine.

Authentication is a prerequisite, not an approval flow. The operator needs SSH
access to the VM and `sudo -n`; exe ingress additionally needs account-management
access to `exe.dev` on the operator side. Known hosts must already be pinned.
For a restricted development session, put a dedicated identity, known-hosts file
and standalone SSH config in ignored `.kuggis/`. Nothing reads personal SSH files
when `ssh_config` selects that standalone configuration and it explicitly sets
its identity and known-hosts paths.

Example SSH configuration (replace the absolute paths and VM name):

```sshconfig
Host exe.dev *.exe.xyz
    IdentityFile /absolute/path/.kuggis/test-identity
    UserKnownHostsFile /absolute/path/.kuggis/test-known-hosts
    GlobalKnownHostsFile /dev/null
    IdentitiesOnly yes
    BatchMode yes
    StrictHostKeyChecking yes
    ForwardAgent no
```

Establish the host key through a trusted source; do not disable host-key checking.
Create the disposable VM through the existing exe management interface, record
its identity, and verify its OS before installing. Its key never goes into the
guest. VM creation/deletion is not yet automated by v2.

Copy `v2/examples/acceptance.json` to `.kuggis/v2-acceptance.json`. Keep the
workspace name `v2-acceptance`, set `ssh` to the VM's `NAME.exe.xyz`, and add:

```json
{
  "ssh_config": "/absolute/path/.kuggis/test-ssh-config",
  "ingress": {"kind": "exe", "vm": "NAME", "web": "preview"}
}
```

These are fields to merge into the fixture, not a complete configuration. Exe
ingress requires the SSH hostname to match the named VM and the web mapping to
exist. The example carries no production/model credentials.

```sh
v2/target/debug/kuggis setup .kuggis/v2-acceptance.json \
  --bin v2/target/x86_64-unknown-linux-musl/release/kuggis
v2/target/debug/kuggis check .kuggis/v2-acceptance.json
v2/target/debug/kuggis connect .kuggis/v2-acceptance.json
```

In the terminal, create a file, start a long-running process, detach with tmux's
Ctrl-B D, then reconnect. The process should still exist. Test `put` and `get`
using a binary fixture and compare SHA-256 checksums. An interrupted transfer must
not publish a partial file; an existing destination must fail without a prompt.

The repeatable real-SSH transfer checks cover binary data, a quoted filename,
create-only behavior, explicit replacement, and an interrupted upload:

```sh
python3 v2/tests/live_client.py v2/target/debug/kuggis .kuggis/v2-acceptance.json
```

Run the host acceptance source **from the trusted checkout**, not a guest copy:

```sh
ssh -F .kuggis/test-ssh-config NAME.exe.xyz \
  'sudo -n /usr/bin/python3 -' < v2/tests/acceptance.py
```

It checks guest-root firewall tampering, namespace separation, forbidden egress,
a terminal process across independent connections, permitted HTTPS egress, the
loopback web relay, reapply/data preservation, and refusal to start after removing
the host firewall. It restores the firewall in a
`finally` block. It runs only when the declared name is `v2-acceptance`.

Repeat setup, verify the marker/data survives, then reboot the VM and check again.
Reboot should preserve files without automatically restarting the terminal's
launch command. Test configured HTTPS and SSH endpoints against disposable
destinations; a permission denied by the destination must never trigger a prompt
or broaden the network rules.

## Private browser page

Start a guest server on the configured guest interface/port:

```sh
python3 -m http.server 3000 --bind 0.0.0.0
```

Open `https://NAME.exe.xyz/` in an authenticated browser. Verify the fixture page
appears there. From an unauthenticated client, verify neither the main URL nor
the alternate port exposes the fixture bytes; an authentication challenge or
redirection to exe login is expected. Probe the VM's other reachable interfaces to confirm there is no
unauthenticated path to the loopback relay. The relay and provider authentication
must both pass; a successful local `curl` is not an ingress acceptance result.

Exe supplies the browser authentication and private proxy routing; v2 supplies
only the configured guest-to-host relay. See the [exe proxy docs](https://hub.exe.dev/docs/proxy).

## Current evidence and remaining gates

On 2026-10-26 (America/Vancouver), the first trial ran on the dedicated
`kuggis-v2-trial.exe.xyz` VM: Ubuntu 24.04, x86-64, systemd as PID 1. No existing
VM or production deployment was changed. Authentication used the operator's
SSH agent without forwarding it. The new VM's SSH key was compared with the
already trusted exe.dev key before pinning it in the workspace.

Observed passing checks:

- 33 Rust tests, 18 Python tests, formatting, Clippy, and a static Linux build.
- All six on-host acceptance cases: namespace/firewall boundary, permitted HTTPS,
  persistent terminal, loopback relay, refusal to boot without the firewall,
  and reapply preserving files.
- Real client binary transfer, quoted filename, create-only and replacement
  semantics, and an interrupted upload that did not publish a partial file.
- Interactive `connect`, detach, reconnect, and a background process surviving
  the disconnection. The automated PTY used `TERM=xterm-256color`.
  A follow-up tested `connect` and `shell` with caller `TERM=xterm-ghostty`:
  the CLI's compatibility mapping allowed attach/detach and a working shell
  reporting `TERM=xterm-256color`, without reinstalling the guest.
- Full VM reboot with a changed boot ID: files and the boundary survived, and
  the terminal launch command did not automatically restart.
- Private ingress through exe: authenticated HTTP 200 containing the fixture;
  unauthenticated HTTP 401 without the fixture, on both the main URL and :8000.
  This exercised the authenticated HTTP flow using an exe login cookie; visual
  browser rendering was not tested.
- Synthetic credential content, UID and private permissions; removing it from
  the declaration removed its managed guest file. A declared `github.com:22`
  CONNECT reached an SSH banner, while crossed host/port combinations were
  denied. Removing that endpoint denied subsequent connections. No production
  credential or deployment was used, and SSH application authorization was not
  tested.

The trial caught and fixed three integration problems: checking `machinectl`
before its package was installed, inherited host terminal input on noninteractive
guest setup commands, and a web-socket boot ordering cycle. The corrected build
was reapplied and tested through another full VM reboot.

Machine-readable receipts and the trial declaration are in ignored `.kuggis/`
(`v2-live-acceptance.json`, `v2-live-transfers.json`, `v2-reboot-acceptance.json`,
`v2-web-acceptance.json`, and `v2-resource-{present,removed}.json`).
The [Pi recipe](recipes/pi/README.md) has since been installed on this VM. The
trial declaration now launches Pi, and `.kuggis/v2-shell.json` retains the shell
declaration. The shell acceptance script refuses an agent launch to avoid
accidentally sending test commands as prompts. Pi version, repeatable install,
search tools, configured project trust and TUI startup were checked. The existing
exe-managed LLM integration then supplied authentication: Pi ran
`exe/gpt-6.1-sol`, called its write and bash tools without approval prompts, and
produced a verified fixture file. The explicit link-local address pin is covered
by tests for default denial, exact host/port matching, and mixed DNS rejection.
Receipts live in
`.kuggis/pi-recipe/`.

## First legacy duplicate

The [legacy recipe](recipes/legacy/README.md) has been exercised from a live
SSH-hosted v1 guest to a separate exe v2 guest. The source came from the
supervisor's current SSH binding, rather than an older retained exe VM.

- A declared filesystem selection exported 73,296 entries. Source membership
  and metadata stayed stable during export. Archive and restored manifest
  SHA-256 digests matched; the Git commit and complete dirty/untracked status
  matched the source. Unrelated home imports and model OAuth credentials were
  excluded.
- Three native Pi conversations and v1 thread records survived. Pi resumed a
  copied conversation, listed all three in `/resume`, and its native naming
  option preserved their display titles. A separate real model smoke test
  passed write/bash calls without altering those conversations.
- The copied Rust, Node and Python toolchains worked, repository access worked
  without a push, and the synthetic app configuration validated. The private
  demo returned HTTP 200 with authentication and 401 without it, on the default
  URL and port 8000. Its loopback-only application listener was retained behind
  a guest-side socket relay.
- The project's own check command passed its two bootstrap tests and then
  stopped at `cargo fmt --check`. The exact same formatting diff was reproduced
  on the original source. Migration did not reformat its pending work or claim
  a passing full project suite.
- Reapplying setup restarted the guest while retaining the workspace symlink,
  project files, renamed conversations and private loopback relay. The demo body
  remained identical, `get` read from the linked project root, interactive
  reconnect showed all three named sessions, and all four boundary checks passed.
- Local checks now include 33 Rust and 24 Python tests, including archive
  traversal, symlink ancestry, file corruption, source changes during export,
  and data/permission/timestamp preservation. Formatting, Clippy and the static
  Linux build passed.

This fresh install found another bootstrap issue: user-declared packages may
require Ubuntu universe, while debootstrap's default index covers main. Setup
now bootstraps only the base runtime and installs declared packages inside the
guest with its configured main+universe sources, just as reapply does. A
regression test covers that distinction. Bulk restore also reads inherited
stdin directly, avoiding a cross-namespace `/dev/stdin` reopen.

Private declarations, archives and receipts live under ignored
`.kuggis/v2-migration/`; no original cell was retired. This proves one selected
development-workspace copy, not migration of every legacy cell, production
workers or databases. ARM validation, independent provider credentials and
production destination authorization remain separate milestones. These checks
are not a general security audit.

## OVH runtime follow-up

On 2026-10-27, the same workspace was duplicated onto its original physical OVH
server, inside a dedicated Ubuntu 24.04 KVM VM. The VM is the ordinary v2 SSH
host; the original v1 guest remains running on the physical host. The existing
local workspace declaration now connects to OVH. The earlier exe duplicate's
runtime was stopped and disabled, and its repository integration detached;
its disk remains a recovery copy.

- The VM exposes 16 vCPUs and 59 GiB RAM. The validated `memory_bytes`
  declaration sets the workspace's systemd memory limit to 56 GiB, matching
  the source. These are limits, not reserved physical capacity: both copies
  share the server while the legacy workspace remains active.
- A fresh export restored all 73,296 selected entries with the same manifest
  digest. Git HEAD, pending work, three named native Pi conversations and
  toolchain checks matched. Interactive `/resume` showed all three names.
- Git uses the existing repository-scoped OVH broker through a private TLS
  endpoint. Reading the configured repository succeeded; another repository
  returned 403. No GitHub credential was copied into the guest.
- Model inference still uses the existing managed provider, reached through a
  dedicated restricted SSH resource tunnel. Agent processes and tools execute
  on OVH. Real Pi write/bash calls produced the expected fixture without
  approval prompts or copied OAuth refresh credentials.
- A private SSH local forward served the unchanged demo. A full VM poweroff
  and start changed its boot ID while preserving files, conversations, Git
  access, memory limits and the demo. The agent terminal did not automatically
  launch on boot. All four boundary checks passed after the restart.
- 34 Rust tests, 24 Python tests, formatting, Clippy and the static Linux build
  passed. The copied project's existing formatting failure was reproduced;
  its pending source work was not reformatted.

Provisioning and resource configuration remain deployment artifacts outside the
Rust core. Private declarations and verification receipts are under
`.kuggis/v2-migration/ovh/`. The existing Git broker and managed-model tunnel
remain dependencies to account for before retiring legacy infrastructure.

## Create wizard and unattended onboarding

On 2026-10-27, a fresh disposable exe workspace exercised `create --json` using
the existing read-only Kuggis repository integration and managed model service.
The command created the VM, established strict SSH trust via the operator's
trusted provider identity, installed the complete boundary and pinned Pi recipe,
cloned the repository, and returned ready after all four boundary checks and a
real model write/bash round trip. No handwritten deployment file, tarball
handling, host configuration copying or separate attachment command was needed.

The packaged client and adjacent Linux helper then resumed the same request
without a `--bin` or repeated `--ssh-config` argument. The VM's creation identity
stayed unchanged, a pending project file survived, and Git/model/boundary checks
passed again. Native Pi opened with the configured initial model under Ghostty
compatibility mode; detach/reconnect preserved the same Pi process. A read-only
Git receive-pack advertisement probe returned 403; no push was attempted.
Public repository reads through the provider gateway remain possible and are
not evidence of private-repository authorization.

37 Rust and 37 Python tests pass, along with formatting, Clippy and packaged
local/static-Linux builds. Tests include actual PTY wizard input matching flags
and JSON input, structured errors without prompts, exact integration selection,
lost creation replies, host-key failures, interrupted setup recovery, competing
setup locks, name/request conflicts, no duplicate creation after an ambiguous
result, guest execution argument isolation, preserved pending checkout edits,
and retained native model preferences. Provider failure cases use fake effects;
the real trial validates the successful and repeated setup paths.

The disposable VM and its attachment were removed after the trial. Receipts are
in ignored `.kuggis/onboarding-acceptance.json` and
`.kuggis/workspaces/onboarding-trial/`. BigBrain was not recreated. Additional
agents, native login profiles, private-preview onboarding, project-specific
toolchain profiles and ARM hosts have not been implemented by this slice.

## Private preview client

`kuggis web` was tested against the existing Merrillfront OVH workspace on
2026-10-27. Name lookup resolved its original config, the new loopback tunnel
returned HTTP 200, a competing client on the same port failed before printing a
URL, and Ctrl-C released the test port. The browser opener was not launched in
this headless live check (`--no-open`). No app or relay restart was needed.
Guest preview JSON and parent AGENTS.md guidance were installed and verified
readable as the agent; repository instructions were preserved.

40 Rust and 41 Python tests pass. New coverage includes typed preview selection,
ambiguous/missing mappings, invalid ports, an offline JSON plan, SSH failure
without browser launch, tunnel lifetime with caller stdin closed, cleanup when
the client exits, and preservation/update of existing agent instructions.
Formatting, Clippy and matching packaged local/Linux builds also pass. New
wizard workspaces now declare preview port 3000; this change was checked in the
configuration tests, without creating another disposable VM.

## Named chats

On 2026-10-27, Merrillfront's helpers and declarative chat recipe were updated
without restarting its default Pi process (PID 160 before and after). Two named
chats began with empty, separate history directories, each completed a bounded
model exchange, and neither contained the other's message. Detach/reconnect
kept the same Pi process. After one Pi exited, two simultaneous connect calls
resumed one replacement process and restored its original session file and
response. The other chat and default Pi retained their process IDs. Trial
services were stopped afterward; synthetic histories remain outside the default
history directory. No guest reboot was performed for this change.

43 Rust and 47 Python tests pass, plus formatting, Clippy and matching packaged
local/Linux builds. Coverage includes validated names/argv recipes, legacy
config round trips, default alias compatibility, missing-recipe errors,
separate guest service selection, and serialized dead-pane recovery. The Pi
recipe supplies history-directory isolation; other agents need their own
declarative chat recipe. Receipts are in ignored `.kuggis/chat-acceptance.json`
and `.kuggis/chat-upgrade-receipt.json`.

## Public internet egress

On 2026-10-27, Merrillfront was changed to `egress: "internet"`. Guest HTTP and
HTTPS requests through the proxy returned 200; direct HTTP returned 200, direct
HTTPS reached a 301 redirect, and direct IPv4 DNS resolved successfully. The
private repository's HEAD remained readable and a sessionless Pi request through
the existing model resource returned its expected answer. Policy (PID 160), VT
(PID 87055), and the guest leader remained running through the update. All four
live boundary checks passed, including rejection of management and ungranted
metadata access. Preview visibility was not changed.

The first live attempt exposed an output-hook ordering issue: reply filtering
at conntrack's own priority could reject the proxy's SYN-ACK after NAT was
registered. Output filtering now runs strictly after conntrack. Firewall batches
also replace the whole Kuggis table atomically, so switching back to restricted
mode removes the NAT chain. Applying internet → restricted → internet rules in
a disposable Linux network namespace verified this cleanup. The unsuccessful
attempts were rolled back; final installed state matches the declared policy.

47 Rust and 48 Python tests pass, plus formatting, Clippy and packaged local/Linux
builds. New coverage includes public target policy, preserved private pins,
HTTP header rewriting and credential stripping, response/connection completion,
legacy restricted defaults, and mode-specific forwarding. No guest reboot was
performed. Direct IPv6 is not configured; public IPv6 remains available through
the proxy only when supported by the host. Receipts are in ignored
`.kuggis/internet-upgrade-receipt.json` and `.kuggis/internet-acceptance.json`.

## Agent selection and chat discovery

On 2026-10-27, a fresh disposable exe host completed creation with **both Pi and
Codex** installed. Both agents passed a real tool/file round trip through the
managed gateway; Git read access and all boundary checks passed. Reapplying the
latest helper passed, and bare `connect --json` returned an empty chat list with
both available agents without starting either one. The trial uses the existing
read-only Git integration; no push was attempted. Its receipt is in ignored
`.kuggis/workspaces/agents-trial/receipt.json`.
The test VM and its repository attachment were removed after verification;
the reusable Git integration was retained.

Merrillfront's runtime was updated without restarting its guest or its active Pi
chats. Existing bindings stayed Pi when the new-chat default became Codex.
Codex 0.165.0 passed real tool/file checks with Sol 6.1, Terra 6.1, Astra 6, and
Sol 6. Its native `/model` picker displayed those four entries. A fresh chat
opened without folder-trust or action-approval questions. After that Codex
process exited, explicit reconnect resumed the same native session and recalled
the synthetic marker from its earlier exchange.
Synthetic chat histories were archived outside the active chat list. Final
discovery returned only `default`, `research`, and `vt`; all retained their Pi
bindings. Local receipts are in ignored `.kuggis/agents-acceptance.json`.

Bare `connect` now lists chats; opening or creating a chat requires `--chat`.
Tests verify that listing neither starts services nor writes bindings, that
explicit agent mismatches fail, and that concurrent creation saves one binding.
The wizard has no agent question. `--default-agent` configures the fallback for
new named chats; connect's `--agent` selects the saved agent per chat.

49 Rust and 57 Python tests pass, as do formatting, Clippy, and packaged native
and static Linux builds. CI now runs these v2 checks. The CI workflow itself has
not run on GitHub yet. Claude Code and native OAuth setup remain unimplemented.

Policy and VT have **not** been switched to Codex. Policy was actively working;
VT received a new task during handoff preparation. The source-digest check
rejected that stale handoff, leaving its Pi binding untouched. Their native Pi
histories remain available; migration requires refreshed context after their
active turns finish.
