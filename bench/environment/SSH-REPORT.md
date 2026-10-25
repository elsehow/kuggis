# Standalone SSH host acceptance

This records the initial bootstrap. See [shared Git acceptance](GIT-REPORT.md)
for the subsequent broker upgrade.

The production bootstrap was exercised on a fresh OVH RISE-S dedicated host
running x86_64 Ubuntu 24.04, with an AMD Ryzen 7 9700X (16 logical CPUs) and
64 GB of physical RAM. The guest limit was 56 GiB, with no CPU quota. The
static Linux binary was built from commit `0ed4d9c`; SHA-256:
`2da83a3ff8412ac1880ec3e788ae8ce1115d74a7e03c1af6604f2ccd4f6688a6`.

The synthetic cell had only provider and public-host grants. It had no
credentials, repositories, imported data, or model calls. The production cell
was not migrated or changed by these checks.

| Check | Result |
| --- | --- |
| Strict SSH bootstrap, pinned machine and cell signing identities | Passed |
| Signed boundary check verified by operator CLI | Passed |
| Private user, mount, PID and network namespaces | Passed |
| Actual systemd memory ceiling matches 56 GiB configuration | Passed |
| Allowed proxy connection and guest package installation | Passed |
| Denied host, address-literal and implicit exe.dev door requests | Passed |
| Direct IPv4, IPv6, metadata and host-service network bypass probes | Passed |
| Guest root firewall flush and alternate guest UID | Passed |
| Pi startup and persistent idle chat without inference | Passed |
| Unchanged CLI apply performs no setup | Passed |
| Actual setup reapplication preserves identity, workspace, configuration, jq and chat | Passed |
| Missing approved firewall snapshot refuses guest startup; restore recovers | Passed |
| Full host reboot, changed boot ID, automatic service startup and persistence | Passed |

One no-op approval became stale when a concurrent acceptance check appended
new signed evidence. Apply refused it; a fresh reviewed plan applied without
setup. This exercised the observation binding as well as idempotence.

Reproduce using [standalone SSH host acceptance](README.md#standalone-ssh-host-acceptance).
Raw logs, host identifiers, private receipts and preservation hashes stay in
ignored deployment storage. The signing secret stays on the host.

## Limits and next work

These checks validate a new standalone cell and the tested boundary. They do
not validate model inference, arbitrary kernel/container escape resistance,
repository access, supervisor integration, cross-provider doors, or migration
of an existing identity and its data.

Repository access will use one provider-neutral implementation across exe.dev
and SSH hosts, replacing the existing exe.dev Git path after canary validation.
Scoped read/write enforcement, forks and credential isolation must pass before
that rollout. Supervisor host bindings and state-preserving migration remain
required before moving an existing cell.
