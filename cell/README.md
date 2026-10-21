# cell

The typed core of a kuggis cell. A cell is a root, a set of grants and a
key. This crate holds the types, the invariants, and the two pieces that
sit on the trust boundary: the signed envelope for everything a cell says,
and the CONNECT proxy that is the wall's one road out.

Sixteen types carry the design.

| kind | types |
|---|---|
| parsed primitives | `Name`, `Host`, `Repo`, `Url`, `Commit`, `Hash` |
| the cell | `Harness`, `Grant`, `Cell`, `Tree` |
| the plan | `Action`, `Plan`, `Approved` |
| what a cell says | `State`, `Said`, `Statement` |

Each primitive has one constructor, its parser, and deserializing goes
through it, so a bad value fails to load rather than loading wrong.
`Tree::new` is the only way to get a tree and refuses every forbidden
shape. `Statement::open` is the only way to read what a cell said, and it
verifies first. `Approved::new` is the only way to get something apply
will run, and it checks the hash the operator repeated back.

The same function derives a cell's allowlist on the laptop, to print the
plan, and on the box, to configure the proxy.

## The binary

    cell key new PATH               mint a secret (0600); print its public key
    cell key pub PATH               print the public key of a secret
    cell say CELL SEQ SECRET        sign the Said on stdin
    cell open KEY                   verify the Statement on stdin; print what it said
    cell allow CELLFILE             the hosts a cell's proxy may tunnel to
    cell tree DIR                   check a directory as one tree
    cell plan DIR [NAME] [--bin LINUX_CELL] [--have OBSERVED [--box HASH]]
    cell apply DIR [NAME] --approve HASH [--bin LINUX_CELL] [--pins DIR]
    cell adopt VM NAME              tag an existing VM as NAME's root
    cell observe [--from DIR]       read exe.dev's listings back as cells and doors
    cell proxy --listen ADDR --cell CELLFILE [--private-ok]
    cell setup CELLFILE
    cell check
    cell probe
    cell door --listen ADDR --cell CELLFILE --said DIR --inbox DIR
    cell ship --cell CELLFILE --secret FILE --outbox DIR --said DIR [--once]
    cell run --cell CELLFILE --inbox DIR --outbox DIR --home DIR
    cell knock --inbox DIR
    cell brief DIR NAME SESSION TEXT
    cell pull DIR NAME [--since N]
    cell login DIR NAME

A cell file:

```json
{"name":"airo","grants":[
  {"agent":"claude"},
  {"code":{"repo":"elsehow/airo","write":true}},
  {"code":{"repo":"forecastingresearch/airo","write":false}},
  {"host":"pypi.org"}
]}
```

The edge is one GitHub App for every cell, so GitHub cannot tell a parent
from a child. What tells them apart is which repository each carries. A
child's grant `{"code":{"repo":"elsehow/kuggis","write":true,"via":"cells/kuggis"}}`
is authority on the upstream carried by a fork under an org you own: the
edge holds the fork, the child pushes there and reports a branch upward.
Its parent carries the upstream, opens the pull request and merges it.
Merging is writing the upstream, and nothing else enforces it. A parent
writing the upstream covers a child writing it through any fork, and a
codebase counts once toward the one-writable-repository rule, so the
parent may carry the fork too, to sync it and fetch the child's branches.
Clones land in `work/<owner>/<repo>`.

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

## Apply

`cell plan DIR` plans every cell in the tree against exe.dev and the
boxes, prints the plans and one hash; `cell apply DIR --approve HASH`
runs them. Four rules make it safe.

Apply is a loop, not a script. Before every action it observes again and
re-plans, and the fresh plan must sit inside the approval: the same
declaration, a subset of the actions. A done action vanishes, a failed one
is retried by running apply again, success is proven by observing rather
than by trusting the edge's reply, and any drift beyond the approval stops
the loop. Cells go leaves first, so a child's VM exists before its
parent's door is opened.

Observe reports reach, not configuration. A VM is a cell only by its
`cell-<name>` tag, so an existing VM is brought in by `cell adopt`, on
purpose. Every integration that reaches the VM counts, by tag or by name,
and a GitHub integration counts once per repository. The converse holds
too: apply attaches only an integration that is exactly one grant, and
makes one, `gh-<owner>-<repo>` or `-ro`, when a repository has none. An
integration that carries more comes off whole, and the rest waits for a
new approval. One by a tag other VMs share is refused, not detached.
exe.dev's default `llm` is attached to every VM and can only be detached
from all of them, so it is the one grant the edge cannot take away; the
wall check proves the proxy refuses it.

The box's word is `Checked`. Setup ends with a check the box signs: the
cell hash it runs, the sha256 of the binary that checked it, and which
harnesses have a credential on the box. The laptop pulls the log over
ssh, pins the box's key on first contact in `DIR/.pins`, verifies every
statement and takes the last passing check as what the box runs. A key
that changes is refused until the pin is removed by hand.

Apply never destroys. There is no destroy action, no `rm`, no retagging.
A cell file removed from the tree leaves its VM alone.

Setup ships a Linux build of this binary: `cargo zigbuild --release
--target x86_64-unknown-linux-musl`, with rustup's toolchain first on
`PATH`, gives a static one in twenty seconds. Give the same `--bin` to
`cell plan` and `cell apply`: a box whose last check names another
binary needs setup, and the approval binds the hash of the one shipped.

## The wall

`cell setup CELLFILE`, run once as root on a box, builds the wall: an
`agent` user with no sudo and one open socket, the proxy on loopback; a
`cell-proxy` user that may resolve at exe.dev's gateway and open TLS
ports and nothing else; a `cell-door` user that serves the statement log
and files what a parent says; a `cell-say` user that holds the key and
ships what the agent says; nftables rules by uid, loaded before the
network; the signing key minted 0600 for `cell-say` alone; everything as hardened systemd
services. `cell check` then runs `cell probe` as the agent, which tries
everything the threat model says the agent cannot do, adds what root can
see, signs the result as `Checked`, with the binary's hash and which
harnesses are signed in, and appends it to the log the laptop pulls.
Both ran on a fresh exe.dev VM: every probe matched, setup is
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

## The voice

The agent cannot sign, by design, so it drops a `Said` in
`/var/lib/cell/outbox`, as `<name>.tmp` renamed to `<name>.json`, and
`cell ship` signs it as the cell and appends it to the log. The shipper
runs as `cell-say`, which owns the key and the log and is the only user
besides root that can read the key; root's wall check appends to the same
log under the same lock, so no two statements take one number. The outbox
is the agent's and the shipper's alone to write.

Everything in the outbox is the adversary's. A file is read without
following links, capped at 64 KiB, parsed strictly, and checked against
the cell: only the wall check says `Checked`, a session names a harness
the cell runs, a verdict names a repository the cell holds. What passes is
renamed to `<name>.<seq>`, the receipt, and the agent reads its own
statement back from the world-readable log. What fails is replaced by
`<name>.refused` holding the reason. The agent gets no other feedback and
needs none. The wall check sends a session statement through the outbox
and a forged `Checked`, and expects a receipt for the one and a refusal
for the other.

## A session

`cell run` is the adapter, running as the agent. It watches the inbox,
clones every granted repository into the agent's `work` directory through
the edge, and runs each brief as a headless Claude Code session there,
full-auto, since the wall is the boundary. The event stream becomes
session statements through the outbox: Starting, Working as the model
talks, Done with its answer, or Blocked with what went wrong. A second
brief to the same session name resumes it. The command line and the
event translation are the only harness-specific code, and nothing below
them knows which harness runs. Setup installs Claude Code as root, with
root's open internet, and copies the binary where the agent can run it
and nobody can change it.

From the laptop: `cell brief DIR NAME SESSION TEXT` files the brief in
the box's inbox over ssh, as you; `cell pull DIR NAME` pulls the log,
verifies it under the pinned key and prints it; `cell login DIR NAME`
opens Claude Code on the box as the agent, for the one thing only you can
do, the sign-in. A knock through the door names the parent cell; one over
ssh names nobody, and the agent knows that means you.

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
injected resolver, the apply loop against an in-memory exe.dev with
signing boxes, the shipper over a real outbox with a link and a
directory left in it, four writers racing for the log's lock, and the
binary as separate processes. The proxy tests
include a table of hostile request lines; one of them, a port written with
a leading zero, found a real hole while this was being written.
