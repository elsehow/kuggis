# Shared scoped Git acceptance

The shared host-side Git broker passed the local test suite and live public and
private repository acceptance on the standalone OVH canary. exe.dev and SSH hosts install the same
broker and guest URL routing. New plans create no platform GitHub integrations;
legacy attachments are removed only after a pinned, signed broker check passes.

The tested Linux executable's SHA-256 is
`6137a3f7817ce356f5fabc74f11ea1fc6e22fa2b2764e9d6dceccce205e530c3`.

## Results

| Check | Result |
| --- | --- |
| Complete Rust suite | 232 passed; 2 existing tests ignored |
| Real Git broker tests, included by the Rust harness | 10 passed |
| Real local clone/fetch, chunked pushes and gzip negotiation | Passed |
| Read-only push refusal, fork isolation, grant revocation | Passed |
| Credential/header isolation, private DNS, redirects and framing rejection | Passed |
| Legacy Git upgrade ordering and signed-evidence guard | Passed |
| Live public clone/fetch through the OVH broker | Passed |
| Standard GitHub and old exe.dev URL rewrites | Passed |
| Live ungranted repository and read-only dry-run push refusal | Passed |
| GitHub CONNECT tunnels remain blocked without host grants | Passed |
| Both guest proxy ports blocked from the public internet, SSH positive control | Passed |
| Guest cannot see credential locations | Passed |
| Host credential installation/rotation in a temporary synthetic fixture | Passed |
| Upgrade preserves signing identity, machine identity, runtime configuration, workspace, jq and idle chat | Passed |
| Reviewed unchanged deployment performs no setup | Passed |
| Automatic Git startup and repeated acceptance after full reboot | Passed; changed boot ID verified |
| Live private GitHub clone/fetch with a repository-scoped token | Passed |
| Installed token alone grants no repository access | Passed; refused before adding the Code grant |
| Read-only grant refuses push despite a write-capable token | Passed |
| Authenticated GitHub dry-run push with a write grant | Passed; proposed test ref absent before and after |
| Actual remote GitHub ref updates / server-side update hooks | Not exercised; live pushes used `--dry-run` |

The first live attempt exposed an incorrect ownership assumption about
systemd's credential copy. Ubuntu supplies a root-owned read-only file with
service access through an ACL. The loader now distinguishes that managed copy
from the original root-private store; a regression test covers both. Failed
setup was reported as a failure and retained its pinned identity. A newly
reviewed fixed build completed setup and passed the signed check.

## Scope of the result

The initial live fixture used an empty broker credential store and one read-only
grant for the public Hello-World repository. A subsequent run installed a
fine-grained token for a private repository, verified that storing it alone
conferred no access, and applied first a read-only grant and then a write grant.
Both phases passed independent clone/fetch, legacy URL routing, ungranted
repository refusal, guest exclusion from credential paths, and preservation
checks. Trusted host probes in the guest network namespace independently
confirmed upload-pack and receive-pack authorization. Fresh signed boundary
checks passed after both changes.

The write phase successfully proposed a unique new branch with `git push
--dry-run`, checking that the exact remote ref was absent before and afterward.
This verifies authentication and negotiation, not actual ref updates or
server-side update hooks. The live run performs no remote writes or model
inference. Real write operations and synthetic credential injection were tested
against disposable local Git backends. Production cells have not been upgraded
or migrated by this run.

Production rollout still needs credentials for each private/write repository
and reviewed upgrades. GitHub REST/GraphQL, PR/issue tooling and Git LFS are outside
this Git transport. Existing explicit public-host grants remain independent
network capabilities; repository grants never add those implicitly.

See [configuration and rollout](../../deploy/scoped-git.md) and the
[live reproduction procedure](README.md#scoped-git-upgrade). Raw host identifiers,
plans, approval receipts, logs and preservation hashes remain in ignored private
deployment storage.
