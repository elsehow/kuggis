# Existing Ubuntu hosts over SSH

The first portable-host milestone bootstraps a new, standalone cell on a
dedicated **x86_64 Ubuntu 24.04 host with systemd**. The host can be a cloud VM
or a physical server from any provider. No provider API or nested virtualization
is needed. The bootstrap passed [live acceptance on an OVH dedicated host](../bench/environment/SSH-REPORT.md),
including boundary checks, reapply, and a full reboot. Migration of an existing
cell remains separate from bootstrapping a new cell.

The supervisor can register a bootstrapped host as a standalone root cell and
route chat, streaming, provider settings, health/recovery, display-name changes,
and reviewed configuration changes over pinned SSH. It does not move an existing
cell, provision/delete external servers, or implement cross-provider parent–child
doors. Provider, public-host, and repository code grants work.
[Scoped Git](scoped-git.md) uses the same broker on every host;
private reads and writes require host-held repository credentials. Publishing
uses the same operator-configured publisher and host signing bridge on either
backend. Private publishing has passed live SSH canary acceptance. exe.dev
integration grants and private door hostnames are refused before connecting.
Migrating an existing cell still requires a separate state-preserving cutover.
See the [supervisor acceptance results](../bench/environment/SUPERVISOR-SSH-REPORT.md)
for tested flows and remaining limits.

## Configuration

The SSH account needs a key, passwordless `sudo`, and Python 3. Setup installs
the cell runtime and an Ubuntu nspawn environment; use a host dedicated to this
cell. Network restrictions apply to the guest, including guest root. Host root
and the operator remain trusted.

Pin the server's SSH host key in a dedicated known-hosts file, verified through
the provider console or another trusted channel. The CLI refuses unknown keys,
disables agent forwarding, and ignores ambient SSH configuration. Use a direct
hostname/IP, not an SSH alias or ProxyJump. Neither private keys nor provider
credentials go in the configuration.

Keep the files under an ignored private directory, for example `.kuggis/hosts/`.
An example `host.json` (replace paths and hostname):

```json
{
  "hostname": "worker.example.net",
  "user": "operator",
  "port": 22,
  "identity_file": "/absolute/private/operator_ed25519",
  "known_hosts": "/absolute/private/worker_known_hosts",
  "runtime": {
    "backend": "ssh",
    "dns": ["127.0.0.53"],
    "memory_max": 8589934592
  }
}
```

`dns` must match the nameserver addresses in the host's `/etc/resolv.conf`.
The systemd-resolved loopback stub is supported, as are IPv4/IPv6 upstream
resolvers. Guest processes still cannot access DNS or the network directly.
`memory_max` is the guest's memory ceiling in bytes; leave room for the host.
CPU is not capped by Kuggis. Existing exe.dev cells retain their 4 GiB default.

Start with a new synthetic test declaration, such as `worker.json`:

```json
{"name":"worker","grants":[{"provider":"openai"},{"host":"pypi.org"}]}
```

## Inspect, review, and apply

Build the static Linux binary as described in the main README. Run these
commands from the operator machine; use the same binary and state directory
for plan and apply:

```sh
cell host inspect .kuggis/hosts/host.json
cell host plan .kuggis/hosts/worker.json .kuggis/hosts/host.json \
  --bin cell/target/x86_64-unknown-linux-musl/release/cell \
  --state .kuggis/hosts/worker-state

# Review the plan, then repeat its approval hash:
cell host apply .kuggis/hosts/worker.json .kuggis/hosts/host.json \
  --bin cell/target/x86_64-unknown-linux-musl/release/cell \
  --state .kuggis/hosts/worker-state --approve HASH
```

The plan binds the target, SSH trust file contents, host machine identity,
declaration, runtime configuration, executable digest, and observed state.
Apply checks them again, and the remote helper rechecks the host under a lock
before writing or executing anything. Mutations are never automatically retried
after an SSH disconnect. The binary is staged in a root-only temporary directory
and its digest verified before execution.

Success requires a passing signed boundary check for the reviewed cell, binary,
and host runtime. The local `host-pin.json` receipt preserves the cell's signing
key and machine identity; subsequent runs reject replacements. Reapplying an
unchanged, verified deployment performs no setup. The state directory is private
deployment state and must be backed up with the host.

After a partial failure, a reachable host's newly created signing key is pinned
even if setup failed. Review another plan before retrying. If the connection is
lost before that receipt can be saved, existing-host adoption is deliberately
refused: inspect and recover the deployment record explicitly. The CLI does not
erase partial environments or replace keys. Failed bootstrap staging remains in
`/var/tmp/kuggis-host-*` for diagnosis.

## Registering with a supervisor

Generate a dedicated SSH key on the trusted supervisor and authorize only its
public key on the host. Use `restrict` in `authorized_keys` to disable forwarding
and PTYs; the account still needs passwordless sudo for host administration.
Keep the private key on the supervisor. Copy the previously verified known-hosts
entry and `host-pin.json` receipt through a trusted channel. The supervisor's
`host.json` must use absolute paths to its own key and known-hosts files.

Use a separate supervisor tree and state directory for canary acceptance first.
For a registered tree, run the following as the supervisor service user:

```sh
cell host register /private/worker.json /private/host.json \
  --receipt /private/host-pin.json --tree /private/tree --state /private/operator

# Review the cell, host, machine ID, signing key and passing signed check.
# Stop the supervisor using this state directory before applying registration.
cell host register /private/worker.json /private/host.json \
  --receipt /private/host-pin.json --tree /private/tree --state /private/operator \
  --approve HASH
```

Registration adds a new root cell with a stable UUID, its existing signing-key
pin, and a private `.hosts/NAME.json` binding. The cell name must be new: this
command cannot adopt or replace an existing exe.dev cell. Existing cells must
already have identity records. The approval binds the observed host, declaration,
SSH trust contents and current supervisor tree. The state lock prevents
registration while that supervisor is running. The declaration is written last;
interrupted registration remains unavailable and requires explicit repair.

Start the supervisor and the new cell appears in its normal cell list. Chat and
provider settings use the same APIs and frontend as exe.dev cells. Each SSH
connection checks the pinned machine, cell name and signing key before running
its command; known-hosts contents are frozen for the connection. A missing or
invalid binding never falls back to exe.dev routing. Configuration approvals
include the full host plan, and apply refuses running chats or changed identity,
host, binary, trust or observation. Failed setup is not automatically retried.

External servers remain operator-owned. The UI hides deletion and subcell
creation for them, and the backend refuses those operations. Unsupported grants
fail before connecting. The exe.dev CLI planner also refuses external targets;
use the supervisor's reviewed settings flow or `cell host plan/apply` instead.
Back up `.hosts`, `.pins`, `.identities.json`, declarations and operator state
together. Do not run two supervisors against the same active cell outside a
controlled canary acceptance run.

## Migrating an existing cell

Use the separate [migration procedure](migration.md), including an isolated
restore rehearsal, a fresh final snapshot, source fencing, and verification
before restoring supervisor routing. Registration is deliberately unable to
perform this operation. The source VM and destination canary remain private
rollback backups; moving the latest state back is required after new writes.

Repository credentials must be provisioned on the destination before cutover.
The shared broker has passed live private clone/fetch, read-only enforcement,
and authenticated dry-run push [acceptance](../bench/environment/GIT-REPORT.md).
Preserve existing grant semantics, including independently granted public hosts.
Cross-provider subcell doors and provider-owned server lifecycle operations
remain separate capabilities.
