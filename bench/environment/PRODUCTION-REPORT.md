# Production environment migration acceptance

The production `cell setup` implementation passed all six acceptance cases on
a disposable fixture: migration preservation, normal-agent toolchain installation,
real registry builds, host boundary/signing checks, setup reapplication, and
missing-firewall startup refusal/recovery.

A full VM restart changed its boot ID. The signed boundary check, installed
tools, synthetic chat and credential fixture, and workspace marker survived.
A reachable host TCP listener was blocked for guest root, the agent, and a
newly created guest user. The disposable VM was then deleted using its
ownership receipt.

The integration's local Rust suite passed 116 tests. The frontend built and
passed 14 tests, with five optional tests skipped. Five Python checks passed,
including firewall comparison across chain creation-order changes and rejection
of reordered rules or changed hooks.

## Boundary and limits

The host retains the declaration, proxy, firewall, key, signer, door, and signed
log. The agent owns guest userspace and has sudo; host nftables filters the veth
for all guest users. Guest root maps to host UID 524288. Writable host mounts
are limited to the statement outbox and dedicated chat socket directory.
Setup preserves the guest filesystem on reapply and runs guest updates inside
its namespaces. The fixture uses Ubuntu 24.04 on an x86_64 exe.dev VM.
These checks do not establish resistance to arbitrary kernel or nspawn exploits.

The integration also prevents refusal receipts from following an attacker-created
symlink and rejects symlink/FIFO races when reading the outbox. Downloads use
signed Ubuntu metadata and upstream HTTPS checksum manifests. Package hosts
remain explicit grants.

## Reproduction and evidence

See [production migration acceptance](README.md#production-migration-acceptance)
for the fixture procedure and [the architecture experiment](REPORT.md) for the
separate 22-case result. Acceptance uses synthetic credentials and does not
require model inference.

Keep raw receipts, identifiers, logs, preservation manifests, and deployment
reports in ignored local storage. They can reveal an operator's environment
even without secret values. This public report contains aggregate fixture
results; it does not document a live user's deployment.
