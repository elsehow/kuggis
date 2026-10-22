# Environment autonomy bench: measured result

The clean run on 2026-10-21 passed **22/22 required bench cases**, with zero
failed or unverified cases. It created a fresh exe.dev VM, started a minimal
container, installed its development tools from inside the restricted boundary,
exercised the tests, rebooted the VM, checked again, collected evidence, and
deleted the VM. The earlier diagnostic VM was also deleted.

The private run receipt records the measured source revision, binary and fixture
hashes, VM incarnation, boot IDs, and every case's expected and observed outcome.
Raw evidence stays in ignored local storage because it identifies the test
environment. This report publishes aggregate outcomes only.

## What passed

| Area | Evidence |
|---|---|
| Root autonomy | Normal guest user can sudo, change system configuration and create another user. |
| Tool installation | Started without Cargo, Node, npm or build-essential; installed all through the actual Kuggis proxy. |
| Real dependencies | Rust smoke project fetched a crate; Node smoke project fetched an npm package; both tested successfully. |
| Kuggis itself | 116 Rust tests passed; frontend built; default frontend suite passed 14 tests. |
| Local networking | Agent-created systemd HTTP service runs and is reachable through guest localhost. |
| Isolation | Host confirms distinct user, PID, mount and network namespaces; guest root maps to host UID 524288. |
| Network restrictions | Root and another user cannot reach forbidden HTTPS, direct public TCP/DNS, IPv4/IPv6 host controls, host port 22 or the platform gateway. |
| Guest tampering | Clearing proxy variables and changing internal firewall/routes or the guest's declaration does not expand authority. |
| Host state | Host key/config remain outside guest access; host firewall is checked independently. |
| Statements | Real Kuggis shipper signs an allowed session statement and refuses a forged `Checked`. |
| Persistence | Tools, agent configuration and the agent's service survive container restart, host-policy reload, and full VM reboot. |
| Failure behavior | Proxy outage prevents external access; missing host firewall prevents container start; host can recover an agent-stopped service. |

The VM ran Ubuntu 24.04.5, kernel 6.12.113 and systemd 255. The container used
Ubuntu 24.04 userspace, Rust/Cargo 1.95.0, Node 22.14.0 and npm 10.9.2. The final
run took roughly eight minutes, including base OS provisioning and cleanup.

## Finding outside the boundary implementation

An earlier diagnostic run failed the existing frontend auth test at
`web/tests/cells.test.mjs:212`: `browserFlow.prompt` was null. The same failure
reproduced in the **full frontend suite outside the container**. Five isolated
runs of that test passed; it also passed in the final clean VM run.

The test helper waits for a login notice but does not require the browser input
prompt to have arrived before dereferencing it. This is consistent with a timing
race. It was not patched or suppressed by the bench. The failed outside-container
run is preserved in the private diagnostic log.

## Scope

This validates the proposed userspace-autonomy architecture for the tested
configuration. It is a prototype fixture, not a migration of the existing cells
or an implementation of container-aware production `cell apply`/upgrades. Host
policy reload is tested; arbitrary production upgrades and environment reset are
not. The shared kernel and container runtime remain trusted; these probes are not
a kernel-escape audit.

The project's five optional installed-Pi/browser tests were skipped by its default
test command. They are not included in the 22 required bench cases or claimed as
passing. There were no model calls, real provider sign-ins or grants to existing
repositories/cells. The port-22 control is a synthetic host listener because the
exe.dev image does not run a TCP SSH daemon there.

## Artifacts and reproduction

- [Run instructions and design](README.md)
- Each local run keeps its receipt, machine-readable results, command evidence,
  host observations, reboot evidence, and deletion receipt in ignored `results/`.

The host records command arguments, exit status, elapsed time, stdout and stderr.
Positive controls must succeed outside the container before blocked-access cases
can pass. Every missing required case remains `unverified`, and the controller
returns nonzero if any required case fails or remains unverified. Three local
harness tests for result classification and cleanup ownership also passed.
