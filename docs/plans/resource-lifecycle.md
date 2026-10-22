# Resource lifecycle refactor

Status: approved direction; implement and merge the PRs below in order.
Prerequisite: static publishing PR #40. Each implementation PR must include its
own review notes, tests, migration story, and explicit remaining limitations.
Self-review is not a substitute for independent review; report it accurately.

## Goal

A cell is a worker, not the lifetime of everything it uses. Sites, compute,
volumes, and jobs are resources. Kuggis must know what a cell owns, what it merely
uses, and what can actually be cleaned up when the cell is deleted. Sites are
the first managed adapter. Externally created resources remain usable without
pretending Kuggis can provision or destroy them.

The first delivered milestone is safe site lifecycle management with an
operator Resources view and resource-aware Delete cell flow. We are not building
RunPod/compute integrations, universal billing, or automatic discovery here.

## Invariants

1. Identity, ownership, access, management, and visibility are distinct.
2. Resource and provider identities are stable. Provider references identify an
   account/registry, not merely a vendor name. Display names never prove identity.
3. Ownership transfer preserves resource ID and external address/URL. The old
   owner's broad namespace grant must not continue to control a transferred site.
4. Ownership alone does not grant runtime access. Delegation requires explicit
   approved access. A cell's reusable name cannot inherit a deleted cell's assets.
5. Trusted infrastructure records resources. Agent assertions do not establish
   ownership, capabilities, successful provisioning, or cleanup completion.
6. Credentials are opaque references, not tokens embedded in records, plans,
   logs, or UI responses. Capability claims come from configured adapters.
7. Deleting a delegate only removes its access. Deleting an owner follows an
   approved lifecycle plan; it must not silently orphan public/billable resources.
8. Stopping effects and destroying data are different operations. Plans disclose
   public exposure, retention, continuing cost, and manual obligations.
9. Plans bind exact identity and revisions, including the complete affected
   inventory. New resources or changed ownership invalidate an old approval.
10. Freeze the departing identity before cleanup. Stale grants or already-signed
    requests cannot reprovision/re-publish after cleanup or deletion.
11. Cleanup operations are durable and idempotent. Unknown outcomes are not
    failures safe to repeat blindly. VM deletion waits for required cleanup.
12. No distributed atomicity is promised. Each adapter records its own operation;
    the deletion workflow tracks partial progress and retries without repeating
    destructive steps already acknowledged by that adapter.
13. Retained resources remain manageable without the original cell. Default site
    cleanup unpublishes and moves custody to the operator; IDs/history survive.
14. External resources are not destroyed by cell deletion. Local detachment does
    not prove provider-side credential revocation. Manual work is explicit.
15. Backend enforcement is authoritative. CLI and UI use the same planning and
    application rules. Browser closure does not discard approved jobs; operator
    service interruption leaves durable state that can be inspected/retried.

## Architecture

### Shared core

Introduce parsed ResourceId, ProviderRef, ExternalResourceId, CellIdentity, and
CredentialRef types, plus ResourceOwner, ResourceManagement, ResourceKind,
AccessScope, ResourceAttachment, ResourceRecord, and revision-bound lifecycle
plans/receipts. Use existing pinned cell keys as incarnation identities initially;
key rotation or ownership transfer must be explicit, not inferred from names.

Managed/external is independent of owner. The operator can own managed resources;
a cell can use an operator-owned external resource. A record does not create an
access grant. Keep adapter-specific scope/operation semantics typed.

The shared layer owns identity, ownership/access metadata, cleanup intentions,
approval binding, and durable operations. Adapters own observation and concrete
side effects. Support observe, plan, execute, and reconcile; unsupported actions
are explicit, never success-shaped no-ops.

### Site adapter and persistence

Keep the working artifact packer, restricted signer, signed-request transport,
replay protection, immutable artifact store, and public reader. Link existing
SiteId addresses to stable ResourceIds; never rename public URLs during migration.
The resource record becomes authoritative for ownership/access. Avoid two owners
that can drift between publishing and resource registries. Namespace allocation
remains a creation capability/addressing concern, not perpetual ownership of
sites that have since been transferred away.

Initially co-locate managed-site resource metadata and lifecycle operations with
the trusted publisher so ownership, authority checks, active-version changes,
and cleanup receipts can share a durable transaction. This is a storage choice,
not a promise that future provider calls are transactional. The operator UI is a
client, not the only place resource state lives.

### Operator transport and UI

Register trusted resource backends in private operator configuration. Local
backends are for explicit local operation/tests; remote publisher administration
uses operator SSH, not agent credentials or a public admin HTTP endpoint.
Inventory requests must report unreachable backends rather than pretending they
have no resources. Plans bind backend identity and must not be redirected by a
changed connection configuration between review and apply.

The Resources UI lists resources even when their former cell is absent. It shows
owner, management type, lifecycle/visibility, access, and relevant warnings.
Operator actions use preview/approval, not optimistic direct mutation.

## Ordered PRs

### Prerequisite — #40 static publishing

Review current diff and local test evidence, including the actual cross-user
HTTPS/Chromium smoke test. Merge only with a clean expected head. It is not a
verified live exe.dev rollout. Production routing remains operator work.

### PR 1 — Shared resource types and invariants

- Add the shared types, bounded identifiers, lifecycle operation phases, cleanup
  policies/effects, and approval/receipt shapes.
- Keep ownership separate from attachments and managed/external.
- Export applicable TypeScript contracts and add wire/identity/plan tests.
- No runtime behavior change and no speculative provider provisioning.

Acceptance: malformed identities and impossible/unsupported lifecycle requests
are rejected; plans change when identity, revision, target, or effects change.

### PR 2 — Resource registry and site migration

- Introduce resource records for existing and newly published sites.
- Migrate existing ownership, grants, reservations, active versions, and history
  under the publisher lock, without resetting replay protection or URLs.
- Implement inventory and revision-bound operator actions (unpublish, transfer
  to operator/cell custody), with durable operation receipts.
- Ensure transfer revokes prior effective access, including broad grants; newly
  assigned ownership does not bypass the approved grant model.
- Add freeze/cleanup transaction primitives for the later deletion workflow.

Acceptance: migration and repeated opening are stable; stale plans fail; transfer
preserves addresses/history; wrong owners, stale broad grants and foreign versions
cannot mutate a transferred resource; interrupted/repeated operations reconcile.

### PR 3 — Operator inventory and lifecycle UI

- Wire configured trusted resource backends through the operator gateway.
- Add Resources navigation, inventory/detail views, management/ownership/access
  labels, and clear unavailable-backend states.
- Ship operator unpublish and custody-transfer review/apply controls with backend
  checks, durable receipts, and actionable errors.

Acceptance: resources remain visible/manageable after the original cell is gone;
UI does not infer authority from names; exact reviewed plans are applied; failures
are visible. Test real HTTP endpoints and the rendered UI in a browser.

### PR 4 — Resource-aware cell deletion (backend and UI together)

- Include resource inventories and cleanup choices in the deletion target/hash.
- Extend Delete cell to show owned vs attached resources, public exposure,
  retention/cost/manual warnings, and exact cleanup choices before confirmation.
- Default managed-site deletion to unpublish + operator custody. Transfer selected
  sites explicitly to keep them live. Purge is separate and not a default.
- Freeze the departing identity; execute/reconcile per-backend cleanup; only then
  disconnect integrations, delete the VM, and remove the declaration/pin.
- Display durable cleanup progress, partial failure, retry/review guidance, and
  manual obligations. Preserve existing interrupted-deletion behavior safely.
- Reject unsafe older deletion approvals when resource coverage is not known.

Acceptance: no required cleanup can be bypassed through CLI/API; new resources
invalidate review; signed requests cannot race deletion into orphaning sites;
retries/restarts do not repeat acknowledged destructive operations; deletion of
a delegate does not remove another owner's resource; offline publishers block
rather than silently disappear from inventory.

### PR 5 — Externally managed attachments and UI

- Register externally created resources using provider/account identity and
  verified operator-entered external identity. Clearly label them external.
- Attach/detach scoped access references to cells independently of ownership.
- Add UI for registration, attachment, and management limits. Do not display
  credential values or pretend a credential reference delivers credentials.
- Cell deletion removes local attachments and explicitly reports any outstanding
  provider revocation/continuing billing/manual cleanup. Require acknowledgement
  of approved manual obligations instead of claiming automatic cleanup.

Acceptance: deleting an attached cell never destroys the underlying external
resource; unsupported operations are refused; one cell's detachment does not
remove another's access; UI distinguishes local access removal from verified
remote revocation. No RunPod/exe.dev API lifecycle implementation is claimed.

## Review and merge gate for every PR

1. Isolate work from unrelated local changes; branch from current main.
2. Review the complete diff against the invariants above. Record concrete
   findings and fixes. A self-review must be described as a self-review.
3. Run Rust tests, generated-contract checks, relevant frontend tests/build,
   private-state/history checks, and diff checks. Add negative/concurrency/restart
   tests for authority or persistence changes, not just happy paths.
4. Exercise changed UI flows in a real browser and critical transport boundaries
   over real local sockets. Re-run the separate-UID HTTPS publisher smoke test
   after changes to ownership, signing, artifacts, or public serving.
5. Put verification evidence and known limitations in the PR. Wait for any
   required checks/review protections; never bypass branch protection.
6. Merge only the reviewed commit SHA. Confirm GitHub reports it merged. Fetch
   main and base the next PR on that merge, not on an assumed local state.
7. Update the execution log below with PR links, merge status, tests, and any
   justified changes in scope. Do not call the whole workstream complete early.

## Explicit non-goals / later adapters

- Automatic discovery of arbitrary resources created with unrestricted credentials.
- RunPod/exe.dev compute provisioning, termination, billing, or reconciliation
  without a concrete adapter, account scope, and verified provider behavior.
- Automatic adoption from a URL or an agent's assertion.
- Transparent provider credential issuance/revocation when the provider has no
  usable API. Store references and describe manual work instead.
- Automatic destructive storage purge or garbage collection.
- Claiming a live Internet deployment from local HTTPS/browser tests.

## Execution log

- Planning: this document records the agreed sequence and merge gates.
- Prerequisite #40: pending review/merge.
- PRs 1–5: not yet implemented.
