# Moving an existing cell to an SSH host

Registration creates a new identity. It must not be used to replace an existing
cell. A migration keeps the cell UUID, binding, public/private signing key,
declaration, signed history, chats, workspace, credentials, installed packages,
and publisher namespace. The destination keeps its own SSH host key, machine
identity, DNS/resource configuration, and repository credential store.

`migration_snapshot.py` supplies the offline filesystem part. It does not
authorize a cutover, transfer the signing key, modify supervisor bindings, or
start/stop services. Keep all migration artifacts in private storage: the guest
archive contains credentials and chat history.

## Snapshot boundary

Never run guest programs or traverse the mutable guest filesystem as host root.
The tool constructs a fresh helper from trusted host Python, tar, gzip, and their
libraries. It enters private user, mount, PID, and network namespaces with a
read-only helper root. The stopped guest is a separate payload mount. The helper
has no host signing key, network connection, or host-root mount.

Capture refuses a running guest. It hashes an inventory before and after the
archive and requires equality. Restore checks the supplied archive digest and
extracts into a new, isolated staging directory. It verifies all persistent
paths, contents, modes, UID/GID, nanosecond modification times, hardlinks,
symlinks, and extended attributes. GNU tar also preserves ACLs and sparse files.
Unmapped owners and unsupported persistent special files fail closed.

The contents of `/dev`, `/proc`, `/sys`, `/run`, and the three host-bound
inbox/outbox/signed-log directories are omitted. Their mountpoint directories
remain. Transfer host authority and signed state separately through the trusted
operator path. The guest UID range must match on both hosts.

```sh
# Source, after checking idle chats and stopping its guest and signing services:
sudo python3 migration_snapshot.py capture \
  --work /var/lib/kuggis-migration/source-snapshot \
  --machine-id SOURCE_MACHINE_ID --key SOURCE_CELL_PUBLIC_KEY

# Transfer guest.tar.gz, manifest.json and receipt.json over pinned SSH.
# Destination staging; the current destination cell is unchanged:
sudo python3 migration_snapshot.py restore \
  --work /var/lib/kuggis-migration/restored-snapshot \
  --machine-id DESTINATION_MACHINE_ID --key CURRENT_DESTINATION_CELL_PUBLIC_KEY \
  --archive /private/guest.tar.gz --manifest /private/manifest.json \
  --archive-sha256 HASH_FROM_SOURCE_RECEIPT
```

The Linux acceptance script exercises a real namespace round-trip and hostile
archive paths/symlinks without modifying the live cell:

```sh
sudo python3 bench/environment/migration_snapshot_acceptance.py \
  /trusted/path/deploy/migration_snapshot.py
```

## Cutover and rollback requirements

1. Verify both host incarnations, SSH pins, source signing key, authoritative
   declaration, idle chats, and absence of parent/child doors. Snapshot the
   supervisor's tree and operator state, including the destination canary.
2. Rehearse restoration and boot using a separate guest with no network,
   signing service, publisher bridge, or production supervisor route. Compare
   saved chats, credentials and installed software. Resume the source during
   rehearsal and take a fresh final snapshot at cutover.
3. Pause supervisor routing during the switch. Stop and persistently fence the
   source guest, signer, door, and publishing bridge. Preserve their original
   units and enablement so rollback is explicit. A reboot must not resurrect
   the old active identity.
4. Back up the destination canary, then install verified source guest and host
   state. Preserve destination Git credentials and host runtime configuration.
   Map host-side service ownership by account name; source numeric host UIDs
   are not portable. Verify every original signed record under the same pinned
   key and retain its exact bytes before allowing new statements.
5. Set up the reviewed binary, verify its signed boundary check and real chat,
   Git, and publishing paths. Keep the existing publisher namespace/site/version.
   Remove only the old VM's exact cell ownership tag, retaining the fenced VM.
   Replace only the source's VM binding with its pinned external-host binding;
   retire the canary registration. Keep the source UUID, display name and key.
6. Resume the supervisor and verify original URLs, chat histories, grants,
   healthy unaffected cells, and a destination reboot. Retain private receipts
   and the source VM until the migration is accepted.

Never run both copies as active signers. Even before users resume, destination
setup/checks append signed statements: rollback must preserve that new log tail
before restarting the source, or sequence numbers would fork. After user writes
resume, a rollback is a reverse migration of the latest guest and host state,
not restoration of the old snapshot. A failure with uncertain remote state
requires inspection and a fresh reviewed action, not an automatic retry.

## Separating development from production

Preserving a cell's credentials is appropriate when moving the same trusted
identity to a different host. Moving a production workload away from a cell
that will continue running development agents is a different operation:
the development guest must not retain production credentials or private data.
Guest root can read all guest-local files regardless of their user ownership.

Before a workload transfer, record an explicit manifest of the data and
configuration paths being moved, their intended identities, and the paths
retained for rollback. Use separate development and production credentials.
Do not copy a provider refresh token from another cell merely to skip sign-in.
Keep deployment SSH identities and credential-bearing state out of development
checkouts and source archives.

Keep rollback archives and retired runtime trees in a host-owned, mode-0700
directory outside every guest rootfs and bind mount. Preserve file contents
and metadata for rollback; an inactive guest service or a disabled wrapper is
not an access restriction. Release-retention jobs that preserve state and
home directories do not sanitize those directories.

Migration acceptance must record these checks before the source resumes
development:

1. Confirm the destination has the required data and its own production
   credentials. Preserve the existing deployment and rollback workflow.
2. Obtain approval for any development interruption. Stop the source guest
   before removing access so old processes cannot retain open handles. Check
   for remaining guest processes, nested mounts and external hardlinks.
3. Quarantine the source manifest's sensitive paths outside the guest. Include
   import archives, temporary transfer copies, and credential configuration;
   moving just the live configuration misses duplicates. Do not automatically
   delete the retained copy or move the whole guest home.
4. After restarting development, verify the live UID mapping, original path
   absence, quarantine ownership/modes, and guest mount roots. Save metadata
   receipts without credential values or application data.
5. Treat previously guest-readable credentials as potentially exposed. Record
   validity and reuse as unknown until provider-side metadata is available.
   When replacement is authorized, provision distinct production credentials,
   validate the authorized production workflow, then revoke the old credentials.
   Removing a local file does not revoke a credential or erase prior copies.

For retired whole-cell migrations, keep the old source fenced as described
above. Never resume it as a development environment while it retains the
production identity or its credentials.
