# Duplicate a legacy workspace

Migration is an operator recipe, outside the Rust core. Create a fresh v2 host,
copy selected guest data, configure its resources, then verify it alongside v1.
The v2 installer deliberately refuses a host containing `/etc/cell`; do not
remove that guard to install over a running legacy cell.

To keep the duplicate on the same physical server, provision a dedicated Linux
VM there and use it as the v2 SSH host. This preserves the one-workspace-per-host
layout without sharing the legacy host's firewall or container configuration.
Declare the guest's `memory_bytes` limit and leave room for the VM's operating
system. CPU and RAM capacity remain shared with the original workspace while
both run. A standalone SSH alias can reach the VM through the physical host;
an SSH local forward can expose `private_web` on the operator's loopback address.

## Inventory first

Read the supervisor's current cell registry and SSH bindings before choosing a
source. An old exe VM may still exist after a cell moved to another provider.
Inspect the active guest, not a retained host-side `/home/agent` backup. Execute
guest file operations through `systemd-run --machine=cell-env`; never traverse
the mutable rootfs from host root.

Record the workspace path, Git HEAD and dirty/untracked status, native Pi
transcripts, custom tools, package requirements, local data, service definitions,
ports, resource endpoints, and integration scopes. Inspect credential *presence*
without printing values. Follow the project's own data and execution boundaries.

## Declare the copy

Keep real declarations, inventories, receipts and archives in ignored `.kuggis/`
or another private operator directory. A selection is ordinary JSON:

```json
{
  "version": 1,
  "roots": [
    "work/example/project",
    ".kuggis/chat/pi",
    ".kuggis/chat/threads",
    ".cargo",
    ".rustup"
  ],
  "exclude": ["*/__pycache__", "*/.pytest_cache"]
}
```

Choose roots from the inventory. Do not assume an entire home directory is
project data. Keep the current repository's `.git`, dirty tracked files and
untracked files; a fresh clone alone loses pending work. Include required custom
toolchains and synthetic app data. Build caches can be copied for convenience or
excluded and rebuilt with the pinned tools.

Do not copy host signing/SSH keys, supervisor state, v1 control-plane services,
inbox jobs, refresh-token stores, or scheduled jobs into an active v2 runtime.
Provision model, repository and deployment credentials through the new
deployment environment. A copied OAuth refresh token can conflict with the
still-running original. Existing scoped platform integrations can be attached to
the new VM without changing the original's attachment.

## Export and restore

`snapshot.py` must run as UID 1000 inside a container. Export reads the selected
paths under `/home/agent` and streams a gzip tar archive to stdout. It includes
a versioned file manifest with SHA-256 digests, permissions and symbolic links.
Hardlinks become independent regular files. It checks source membership and file
metadata again before completing; changing input fails the export. Keep partial
output unpublished on any command/SSH failure. Protect archives as private data.

```sh
# Inside the source guest; SELECTION_JSON is the JSON object above.
python3 snapshot.py export "$SELECTION_JSON" > snapshot.tar.gz
```

Prefer streaming stdout through operator SSH into a private local file, with
stderr captured separately. No source service needs to stop for an idle
workspace copy. This is a filesystem export, not an application-aware database
backup: use the database's own consistent backup for a live database, or arrange
a stopped final copy during cutover. A copy can become stale immediately after
export; it does not synchronize future edits.

Use `kuggis put` to upload the archive and helper to the new guest. For a large
copy while the guest is bootstrapping, an operator can instead stage the archive
as a root-owned mode-0600 host file, verify its SHA-256 against the local receipt,
and pipe its bytes through `systemd-run --machine=... --uid=agent --pipe` into
`snapshot.py restore - STAGING`. Read the inherited stream directly; reopening
`/dev/stdin` can fail when its descriptor was opened in another user namespace.
Never unpack that archive on the host.
Restore into
a **new** staging directory before activating any files:

```sh
python3 snapshot.py restore snapshot.tar.gz /home/agent/.kuggis-v2-restore-project
```

Restore rejects traversal, duplicate members, symlink ancestors, special files,
missing manifests and mismatched hashes. It never overwrites an existing staging
directory. Compare its manifest digest with the export receipt. A failed stage
remains inspectable; do not activate it. The helper is guest code and must never
be run as host root. This copy preserves regular files, symlinks and permission
bits and modification times; it does not preserve hardlink identity, extended
attributes or ACLs.

Activate only verified roots into the fresh guest. Preserve original absolute
paths where venvs, app configuration and conversations use them. A
`/home/agent/workspace` symlink to the original repository path gives Kuggis's
entry and put/get commands the same project root. Keep staging uploads outside
that symlink before activating it.

## Conversations and services

Legacy v1 already stores native Pi JSONL under `~/.kuggis/chat/pi`. Install the
pinned Pi recipe and use a launch argv such as:

```json
[
  "/home/agent/.local/bin/pi", "--offline",
  "--session-dir", "/home/agent/.kuggis/chat/pi",
  "--continue", "--model", "exe/gpt-6.1-sol"
]
```

Use Pi's native `--name` option or the rename control in `/resume` to carry over
the titles from v1 thread metadata. Do this before starting the interactive
terminal, with no other writer to those copied sessions. Keep thread metadata
as an archive; v2 does not recreate the old chat sidebar, archive flags, signed
log, inbox or parent/child orchestration. Verify that Pi
can select the copied conversations and continue one without replaying a prompt.
Original instruction history remains historical context; update any obsolete
workflow guidance explicitly when beginning new work.

Recreate required app services declaratively inside the guest. Map their ports
with `private_web`, and verify private ingress. An app that deliberately accepts
only loopback listeners can retain that restriction: configure a guest
`systemd-socket-proxyd` socket on the private guest interface, forwarding to its
loopback port. Check its unit ordering through a guest restart. Do not auto-enable copied cron,
timers, deployment workers or production consumers. A development duplicate
can deploy to the same external destination using explicitly configured SSH
authority; duplicating production is a separate task.

## Verification and cutover

Compare manifest digests, Git HEAD and dirty/untracked status, transcript counts,
toolchain versions and application health. Run the project's bounded checks and
report existing failures separately from migration failures. Verify Git access
without pushing, model tool execution in a disposable directory, private web
access, detach/reconnect and `kuggis check`.

Keep originals running while trying the duplicate. Pick one workspace as the
editing source for each task to avoid divergent uncommitted work. For cutover,
reconcile edits, take a final consistent copy, transfer service ownership and
only then stop the old runtime. Retain recovery archives before removing v1.
Flatten former child cells into separate declarations; replace their door/inbox
workflow with explicit external resources only where still needed.
