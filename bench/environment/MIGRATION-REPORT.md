# exe.dev to OVH migration acceptance

2026-10-25. One existing standalone production cell was moved from exe.dev to
an OVH Ubuntu 24.04 dedicated server. Its UUID, display name, declaration and
signing key stayed unchanged. The guest sees 16 CPU threads and a 56 GiB memory
ceiling. The supervisor remains on exe.dev and routes to the host over pinned SSH.

Runtime commit: `074e36ed2975bc5f41a7750fd1c68fcc50174e25`.
Snapshot tooling: `134c516`.
Linux binary SHA-256:
`e5d083f971d0e6f911f5fe850bfa7804762659bf22a206d1a4016fa6d01c0579`.

## Verification

| Check | Result |
| --- | --- |
| Rust regression suite after SSH publishing change | 236 passed, 2 ignored |
| Isolated snapshot round-trip | Contents, modes, mapped owners, hardlinks, symlinks, xattrs, FIFO, sparse files and nanosecond timestamps preserved |
| Hostile archive containment | Absolute paths, parent traversal and earlier symlinks could not change a host sentinel |
| Rehearsal snapshot | All 147,330 filesystem entries matched after restore |
| Rehearsal boot | Real guest setup ran with no external network, signing key or publisher bridge |
| Rehearsal state | Both original chats and all 143 messages matched; credentials, package inventory and workspace survived |
| Final snapshot | Fresh quiesced snapshot; all 147,333 filesystem entries matched before boot |
| Signing history | Every original statement verified; all 23 original signed records retained byte for byte |
| Credentials and software | Saved login and package fingerprints matched before the real-provider test; full filesystem transfer also retained installed non-package software |
| Scoped Git | Private clone/fetch, legacy URL, authenticated dry-run push, absent remote test ref, and denied ungranted repository |
| Publishing | Canary private publish/verify/status/unpublish; existing production private site retained its namespace, URL, version and artifact and passed external verification |
| Real chat | Saved login produced real model replies; incremental response content arrived over SSE |
| Full host reboot | Identity, credentials, packages, workspace, all chats, original log bytes and private site preserved; signed boundary check passed again |
| Supervisor cutover | Existing UUID route, name, key, grants and full original transcripts preserved; all five production cells healthy |
| Supervisor route | SSE relay and provider settings passed; unauthenticated and foreign-origin requests refused |

The real streaming check observed 23 SSE frames, including response progression
from a partial prefix to the complete marker while the run was active. Status
polling intentionally causes the API's recovery path to emit incremental
snapshots. An initial assertion requiring the literal `text_delta` event was
incorrect for that delivery path; the saved frames verified actual incremental
content and final completion. No production streaming implementation changed.
The separate acceptance conversation was archived; original conversations were
never used for test prompts.

Git write acceptance negotiated a fresh branch with `--dry-run` and checked
that it remained absent. It did not exercise remote update hooks or branch
protection by writing a real ref. The existing public-host grants were preserved
independently of Code grants.

## Cutover and recovery

The source was resumed after the rehearsal snapshot. At final cutover, the
supervisor's authoritative state was backed up and routing paused. The source's
guest, signer, door and publishing bridge were stopped and persistently masked
before its signing identity was installed on OVH. A supervisor restart guard
kept old routing unavailable during the transfer. There was one active signer.

The destination canary's complete guest and host state were retained. The
destination kept its host machine identity, SSH key, DNS/resource settings and
root-held Git credentials. Host-state ownership was mapped by service account;
the guest's existing UID range was preserved. Signed history was verified before
setup appended new records.

After functional and reboot acceptance, a signed host plan required no further
setup. Only the exact old VM's cell tag was removed; the VM itself remains
preserved and fenced. The source's supervisor binding was switched to OVH and
the canary registration retired. All original production chat IDs remain; the
total now includes one archived migration test conversation.

Raw snapshots, signing material, deployment bindings, transcripts, detailed
receipts and rollback units remain in private storage. Rollback after new writes
requires returning the latest state and signed-log tail, not blindly restarting
the old snapshot. See the [migration procedure](../../deploy/migration.md).

This verifies a standalone cell on the tested Ubuntu/architecture/UID mapping.
Cross-provider child doors, exe.dev integration grants, automatic server
provisioning/deletion, and a general migration CLI remain separate work.
