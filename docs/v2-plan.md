# Kuggis v2: configured workspaces, autonomous agents

Status: implementation plan and tested first v2 slice. V1 remains in `cell/` and
`web/` while v2 is developed independently in `v2/`.

Implementation update: the v2 installer, host/guest helpers, persistent terminal
entry, CONNECT proxy, atomic transfers, private relays and exe private-ingress
configuration now exist. Local tests, a Linux build, and the first disposable
exe trial pass, including a full reboot and authenticated private ingress. See
[the testing guide](../v2/TESTING.md) for evidence and remaining gates.

## Product contract

Kuggis prepares a persistent, isolated workspace and gives it the authority
configured in its deployment environment. The operator connects over SSH and
uses an agent's own terminal UI. Pi, Codex, and Claude Code are guest programs,
not architectural dependencies. The repository owns its build and deployment
workflow. A development workspace is not a second production deployment.

**Configure the boundary, then let the agent run.** There are no runtime
approval prompts, approval hashes, permission-request queues, escalation
workflows, or optional approval mode. Configured actions run autonomously;
unconfigured actions fail. Agent installation recipes must also select and test
unattended behavior inside the environment. Kuggis must not replace a removed
approval prompt with an agent-side approval prompt.

Workspaces are flat. There are no nested cells, subcells, parent grants,
inherited authority, delegation protocols, or per-workspace child managers.
Initially support one dedicated Linux host and one guest per workspace. A flat
collection of SSH targets is sufficient; multiple guests per host are deferred.

## User workflow

The BigBrain trial showed that the manual setup below needs a complete operator
entry point. The [onboarding proposal](v2-onboarding.md) describes one create
operation with interactive questions or unattended flags/JSON, built on the
existing boundary and recipes. The initial exe/Pi create flow is implemented;
additional agents and native login profiles remain planned.

Deployment configuration is prepared before agents run. It selects an SSH target,
resource access, credentials, initial tools, launch command, and private web
ports. Missing configuration is an error, never a request for an approval.
Authentication, SSH host keys, and unattended sudo must be provisioned ahead of
time. Do not forward the operator's SSH agent into the guest.

Current CLI (setup takes a Linux binary built from the same source):

```sh
kuggis setup workspace.json --bin path/to/linux/kuggis
kuggis connect workspace.json
kuggis shell workspace.json
kuggis put workspace.json ./design.png inputs/design.png
kuggis get workspace.json output/report.pdf ./report.pdf
kuggis check workspace.json
```

Configuration paths are the initial interface. Name-based lookup can be a small
local registry later; it is not a tree or a second source of authority.

`connect` enters a persistent guest terminal such as tmux. Disconnection leaves
work alive. After a process exits or a machine reboots, the selected agent owns
conversation resumption; Kuggis never resubmits work. `shell` opens an ordinary
guest shell. Files and Git state are shared when switching agents, but conversation
formats are not translated. The core does not supervise agent prompts or models.

## External resources

A named resource is configuration shorthand for concrete outbound connections
and scoped credential files. For example, a deployment resource supplies a
dedicated SSH key and access to the production SSH endpoint. The remote account
or forced command limits what that key can do. A resource label does not enforce
authority, and network reachability does not imply application authorization.

Credential sources refer to files on the trusted deployment host. They are not
credential values in JSON, laptop credentials implicitly copied into a guest,
or paths that the guest can request at runtime. Installation happens before
launch, with private permissions. Credential destinations are relative to the
guest agent's home. The host configuration and the source credential directory
are not writable or mounted wholesale into the guest.

Guest-readable credentials can be copied. Revocation therefore includes revoking
or rotating them at the destination, or using expiry; removing a local file is
not sufficient. V2 does not initially implement a credential broker.

## Private webpages on exe.dev

Preconfigure a small set of private web ports, for example guest port 3000. The
agent can start any web application on a configured port without asking anyone.
For the first version the app binds the guest interface, not just guest loopback.
A host-controlled TCP relay exposes only the configured guest port to exe's
authenticated ingress. It must not create a guest route into host services.

The exe helper explicitly sets the VM proxy private and selects the host relay
port. The workspace agent does not receive the exe account-management key or a
way to make the proxy public. Check the selected port and visibility before
reporting a usable URL; ensure the relay is not exposed through an alternative
unauthenticated network path. A non-exe host needs a separately configured
authenticated ingress, or an SSH tunnel. V2 does not silently bind a public app.

Exe currently supports a private main URL and authenticated alternate ports
3000–9999. We initially select one explicit main port rather than auto-discover
guest listeners. Browser identity belongs to exe; the core needs no login UI.
Source, checked 2026-10-26: [exe HTTP proxy documentation](https://hub.exe.dev/docs/proxy).

## File transfers

`put` and `get` transfer a single file over SSH stdin/stdout through a fixed host
entry command. Paths are relative to `/home/agent/workspace`. File operations run
as the agent **inside the guest namespaces**, never as host root traversing a
guest-controlled filesystem. Reject absolute paths, `..`, and control characters.
Guest symlinks may resolve only within the guest's existing authority; they must
never cause host filesystem traversal. Shell-quote all remote arguments.

The default upload/download creates a new file and fails if it exists; an
explicit replace flag is an operation choice, not an approval prompt. Transfers
must stream binary data, propagate failure, and publish complete files atomically.
Temporary files must have private permissions and be cleaned up on failure.
Do not implement host-root `scp` into a mutable guest rootfs. Directory sync and
archive extraction are deferred.

## Rust core

Keep a separate Cargo workspace in `v2/`, initially containing one `kuggis` crate.
Do not move v1 or share its approval, agent, session, or provider types. Extract
small, useful boundary logic only when the new implementation needs it.

The core owns:

- Strict validated deployment configuration and concrete endpoint rules.
- SSH command construction with batch authentication, pinned known hosts, no
  agent forwarding, and a fixed host entry command.
- Repeatable setup, persistent storage, and guest terminal entry.
- Host-owned namespace isolation, firewall rules, and private ingress relays.
- Credential provisioning and file transfer within the guest boundary.
- Host-side boundary checks and ordinary machine-readable diagnostics.

The core does not own chats, OAuth UIs, provider account observations, models,
agent tool calls, request journals, signed statements, cloud reconciliation,
parent/child communications, application deployments, or runtime approvals.
Do not add a generic provider framework or a speculative chat API. A future
browser terminal can use the same entry path. A future custom chat UI is a
separate application with agent-specific adapters.

Use existing system facilities (OpenSSH, systemd-nspawn, nftables, tmux). Preserve
the current boundary's useful properties: guest root is not host root; all guest
traffic is subject to host policy; guest-modified files are never interpreted or
executed in the host namespaces; the environment cannot boot with a broken wall.
Avoid rewriting the existing Python boundary helpers in Rust merely to change
languages. First strip their v1 chat/signing assumptions and audit the result.

Keep service units and firewall rules in plaintext templates. Compile validated
configuration into a deterministic bundle with pure functions. Keep SSH, process,
filesystem and Linux effects outside the configuration and policy modules.

Network rules are exact destination/port pairs, not independent host and port
sets whose cross product grants extra access. DNS resolution must reject private
and metadata addresses by default; explicit address pins permit deliberately
configured private or IPv4 link-local platform endpoints. A TCP tunnel cannot
restrict the guest's TLS/HTTP host headers; shared gateways must enforce their
own authorization. The Pi recipe explicitly grants exe's already-attached LLM
gateway through such a pin, with provider credentials retained by exe.
Support SSH egress as well as HTTPS without
granting a general route around the boundary. No implicit exe door exception.

Setup is an idempotent sequence, not an approval transaction. Serialize updates,
stop guest activity before changing its boundary, install the new configuration,
verify it, then start. Errors are noninteractive and leave an incomplete boundary
stopped. Hashes may identify artifacts; they never request approval.

## Delivery sequence

1. **Configuration and transport draft.** Add the independent Rust crate, strict
   schema, exact endpoint matching, deterministic environment layout, SSH entry
   requests, private ingress metadata, fixtures, and tests. The CLI can validate
   and render these artifacts. It cannot claim that rendering installs isolation.
2. **Boundary installer.** Support one pinned Linux base and nspawn layout. Port
   and audit the firewall, guest UID mapping, namespace-safe updates, and startup
   guard. Implement destination checks, explicit SSH egress, secret installation,
   locking and failure recovery. Keep host policy independent of guest files.
3. **Entry and transfer helper.** Install the fixed host helper. Validate commands
   against host-owned configuration; enter the guest before launching any guest
   command. Implement persistent terminal attachment, a shell, and atomic file
   transfers. SSH errors must never fall back to a host shell or host file access.
4. **Private ingress and exe convenience.** Relay configured guest ports and
   validate private ingress. Add a thin create/configure helper that yields an
   ordinary SSH target. No account-management credentials in guests.
5. **Agent recipes and acceptance.** Install and configure the supported agents
   for unattended use. Exercise authentication, package installation, reboot,
   disconnect/reconnect, deployment access, private pages, and binary transfers
   on a disposable host. No paid inference is required for core tests.
6. **Move real workspaces, then remove v1.** Back up guest files and agent-native
   transcripts; migrate a workspace explicitly and verify preservation. Stop the
   old services before taking ownership. Retire v1 code and compatibility formats
   only after the migrated workspaces are accepted. Keep migrations out of the
   permanent v2 runtime when possible.

The [legacy duplication recipe](../v2/recipes/legacy/README.md) provides the
side-by-side migration path: resolve the current source binding, select guest
data declaratively, export and verify file manifests, configure a fresh host,
and preserve native Pi conversations. Authority and service ownership are
configured separately from the data copy. The recipe adds no migration protocol
or legacy types to the Rust runtime.

## Acceptance criteria

- No approval API, approval flag, prompt, parent field, or child-cell lifecycle.
- Guest root cannot modify host policy or escape through an update/transfer path.
- Each configured endpoint works; unconfigured host/port combinations fail.
- Missing credentials or broken policy fail clearly without widening authority.
- Disconnect preserves the terminal process; restart never replays agent work.
- Setup and upgrades preserve workspace data and agent-native sessions.
- Private web ingress rejects unauthenticated access, including alternate paths.
- Transfers preserve arbitrary bytes; failed transfers do not publish partial
  destination files or overwrite an existing file by default.
- Management credentials stay outside the guest; resource credentials are scoped.
- Pure configuration/transport tests pass locally; actual isolation claims require
  Linux boundary and deployed-ingress tests, not just unit tests.
