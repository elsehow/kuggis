# Static publishing deployment

This is an operator-run recipe for the publisher VM; `cell setup` installs the
signer on each cell. It has been exercised end to end on exe.dev: a dedicated
publisher VM, a cell whose agent publishes from inside its environment, public
HTTPS, and a browser. No account credentials are installed in cells.

## Publisher VM (separate from every agent/private door)

1. Provision a dedicated exe.dev VM using the operator's account. Install a
   Linux `cell` binary at `/usr/local/bin/cell`, root-owned and executable.
   Install nginx, and create an unprivileged `cell-publisher` service user.
2. Initialize state as that user. The parent must allow it to create the
   directory; for example create `/var/lib/cell-publisher-state`, owned by
   `cell-publisher`, run `cell publisher init` on a new child directory, then
   move that child to `/var/lib/cell-publisher` as root. Alternatively initialize
   `/var/lib/cell-publisher` as root and chown it recursively to the service user.
   **Do not pre-create the final registry directory**: init deliberately refuses
   existing directories. Record the printed audience ID.
3. Configure the public URL:

   ```sh
   cell publisher configure /var/lib/cell-publisher --public-base https://YOUR-PUBLISHER-HOST
   ```

   This must be the actual HTTPS origin; URLs cannot change after first publish.
4. Install `cell-publisher.service` and `cell-public.service`. Keep the registry
   private (0700 directories, 0600 files). Both units use the service user; the
   public unit's mount namespace is read-only except for the coordination lock.
   Migrate any older registry with an operator `cell publisher plan` before
   starting the read-only public unit.
5. Install/review `nginx.conf` for this **dedicated VM only**, for example as
   `/etc/nginx/sites-available/cell-publisher`, enabled, with the default site
   removed. Point exe.dev's HTTPS proxy at it and make it public, naming the
   publisher VM and nothing else:

   ```sh
   ssh exe.dev share port PUBLISHER_VM 8000
   ssh exe.dev share set-public PUBLISHER_VM
   ```

   exe.dev's proxy reaches nginx's loopback listener. Never make an existing
   cell's private door public.
6. Run `nginx -t`, reload nginx, and enable/start the two systemd units. Confirm
   `/state.json` returns 404, the unsigned `/v1/requests` API refuses requests,
   and no filesystem/log directory is exposed.

The control API and public reader are separate listeners/processes; nginx may
route them through one HTTPS origin because control operations require signed
requests, not cookies. Public HTML is sandboxed to an opaque browser origin.

## Authorize a cell

On the operator side, obtain the cell's **existing verified pin**, not an
agent-supplied key. On the publisher allocate a namespace for that pin:

```sh
cell publisher namespace /var/lib/cell-publisher PINNED_CELL_KEY
```

Add `{"publish":{"namespace":"NAMESPACE_ID"}}` to the cell declaration (and
ancestors as required by tree containment). Also grant the outbound publisher
hostname with `{"host":"YOUR-PUBLISHER-HOST"}`; publishing authority does not
silently add network authority. Apply the ordinary cell plan so the host-owned
`/etc/cell/cell.json` and proxy allowlist receive the reviewed grants.

Copy the complete reviewed tree and its verified `.pins` to a private operator
location on the publisher (do not copy secrets). Preview and approve:

```sh
cell publisher plan /var/lib/cell-publisher /private/operator-tree
cell publisher apply /var/lib/cell-publisher /private/operator-tree --approve HASH
```

Apply replaces the **entire** publishing policy. Do not supply a subtree to make
an incremental change. Keys, topology, grants, origin, registry identity, and
prior policy are bound by the approval hash. Revocation requires updating this
policy too; a stale local signer cannot override revoked publisher authority.

## Cell bridge

`cell setup` installs `cell-publish-bridge.service` on every cell VM. It stays
inactive until the operator creates the host's root-owned
`/etc/cell/publisher.json` (0644):

```json
{
  "socket": "/run/cell-publish/bridge.sock",
  "namespace": "NAMESPACE_ID",
  "audience": "PUBLISHER_AUDIENCE_ID",
  "endpoint": "https://YOUR-PUBLISHER-HOST"
}
```

Keep the socket at `/run/cell-publish/bridge.sock`: setup binds that directory
read-only into the cell environment at the same path. The namespace, audience
and endpoint are public configuration, not credentials. The bridge copies them
to `/run/cell-publish/publisher.json`, where the agent's `cell publish` reads
them. Start it with `sudo systemctl restart cell-publish-bridge`, or apply the
cell's plan again; apply also checks that the agent can reach the bridge from
inside its environment. A cell set up before this bind existed needs one apply,
which restarts its environment.

The bridge runs as `cell-say`, which already holds `/etc/cell/secret`; keep that
key private and do not copy it. It has no IP networking, reads only host-owned
config/declarations/key, and accepts only the environment's `agent` over its
Unix socket, identified by its UID as mapped on the host (the environment's UID
base plus 1000, not the host's own `agent` user). Root inside the environment is
refused, so run `cell publish` as `agent`. The bridge signs typed, locally
authorized commands and fixed metadata, never arbitrary bytes. Its directory is
preserved across restarts, and a restart replaces the previous socket only when
nothing answers on it.

The agent can now run:

```sh
cell publish ./dist --site news-viewer
cell publish status news-viewer
cell publish rollback news-viewer VERSION_ID
cell publish unpublish news-viewer
```

Each publish returns JSON with a stable URL and version ID. Additional names
create additional sites within the approved namespace. A site delegate can
update its existing site, but cannot reserve new names. Files are read only by
the agent-side packer, not by the privileged signer.

## Limits and operational semantics

- Static HTML/CSS/JS/images/data only. Bundle libraries; external CDN scripts,
  frames, forms, cookies, service workers, and same-origin storage are blocked
  by the public sandbox. ES modules and local JSON fetch work via anonymous CORS.
- Use relative asset URLs (for example Vite `base: "./"`); sites live below a
  namespace/name path, not at the host root.
- Every artifact needs `index.html`. Names are non-hidden relative ASCII paths;
  symlinks, special files, traversal, duplicate paths and file/directory conflicts
  are rejected. Content can still contain sensitive data: publishing is an
  explicit disclosure grant, not a secret scanner.
- 512 files / 5 MiB content per upload; 64 sites per namespace; 32 retained
  versions per site; 256 MiB total encoded artifact storage. JSON byte-array
  encoding uses more disk/wire space than raw content. Quota exhaustion fails
  without changing the live version. Limits are deliberately conservative.
- Rollback selects an existing version of the **same site**. Unpublish clears
  only the live pointer, never ownership or reservations. It cannot erase copies
  already downloaded by readers.
- Receipts/version history are durable in the publisher registry and returned
  over HTTPS; they are **not yet integrated into the cell's signed statement
  log**. Preserve the private registry and artifact directory together in backups.
- Failed/expired/replayed requests cannot change a live site. If a response is
  lost, use status to inspect the result; repeating `cell publish` creates a new
  version. Atomic updates ensure readers see complete old or new snapshots.
- A crash before commit can leave an unreferenced blob. There is no automatic GC
  or version pruning yet; do not manually remove a blob referenced by history.
- The bridge reloads its declaration per request; publisher policy is checked
  again under the same lock as the publication commit. To stop all publication,
  revoke publisher grants, not merely the local bridge configuration.

## Practical smoke test

```sh
cargo build --manifest-path cell/Cargo.toml --locked
python3 scripts/test-publishing-live.py
# Optionally exercise a locally installed Playwright/Chromium too:
PUBLISH_SMOKE_PLAYWRIGHT=/path/to/node_modules/playwright python3 scripts/test-publishing-live.py
```

The script requires an unprivileged user with passwordless sudo, curl, and
openssl. It uses disposable root-owned keys/services under `/run`, local HTTPS
and temporary state, then cleans them up. It never contacts exe.dev or publishes
anything to the Internet. Browser coverage checks module loading, JSON fetch,
and opaque-origin isolation in addition to the complete CLI lifecycle.
