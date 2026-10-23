# Stable cell identities and compatible migration

The operator gives a cell a permanent UUID and a mutable display name. A pinned
public key still authenticates the cell. A UUID is an address, not an access grant.

## Compatibility boundary

Legacy names already occur in declarations, VM tags, integration names, pins,
configuration hashes, session ownership, request receipts and signed statements.
Those bytes remain unchanged. The private tree file `.identities.json` (version
1) maps a UUID and display name to the **immutable runtime binding**, pinned key,
and exact VM name/creation timestamp. Existing runtimes keep their old binding;
renaming only changes the display name. This is an additive migration, not the
breaking declaration/wire-format replacement originally proposed in #54.

Once migration is enabled, operator-created cells receive their UUID at creation
preview and use it as their runtime binding from the beginning. Retry retains the
same UUID. A durable creation record keeps the requested display name until setup
has established the key and VM identity and the registry can enroll it. Existing
name-based declarations remain readable, including after a supervisor rollback.

The runtime protocol stays compatible: signed statements and session commands
carry the immutable binding. For new cells that binding is the UUID. For legacy
cells it is the old name. Never replace that field with a current display name,
rewrite signed history, or infer an identity from a checkout name.

## Operator migration

Choose **Enable cell renaming…** in the Cells sidebar. Review the names, VMs and
assigned UUIDs, then choose **Assign permanent IDs**. Every remote cell must have
a uniquely tagged VM, a creation timestamp, and an existing pin matching the
host's public key. The host's signed statement log must verify under that pin.
Unprovisioned cells and unfinished setup/deletion must be resolved first. Local
socket mode can migrate pinned local cells without pretending they have a VM.

The equivalent authenticated operator routes are:

- `POST /api/identity/plan` creates a saved, versioned plan with allocated UUIDs.
- `GET /api/identity/migrations/ID` retrieves that exact plan after a disconnect.
- `POST /api/identity/migrations/ID/apply` with `{"approval":"HASH"}` rechecks the
  complete declaration/parent inventory, pins, VM incarnations and registry.

Planning does not alter declarations or allocate remote resources. Applying
serializes with operator mutations, writes the previous registry and approved
plan into `.identity-migrations/`, then atomically replaces `.identities.json`
with file and directory synchronization. That single replacement is the commit
point. Repeating the same request after a lost reply or process interruption
preserves the UUIDs and original backup. A changed tree, pin, VM or registry
requires another review. No chat process is stopped, and no prompt is replayed.

Old unstarted creation/deletion/configuration approvals require a new review.
Completed receipts remain history. Migration refuses unfinished destructive jobs
instead of changing the meaning of a partially completed operation. Resource
plans already bind pinned keys and revisions; public-key custody is unchanged.

## Rename and aliases

Use **Cell settings → Cell name → Rename**, or
`POST /api/cells/UUID/rename` with `{"name":"new-name"}`. Rename is serialized
with cell changes and refuses unfinished setup/deletion, duplicate labels and
reserved UUIDs. Previous display names may be reused by other cells; legacy
runtime aliases do not reserve display names. It changes no runtime configuration,
grant, key, workspace, conversation, resource custody or parent/child relationship.

The Cells API supplies `id`, `name`, immutable `binding` and the parent's route ID.
Unmigrated cells temporarily use their legacy route as `id` and cannot be renamed.
HTTP routes accept UUIDs and frozen legacy aliases, **not mutable labels**. The
browser redirects old cell links to UUID links without dropping the selected
chat; a mounted legacy transport can continue through its frozen alias. Model
preferences and unread state keep their original binding-based storage keys.

UUID and legacy routes reject changed pins and remote VM incarnations. The relay
also checks the host's public key before connecting. Deleted identities remain
reserved records, so removing and recreating an old declaration cannot silently
inherit its UUID or old links. Explicit key rotation/adoption is not implemented;
changed bindings fail closed and require a separately reviewed repair.

Tree-scoped CLI commands (`plan`, `apply`, `brief`, `pull`, `login`) resolve current
labels or UUIDs through the registry and check pins. Low-level signing and door
protocols continue to use runtime bindings. Legacy CLI-created cells can be
registered in a subsequent migration. If a CLI name matches one cell’s current
label and another cell’s legacy binding, the command refuses it and asks for a
UUID. HTTP legacy routes remain fixed to their original cell, and the UI uses
UUIDs for current links. The operator never generates identities
implicitly while reading files.

## Rollout and rollback

Test the plan/apply/retry and rename flows against a disposable parent/child tree
first, then migrate a quiet real tree after the compatible supervisor is deployed.
Back up both tree and operator state with the service stopped before production
rollout. This release does not retag VMs or rewrite cell state, so existing chat
runtimes need no cutover restart. Keep the compatible supervisor for rollback;
older supervisors can still read runtime declarations but do not understand
friendly labels or enforce the registry's reserved-alias rules.

Do not restore a stale tree or operator snapshot after new writes. Repair forward
using the saved plan/registry receipt. There is no automatic downgrade or resource
backend migration. No live deployment or VM migration is implied by test results.

## Agent discovery

See [cell delegation](cell-delegation.md) for the one-hop projection and
UUID-addressed child tasks. Trusted context retains each child's frozen runtime
binding and pinned key. Door requests and signed results use that binding;
display labels only affect discovery metadata.
