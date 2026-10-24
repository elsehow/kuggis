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
cell publish ./dist --site news-viewer --visibility public
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

## Private sites and automatic cell provisioning

Private publishing extends the existing managed Site resource. It uses the same
resource ID, owner, attachments, retention policy, immutable artifacts and version
history. A cell rename or move does not change its namespace or website URLs.
This supports static HTML/CSS/JavaScript and data, including modules and local
JSON fetch. Hosting arbitrary application servers is not implemented.

### Configure hosting once

Use a dedicated publisher VM, separate from agents and the operator UI. Native
exe.xyz hosting needs no custom domain or external DNS. Configure a port range:

```json
{"domain":"YOUR-PUBLISHER-VM.exe.xyz","ports":{"first":4000,"last":4999},"users":["STABLE_READER_ID"]}
```

Each Site receives a permanent URL such as `https://YOUR-PUBLISHER-VM.exe.xyz:4000/`.
exe.dev provides TLS and login on these ports. Alternate ports remain private
when the default port is public for the signed control API. Never select a site
port as the default public share port. The private reader binds the entire
configured range on the `--listen` IP; its single port is ignored in this mode.
All listeners must bind successfully. Ports are never recycled, including after
unpublish. Exhaustion refuses new sites without changing existing publications.
The range is limited to 1000 ports within 3000–9998; reserve 9999 for platform use.

Port origins isolate DOM, fetch and localStorage, but **cookies share a hostname
across ports**. Use localStorage for site state. Gateway sessions must use
HttpOnly cookies. Origin-Agent-Cluster and document-domain restrictions prevent
legacy origin relaxation. For distinct cookie scopes, omit `ports` and configure
wildcard DNS/TLS and gateway routing for `*.sites-private.example.com` instead.
The platform does not create DNS records, gateway accounts or VMs automatically.

For external verification through exe.dev's private ingress, generate a
VM-scoped HTTP token for this publisher only (see exe.dev HTTP token docs).
Store `X-Exedev-Authorization: Bearer TOKEN` in a private regular file owned by
`cell-publisher`, mode 0600, under a root-owned directory. Add
`Environment=CELL_PUBLISH_INGRESS_HEADERS=/etc/cell-publisher/ingress.headers`
to the publisher service. This token stays on the trusted publisher, never in
cells, receipts, registry state or discovery. It is sent only with authenticated
private verification requests, never anonymous checks; redirects are not followed.
On exe.dev, the VM's `/etc/hosts` maps its own name to an internal address.
Create `/etc/cell-publisher/verify-hosts` as a root-owned 0644 file containing
only `127.0.0.1 localhost` and `::1 localhost`, each on its own line. Install
`exe-verifier.conf` as `/etc/systemd/system/cell-publisher.service.d/exe.conf`,
then reload systemd and restart `cell-publisher`. This overrides `/etc/hosts`
only inside the verifier service, so checks use public DNS and the real HTTPS
auth gateway without changing the VM's own hostname mapping or pinning IPs.

Manage expiry/rotation or explicitly choose a non-expiring VM-scoped credential.
Revocation affects verification, not readers' ordinary exe.dev login.

The gateway must overwrite/remove client-supplied `X-ExeDev-UserID`, assert the
stable signed-in user ID, and preserve the original host. Sessions should use
Secure, HttpOnly cookies; no credentials are sent into cells. Anonymous requests
may reach the reader (which returns 403), or go through gateway sign-in. Requests
bearing `X-Cell-Site-Verification` must reach the reader: it validates the signed,
site-scoped, expiring proof and current grants itself. A header's presence is
**not** authentication. Keep the reader loopback-only and agents off this host.

Create an operator-owned configuration, containing reader IDs, not secrets:

```json
{"domain":"sites-private.example.com","users":["STABLE_READER_ID"]}
```

Then configure the existing registry and install the private reader:

```sh
cell publisher configure-private /var/lib/cell-publisher /etc/cell-publisher-private.json
# Install cell-private.service and adapt nginx-private.conf to your gateway/domain.
sudo systemctl enable --now cell-private
```

The private domain and port range cannot change after publishing; reader membership can change.
Public and private versions cannot share a site identity: use a different site
name to change audience. This avoids silently disclosing a formerly private URL
or changing an existing public URL. Private sites use separate browser origins,
no cross-origin read permission, and no service workers. The public reader never
returns private artifacts, even to an authenticated user.

### Connect the operator

Add `publishing` to the existing backend in the operator tree's private
`.resources.json`:

```json
{
  "backends": [{
    "name": "sites",
    "provider": "PUBLISHER_AUDIENCE_ID",
    "location": {"kind": "ssh", "vm": "PUBLISHER_VM"},
    "publishing": "https://publisher.example.com"
  }]
}
```

SSH resource commands run as `cell-publisher` through sudo, matching the service
user and preserving registry file ownership.

This opts into reconciliation of the publisher's **complete** policy from this
operator's validated tree and verified pins. Do not enable it for a publisher
whose policy belongs to another operator or a larger tree. One automatic
publisher per operator is supported; other resource backends remain available
for lifecycle management. Existing manual publishing configurations still work
without opting in.

Enable **Private websites** in a cell's settings (and its ancestors). The grant
is `{"publish":"private"}`. Children may inherit this capability, but receive
their own namespace, not access to their parent's or siblings' sites. The form
also includes the publisher hostname in the reviewed network grants; normal
parent containment still applies. Public publishing requires a separate existing
namespace/site grant and the explicit CLI flag `--visibility public`.

On tree changes and at least once per minute, the operator allocates stable
namespaces for permitted pinned keys, synchronizes policy, and installs the
bridge configuration on the cell host. It does not restart chats. Resource
transfers and explicit detachments are preserved. Removed grants stop subsequent
publication and verification. Host context advertises the bridge and the cell's
accessible Site resources, including their canonical URLs and last verification.
`cell publish capabilities` reads this discovery without needing the chat URL.

Upgrade the operator first (it accepts inventories from older publishers), then
the publisher and cell runtime, before enabling the new grant. Restart all
publisher processes (control, public reader and private reader) on the new binary
before configuring private hosting. Back up the
registry as usual. Older clients can continue explicitly authorized public
publishing; existing public URLs and historical receipts retain their meaning.
New private fields require the new publisher binary; restore the matching backup
if rolling the publisher back. Cells predating the `/run/cell-publish` bind need
an ordinary runtime apply before their bridge becomes available.

### Publish and verify

```sh
cell publish capabilities
cell publish ./dist --site pilot             # private by default
cell publish verify pilot
cell publish status pilot
cell publish ./public-dist --site homepage --visibility public
# Manage a previously granted site outside the default namespace:
cell publish status homepage --namespace NAMESPACE_ID
```

Publish and rollback return the complete URL and perform an external HTTPS check
from the publisher. Verification compares the exact returned URL and every
uploaded file to the immutable artifact. It follows no redirects, rejects login
pages even when they return HTTP 200, and checks anonymous denial for every
private URL both before and after the authenticated response. A fresh signed Verify request serves as a five-minute, site-scoped
read proof; current grants are checked again on each private file request.
It cannot authorize publication or change reader membership. Cells need network
access only to the publisher control endpoint.

A failed check leaves the durable publication in place, reports its receipt and
the failure, and exits nonzero. Repair hosting and run `cell publish verify`;
do not republish just to retry a check. The resource view shows the canonical
website link, private/public audience, and the latest verification result/time.
A new version invalidates the previous version's check. These checks establish
served content and access boundaries, **not** a browser-render or user sign-in
test. The agent instructions explicitly distinguish those claims and prohibit
sharing guessed relative URLs or workspace paths as websites.

Validation (no production state is touched):

```sh
cargo test --manifest-path cell/Cargo.toml --locked
# Linux: real CLI, signed requests, local TLS and private ingress; no sudo:
python3 scripts/test-private-publishing.py
KUGGIS_TEST_NATIVE_PORTS=1 python3 scripts/test-private-publishing.py
# Browser origin/CSP contract using a local authenticated-gateway fixture:
KUGGIS_TEST_PLAYWRIGHT=/path/to/playwright/index.mjs \
KUGGIS_TEST_BROWSER=/path/to/chromium \
  cargo test --manifest-path cell/Cargo.toml --test private_publish \
  private_sites_in_a_browser -- --ignored --nocapture
```

The local fixtures do not verify production DNS, TLS or the gateway's real login
flow. Exercise those with an authorized browser before calling a live deployment
verified.
