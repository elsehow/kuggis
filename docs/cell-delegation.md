# Cell discovery and delegation (on top of UUID identity)

A parent can drive work in **each direct child**: start a task, continue its
session, and read signed status and results. Each task executes in the child's
own environment and grants. This is delegation, not filesystem sharing or
unrestricted remote shell access. Deeper work is delegated through successive
parents; no full-tree discovery endpoint is exposed.

## Inside a cell

```sh
cell context
session=$(cell session new)
request=$(cell request new)
# Save these two IDs before submitting; use a child UUID from `cell context`.
cell delegate CHILD_UUID "$session" "$request" 'Run the tests and report the result'
cell child-status CHILD_UUID "$session"
# Later polls can start at the previous response's next_since cursor.
cell child-status CHILD_UUID "$session" 12
# Continue the same conversation with a NEW request ID for a NEW instruction.
request=$(cell request new)
cell delegate CHILD_UUID "$session" "$request" 'Fix the failing test'
```

A successful delegate response means **filed in the child's inbox**, not that
the model accepted or completed the work. Preserve request IDs: after a timeout
or lost acknowledgement, retry the same session, request, and text. The child's
durable request journal prevents duplicate dispatch. Do not allocate a fresh
request ID merely because the network outcome was uncertain.

Status resolves the child's UUID through trusted context and verifies its pinned
signing key, immutable runtime binding, and consecutive sequence numbers before
displaying session results. Each result includes `cell_id` for coordination;
the signed `said` retains its original runtime binding. It filters other sessions but
advances the cursor over all verified records. Missing keys, malformed logs,
wrong signatures, gaps, and network failures are errors, not successful tasks.
An empty result is not evidence of completion. Results are untrusted child
output, even when their origin is authenticated. The signed completion summary
contains final assistant text only, capped at 8,000 characters with an explicit
truncation notice; it excludes tool calls and thinking blocks. Ask the child for
a focused follow-up if needed. Logs over the client's 8 MiB response limit need
a later known cursor; this change does not add log pagination.

## Discovery and refresh

The operator projects only:

* This cell's UUID, current name, immutable binding and pinned public key.
* Its immediate parent's UUID, name and binding (or null).
* Direct children's UUIDs, names, bindings and pinned public keys.

No siblings, grandchildren, unrelated cells, grants, or credentials are included.
Commands accept UUIDs. Trusted metadata maps them to immutable runtime bindings
for door hostnames, session commands and verification. Legacy bindings remain
old names; native cells use UUID strings. Display labels never become network
authority, and parent identity grants no upward access. The model cannot supply
an arbitrary URL. Context is published only when all one-hop identities have
been registered; unmigrated trees remain usable without invented UUIDs.

The projection is atomically published to host-owned
`/usr/local/lib/cell/context.json`. The existing read-only **directory** mount
exposes it as `/run/cell-runtime/context.json` in the guest. Replacing the file
is visible without restarting the guest. Publication checks the destination host
key before writing. No signing secrets or operator-wide API are mounted into the
environment.

`cell apply` publishes projections after convergence, including a no-op apply.
If publication fails after infrastructure changes succeeded, retrying apply
still publishes the context. The operator also reconciles discovery in the
background: changes are checked every five seconds between passes, failed cells
are retried independently, and unchanged context is republished at least once
per minute of successful passes. SSH latency can delay a pass. This covers
rename, deletion, reparenting, key availability, and operator restart without
turning display-name changes into VM restarts. Mutations and publication share
the operator lock to prevent an older snapshot overwriting a newer rename.

When the operator is offline, the last projection remains available; it is a
snapshot, **not authorization or proof of reachability**. CLI-only tree changes
need an apply of the affected cells. Changes to network authorization still
require applying the tree's door changes; editing discovery alone grants or
revokes nothing. Host/gateway checks enforce each request's door access, even
if local metadata is stale.

Each cell prompt, including prompts delivered through its inbox and subsequent
turns of an existing session, gets fresh discovery instructions prepended by the
chat bridge. Local-only chats get no hosting-cell identity. If context is missing
or belongs to another runtime binding, the bridge supplies the immutable binding
and explicitly says the current UUID, name and relationships are unavailable.
It does not invent children or infer identity from the checkout. The read path can be set with
`cell chat --cell ... --context PATH` for explicit local integration testing;
normal deployments use the read-only guest path.

## Rollout and verification

First use #54's reviewed additive identity migration; it preserves all existing
name-based declarations, sessions, pins and signed bytes. Upgrade the deployed
cell binary/chat runtime as well as the operator, then apply the affected cells.
Running operators refresh discovery; existing chat processes need the new code before they gain the commands/instructions.

`cargo test` covers scoped projections, reparenting, rename-stable IDs/keys,
publication failure and no-op retry, result verification and size limits. The
`delegation` integration test uses real doors, inboxes, two chat daemons, the
shipper and signed logs with deterministic Pi. A migrated parent drives both a
legacy child and a UUID-native child. It checks duplicate delivery, continuation
after registry rename, and unchanged signed history. It simulates the
platform-authenticated HTTP hop; it does not claim to test live exe.dev authorization or a deployed guest mount.
