# Environment autonomy bench

Production integration: [migration acceptance results](PRODUCTION-REPORT.md).
Standalone SSH hosting: [dedicated-host acceptance results](SSH-REPORT.md).

Architecture experiment result: [22/22 cases passed on exe.dev](REPORT.md), including a
full VM reboot. The report also records an intermittent existing auth-test failure.

A disposable exe.dev VM runs a persistent Ubuntu 24.04 systemd-nspawn container.
The container gets root in a private user namespace, a writable root filesystem,
its own systemd and network namespace, and a deliberately small host connection:
the existing Kuggis CONNECT proxy plus a scoped statement outbox. The host owns
the firewall, authoritative declaration, signing key, and evidence recorder.

This is an executable architecture experiment. It does not change production
cell setup, expand existing cells' grants, sign in to providers, or call models.
The shared host kernel and container runtime remain trusted. Passing probes is
evidence for the tested configuration, not proof against all container escapes.

## Run

Requires Python 3.10+, SSH access to exe.dev, and a Linux x86_64 `cell` binary
built from this checkout. From the repository root:

```sh
cargo zigbuild --manifest-path cell/Cargo.toml --release --locked --target x86_64-unknown-linux-musl
python3 bench/environment/run.py --bin cell/target/x86_64-unknown-linux-musl/release/cell
```

The controller creates a unique VM, uploads the binary and `git archive HEAD`,
bootstraps the container, runs the tests, reboots the VM and checks again,
downloads the evidence, then deletes the VM. Source is the committed revision,
not uncommitted files. Bench scripts themselves are uploaded from this checkout.
VM creation may consume account capacity. No existing VM is adopted or modified.

`results/<run>/receipt.json` records the VM's name, unique tag, creation timestamp,
source commit and binary hash. Deletion requires matching both the tag and VM
incarnation. Evidence includes command arguments, exit statuses, timings, stdout,
stderr, host namespace/mount observations, service journals, and JSON/Markdown
reports. These contain no real provider credentials; the signing key is synthetic
and remains on the disposable VM. Raw evidence can still identify an account's
VMs, source revisions, network, and local paths. Keep it in ignored `results/`
or private storage; publish only reviewed aggregate results. The report always includes all required cases:
unrun or dependency-blocked cases are `unverified`, never `pass`. Any non-pass
gives the controller a nonzero exit status.

For investigation, use `--keep-vm`. Resume only through that run's receipt:

```sh
python3 bench/environment/run.py --resume bench/environment/results/RUN --stage test --keep-vm --bin PATH
python3 bench/environment/run.py --resume bench/environment/results/RUN --stage cleanup
```

Phases: `inspect`, `setup`, `test`, `post-reboot`, `all` (default), `cleanup`.
`post-reboot` alone does not reboot a VM; both the controller and host verifier
require a boot ID change before that case can pass. Tests run in a remote systemd service so an
SSH interruption does not kill a half-finished operation. Evidence is saved after
each case. Resume is for diagnosis; a clean full run is required for acceptance.

## Contract

The fixture is intentionally not a privileged container. Root maps to host UID
524288; the host applies input/forward rules to `ve-cellbench`, independently of
guest UID, proxy variables, guest nftables, routes or source addresses. Ordinary
guest loopback traffic is unrestricted. An external startup guard compares live
host rules to the host's approved rule structure. It refuses a missing or changed
wall. The default nspawn networkd masquerading configuration is overridden.

The guest starts without compilers or Node. It installs build tools with apt,
Rust 1.95.0 through rustup, and Node 22.14.0 from its upstream archive. These are
fixture versions, not a production toolchain policy. HTTPS downloads use the
actual Kuggis proxy with explicit hosts for Ubuntu packages, Rust, Node, npm and
crates.io. Release downloads are checked against upstream HTTPS SHA-256 manifests.

Tests cover guest sudo/system changes, real registry dependencies, Rust and Node
smoke projects, the current Kuggis Rust/frontend suites, local services, root and
alternate-user boundary probes, host secrets/config, the real signed outbox,
container restart, host-policy reapplication, proxy outage, missing firewall,
guest-service failure/recovery, and a full VM restart. Host-side positive controls
prevent ordinary outages or unavailable destinations from counting as isolation.

`BoundarySpec`, `Result`, and `Status` are the only new model types. The experiment
keeps mutable environment state separate from host-owned boundary configuration
and from observations; no production Rust schema or grant wire format changes.

## Local verification

```sh
python3 -m unittest discover -s bench/environment -p 'test_*.py'
```

These tests check failure classification and VM ownership guards. The isolation
and autonomy claims require the real VM run.

## Production migration acceptance

`production.py` tests the actual production `cell setup` implementation, not the
architecture fixture. It refuses any declaration except `env-migration`.
On a disposable VM, install the previous binary with a declaration containing
package host grants, create a chat titled `Migration acceptance`, write
`preserved` to `/home/agent/work/migration-marker`, and seed an `auth.json` with
synthetic credentials. Run the new binary's `setup` on the same declaration,
then `sudo python3 production.py`. It records each result in
`/var/lib/cell/production-acceptance.json` and exits nonzero on any failure.
It checks migration preservation, toolchain installation as the normal agent,
real registry builds, host authority, reapplication and missing-wall recovery.
Run it only on a disposable VM; reboot that VM and run `cell check` plus the
preservation/tool version checks afterward. Use `run.py`'s ownership receipt
checks when deleting the VM. Never seed synthetic credentials on a real cell.

## Standalone SSH host acceptance

`ssh_acceptance.py` exercises the production runtime on a disposable cell whose
name ends in `-canary`, bootstrapped with the [SSH host CLI](../../deploy/ssh-hosts.md).
The declaration needs Ubuntu package mirror grants (`us.archive.ubuntu.com` and
`security.ubuntu.com`). It refuses other grant types besides public hosts and
providers. Run each phase as host root, passing the exact cell name:

```sh
python3 ssh_acceptance.py test-canary seed
python3 ssh_acceptance.py test-canary boundary
python3 ssh_acceptance.py test-canary verify
python3 ssh_acceptance.py test-canary fail-closed
```

The seed requires an empty chat list and no provider credentials. It installs
`jq` through the guest proxy, writes workspace and guest configuration markers,
starts an idle Pi chat without calling a model, and records host identity hashes
in a private host receipt. Boundary tests flush only the guest firewall and
exercise an alternate guest UID. The fail-closed test temporarily hides the
approved firewall snapshot and checks that guest startup refuses; the actual
host firewall stays loaded, protecting the public proxy listener throughout.

Separately review and apply an unchanged CLI plan, which must contain no setup
actions, then run `verify`. To exercise actual reinstallation, run
`cell setup /etc/cell/cell.json --host-runtime /etc/cell/host.json` on the host
and verify again. Run `verify` after restarting `cell-agent`; after an explicitly
authorized host reboot, run `post-reboot`, which requires a changed boot ID.
These phases check identity, configuration, workspace, installed software,
idle chat, Pi startup, and a fresh signed boundary check. Verify the resulting
signed check and pinned identity with `cell host plan` from the operator machine.
Keep raw logs and deployment receipts in ignored private storage.
