# Hosted supervisor

Run the operator on a dedicated trusted exe.dev VM. Cells remain on separate hosts;
the supervisor holds the private tree, identity pins, operation receipts and an
operator SSH key. Chats, workspaces and provider credentials stay in their cells.
[Existing Ubuntu hosts](ssh-hosts.md) can be registered through pinned SSH.
Do not run agents or untrusted workloads on the supervisor: local processes can
impersonate its trusted ingress, and its SSH key can manage the exe.dev account.
Keeping that key off a laptop does not isolate it from a laptop agent that can
use an owner SSH credential to reach the supervisor and run sudo. Agents on
personal computers need separate credentials scoped to their own VMs, without
access to the owner's private keys or an SSH agent holding them. Browser access
to the supervisor does not require distributing its SSH key.

`cell ui` defaults to local access. Hosted access must be selected explicitly:

```sh
cell ui /var/lib/kuggis-supervisor/tree \
  --state /var/lib/kuggis-supervisor/operator \
  --assets /opt/kuggis/current/web --bin /opt/kuggis/current/cell \
  --listen 127.0.0.1:8000 --exe-proxy /etc/kuggis-supervisor/access.json
```

The private access file contains a canonical HTTPS origin (no trailing slash or
port) and stable exe.dev user IDs, not email addresses:

```json
{"origin":"https://supervisor.example","users":["usr_fixture"]}
```

The listener stays on loopback. Every route, including assets and SSE streams,
checks the exact Host, forwarded host, HTTPS protocol, and authenticated user ID.
Browser Origin must match when present; cross-site and same-site API requests are
refused. Top-level GET navigations to `/` can arrive from links and login redirects.
Missing or ambiguous authentication headers fail closed. Browser and
ingress credentials are stripped before forwarding to a cell.

This relies on [exe.dev's authenticated ingress](https://exe.dev/docs/login-with-exe)
overwriting `X-ExeDev-UserID`. Keep the
[HTTPS proxy private](https://exe.dev/docs/proxy):

```sh
ssh exe.dev share set-private "$SUPERVISOR_VM"
ssh exe.dev share port "$SUPERVISOR_VM" 8000
```

Deployment procedure:

Stop and persistently mask bundled host development agents, including their
activation sockets, on every Kuggis host: supervisor, cells (including production),
and publishers. Do this on the supervisor before giving it an operator key.
From an authenticated administrator SSH session, run the reviewed helper from
this release (copy `cell/src/host_agents.py` to the host as needed):

```sh
sudo python3 host_agents.py --apply
sudo python3 host_agents.py
```

Require `ok: true`. The helper stops/disables and persistently masks only
`shelley.socket` and `shelley.service`, preserves local unit definitions and a
receipt under root-only `/var/lib/kuggis-host-agent-policy/`, and verifies the
tracked Kuggis services retain their state and process IDs. It refuses known
managed-service dependencies on Shelley. A failure is a review item; do not
continue provisioning while the boundary is unverified. No guest, supervisor,
publisher, credential, or application restart is requested by this helper.

`UnitFileState=disabled` alone does not mean a service or socket has stopped.
Recheck after image updates and reboots. New cell setup applies the same embedded
helper before modifying the guest; `cell check` verifies the policy read-only.
Supervisor and publisher provisioning must run the helper explicitly. Stopping
Shelley interrupts its own sessions, so perform this step through SSH, not from
a Shelley session. Normal Kuggis deployment continues through its existing
SSH identity and services. Rollback of this policy is a separate administrator
decision: preserve the receipt and original unit files; do not automatically
restore host agents when rolling back an application release.

1. Build `web/dist` and a Linux `cell` binary. Put a release under
   `/opt/kuggis/releases/RELEASE/` with `cell` and `web/`, owned by root, then
   point `/opt/kuggis/current` at it. Keep the previous release for rollback.
2. Create a system user `kuggis-supervisor` with home
   `/var/lib/kuggis-supervisor` (0700). Generate its dedicated SSH key there and
   register only the public key with exe.dev. Configure `exe.dev` and `*.exe.xyz`
   to use it with `IdentitiesOnly yes`, `BatchMode yes`, and pinned known hosts.
   Set `ForwardAgent no`, `ClearAllForwardings yes`, and `ForwardX11 no` on
   deployment connections, including nested SSH and manual SCP commands. The
   runtime supplies these restrictions explicitly; standalone helper scripts
   must do so too. The supervisor service removes inherited `SSH_AUTH_SOCK`
   and `SSH_AGENT_PID`; authentication uses its dedicated identity files.
   This operator needs account management access; a VM-scoped key is insufficient.
   Never copy a laptop private key or a cell's provider credentials.
3. Establish the owner ID through an authenticated private HTTPS request to a
   temporary probe. Verify that a caller-supplied user ID is overwritten. Save
   the allowlist at `/etc/kuggis-supervisor/access.json`, root-owned and readable
   only by the supervisor group (0640). Stop the probe before starting the UI.
4. Stop the old operator. Back up and copy only its tree (including `.pins`,
   `.hosts` and `.identities.json`) and
   operator state to the new home. Keep directory permissions 0700 and files
   0600, owned by the service user. Do not copy an entire checkout's `.kuggis/`:
   it can contain unrelated private backups and evidence. Run only one active
   supervisor for a tree; the local state lock does not coordinate two machines.
5. Install [supervisor.service](supervisor.service) as
   `/etc/systemd/system/kuggis-supervisor.service`, reload systemd, and enable it.
   The service starts on boot, has no root capabilities, and writes only to its
   state directory and private temporary directory.
6. Verify the private HTTPS URL with an authenticated browser, unauthenticated
   requests, forged identity headers, and foreign Origin headers. Compare chat
   IDs, titles, archive flags and contents before and after a service restart and
   VM reboot, with the original operator stopped. Verify SSH relay streaming.
   Remove any temporary test token and probe. Store real receipts and deployment
   configuration outside tracked source, such as ignored `.kuggis/`.

To roll back, stop the hosted service before restarting the old operator. If the
hosted supervisor has changed declarations or operation receipts, copy its latest
tree and operator state back first. Repointing `/opt/kuggis/current` and restarting
the service rolls back code without moving chats or operator state.

## Updating an existing supervisor

A merge does not deploy itself. Build an exact source commit containing both the
new changes and the features in the running release. Run the Rust tests, frontend
build/contracts check, and web integration tests before packaging the Linux binary
and `web/dist`. Include a `release.json` with the source commit and artifact hashes;
keep deployment addresses, private tree contents, and acceptance receipts outside
tracked source.

Stage that package in a new root-owned `/opt/kuggis/releases/RELEASE` directory.
Check that no creation, deletion, or apply operation is running. Record the current
release and snapshot the authoritative tree and operator state on the supervisor.
Atomically replace the `current` symlink and restart `kuggis-supervisor.service`.
Verify the served asset hashes, authenticated cell inventory, chat history and SSE
relay, plus rejection of unauthenticated requests and foreign origins. If those
checks fail, restore the previous symlink and restart the service. Retain the old
release for rollback.

The supervisor serves the frontend and control-plane API. Restarting it reconnects
browser streams; chat processes keep running in their cells. This does not deploy
new binaries into existing cells. A runtime update is a separate plan/apply from
the supervisor's authoritative tree, after checking that the affected cell is idle.
Do not reconcile cells merely to update frontend assets, and never replace the
live tree with a checkout's older copy. Hosted sign-in retains Pi's manual redirect
prompt; automatic loopback callbacks require a supervisor on the browser's machine.
