# cell

The typed core of a kuggis cell. A cell is a root, a set of grants and a
key. This crate holds the types, the invariants, and the two pieces that
sit on the trust boundary: the signed envelope for everything a cell says,
and the CONNECT proxy that is the wall's one road out.

Fifteen types carry the design.

| kind | types |
|---|---|
| parsed primitives | `Name`, `Host`, `Repo`, `Url`, `Commit`, `Hash` |
| the cell | `Harness`, `Grant`, `Cell`, `Tree` |
| the plan | `Action`, `Plan` |
| what a cell says | `State`, `Said`, `Statement` |

Each primitive has one constructor, its parser, and deserializing goes
through it, so a bad value fails to load rather than loading wrong.
`Tree::new` is the only way to get a tree and refuses every forbidden
shape. `Statement::open` is the only way to read what a cell said, and it
verifies first. `Plan::approve` binds an approval to the exact plan.

The same function derives a cell's allowlist on the laptop, to print the
plan, and on the box, to configure the proxy.

## The binary

    cell key new PATH               mint a secret (0600); print its public key
    cell say CELL SEQ SECRET        sign the Said on stdin
    cell open KEY                   verify the Statement on stdin; print what it said
    cell allow CELLFILE             the hosts a cell's proxy may tunnel to
    cell tree DIR                   check a directory as one tree
    cell plan DIR NAME [--have OBSERVED] [--box HASH]
    cell observe [--from DIR]       read exe.dev's listings back as cells and doors
    cell proxy --listen ADDR --cell CELLFILE

A cell file:

```json
{"name":"airo","grants":[
  {"agent":"claude"},
  {"code":{"repo":"elsehow/airo","write":true}},
  {"code":{"repo":"forecastingresearch/airo","write":false}},
  {"host":"pypi.org"}
]}
```

A cell does not know where it sits. The directory is the tree:
`cells/forecasting.json` is held by the human, `cells/forecasting/airo.json`
sits under forecasting, and re-parenting is `mv`. The box has no use for
the edge, a name is not a capability, and the hash must mean "this box's
configuration" and nothing else, so the edge lives in `Tree` alone.

The tree is applied as doors. A parent reaches a child through an exe.dev
peer integration named `door-<child>`, attached to the parent's VM alone,
so the gateway stamps every knock with the caller and serves the door to
nobody else. The plan opens and closes doors; observe reads them back; the
proxy lets any door hostname through to the gateway, which refuses the
ones not attached. Through a door the parent sends a `Said`, a brief or a
reply, trusted because only the parent can knock. Everything upward is a
signed `Statement`.

## The wall

`cell setup CELLFILE`, run once as root on a box, builds the wall: an
`agent` user with no sudo and one open socket, the proxy on loopback; a
`cell-proxy` user that may resolve at exe.dev's gateway and open TLS
ports and nothing else; a `cell-door` user that serves the statement log
and files what a parent says; nftables rules by uid, loaded before the
network; the signing key minted root-only; everything as hardened systemd
services. `cell check` then runs `cell probe` as the agent, which tries
everything the threat model says the agent cannot do, adds what root can
see, signs the result as `Checked` and appends it to the log the laptop
pulls. Both ran on a fresh exe.dev VM: every probe matched, setup is
idempotent, and the wall survives a reboot.

## The door

`cell door` listens on loopback where exe.dev's in-VM auth proxy delivers
the VM's web traffic. A parent reaches it at `door-<child>.int.exe.xyz`
through a peer integration attached to the parent's VM alone; the
platform sets `X-Exedev-Source-Vm` after stripping whatever the caller
sent, and the gateway answers everyone else with "not attached".
`GET /said?since=N` serves the signed log; `POST /say` files one brief
for this cell, or one reply, in the inbox the agent reads. The agent
cannot knock on its own door from inside: the wall allows it only the
proxy's port. Verified live between two VMs, spoofed header included.

## The proxy

Standard library only. It tunnels CONNECT to allowed hosts on 443 or 8443
and refuses everything else: other methods, malformed targets, address
literals, ports off the list, hosts off the list, oversized or slow
request heads. After resolving an allowed name it checks where the name
points: an exe.dev edge host must resolve to the platform gateway and
nowhere else, and every other host must resolve somewhere public. TLS runs
end to end, so the proxy sees hostnames and never content.

## Tests

`cargo test` runs the unit tests, the proxy over real sockets with an
injected resolver, and the binary as separate processes. The proxy tests
include a table of hostile request lines; one of them, a port written with
a leading zero, found a real hole while this was being written.
