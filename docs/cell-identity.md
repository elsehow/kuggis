# Cell identity and renaming

A cell declaration has three fields:

```json
{"id":"00000000-0000-4000-8000-000000000001","name":"project","grants":[]}
```

New cells receive a randomly generated UUID v4 at creation preview. The durable
creation record retains that ID through approval, failure, and retry. Loading a
file never generates an ID. Names retain the existing validated lowercase-label
syntax and must be unique within a tree; names are not credentials or identity.

## Stable references

The declaration filename is `<id>.json`. Its optional sibling directory `<id>/`
contains its children. This preserves the directory-based tree representation,
but parent/child references in memory and on the wire use `CellId`, not labels.
Pins are `.pins/<id>.pub`, VM ownership tags are `cell-<id>`, and parent doors are
`door-<id>`. Provider VM names for new cells are also `cell-<id>`.

Sessions, signed statement envelopes, operation targets, and cell API routes
refer to the UUID. The UUID identifies a cell; its pinned public key authenticates
its statements. Resource custody that was already keyed by public key remains so. A door records
the platform-authenticated source **VM name** as provenance; it does not mistake
that external hostname for a cell UUID or display label.

The runtime configuration hash covers **ID and grants**, not the name. Complete
declaration serialization still includes the name for storage and display.
The edge cannot observe display labels; its reconstructed cells use their ID as
a diagnostic label, which does not affect configuration comparisons.

## Rename

Use **Cell settings → Cell name → Rename**, or:

```http
POST /api/cells/<id>/rename
Content-Type: application/json

{"name":"new-name"}
```

This is an operator-side, atomic metadata write. It requires neither a deployment
binary nor a VM restart, and is allowed while conversations are running. It does
not move declaration files, rewrite signed history, change keys or credentials,
reconnect parent doors, or rename descendants. URLs and React workspace keys remain
UUID-based, so the selected conversation stays attached.

Rename uses the same mutation lock as provisioning, grant changes, and deletion.
It refuses unknown IDs, conflicting names, cells with unfinished provisioning,
and active deletion. Pending approvals still detect declaration changes and must
be reviewed again where applicable. Completed operation records remain historical
snapshots; the tree supplies the current name.

`cell tree` prints ID, parent ID, grant count, configuration hash, and name.
Tree-scoped CLI commands can resolve a current name to its ID; persistent API
clients should retain the ID instead. Low-level commands such as `say` and `adopt`
require the ID directly.

## Compatibility boundary

**This is a breaking declaration and wire-format change. Migration is deferred.**
Old name-keyed declarations, tags, pins, sessions, and signed envelopes are not
silently adopted or rewritten. Do not deploy this version over an existing tree
until a separate migration strategy has been reviewed. In particular, modifying
old signed statement bytes would invalidate their signatures.

Standalone `chat --local` uses a fixed local-only UUID, scoped to its state
directory, rather than allocating a provisioned cell. Its UI label is `local`.

## Agent discovery

See [cell delegation](cell-delegation.md) for the one-hop runtime projection and
UUID-addressed child tasks. Runtime labels are refreshed separately from the
configuration hash; renaming does not change task or door identity.
