# Existing Ubuntu hosts over SSH

The first portable-host milestone bootstraps a new, standalone cell on a
dedicated **x86_64 Ubuntu 24.04 host with systemd**. The host can be a cloud VM
or a physical server from any provider. No provider API or nested virtualization
is needed. The bootstrap passed [live acceptance on an OVH dedicated host](../bench/environment/SSH-REPORT.md),
including boundary checks, reapply, and a full reboot. Migration of an existing
cell remains separate from bootstrapping a new cell.

This is a bootstrap CLI, not yet a second backend for the operator tree. It does
not register the host in the browser UI, move an existing cell, provision/delete
servers, or implement parent–child doors. Provider and public-host grants work;
code, integration, and publishing grants are refused before connecting because
their current enforcement depends on exe.dev. Do not remove an existing cell's
code grants to bypass that check. Migrating Merrillfront requires scoped Git
access and supervisor/identity integration in subsequent work.

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

## Remaining work before a migration

1. Implement one provider-neutral repository-scoped Git access path, with tests
   for read-only grants, forks, and credential isolation. Validate it on the
   canary, then use it for both exe.dev and SSH hosts and retire the existing
   exe.dev Git path. Hosting backends must not select separate permanent Git
   implementations. Preserve grant semantics throughout the rollout; never
   broaden access to make migration work.
2. Route supervisor chat, health, settings, and lifecycle operations through an
   explicit host binding; external hosts must never inherit exe.dev deletion.
3. Add a reviewed migration that preserves cell identity, signed logs, chats,
   credentials, workspaces, and imported data, with one active instance and a
   rollback procedure.
4. Repeat boundary, reapply, and reboot acceptance after Git and supervisor
   integration, then run the migration acceptance checks. Cross-provider subcell
   doors remain a separate capability.
