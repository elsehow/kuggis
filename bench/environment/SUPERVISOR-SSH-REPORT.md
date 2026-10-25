# External-host supervisor acceptance

The supervisor connection passed local regression tests and live acceptance on
the standalone OVH canary before deployment. The production supervisor now has
one additional external canary; its five existing cells and all 38 existing chat
IDs were preserved. Existing cells were not moved or given a runtime upgrade.

Implementation commit: `c7b3bdecc46f8128b394a5df601fc1f88db45152`.
Tested and deployed Linux executable SHA-256:
`f4f6472255e62dbe6377bc02760128f0e7185d533fc52cb9ab51efde20c3ca01`.

## Results

| Check | Result |
| --- | --- |
| Complete final Rust suite | 236 passed; 2 existing tests ignored |
| Embedded Git broker / bootstrap / chat-health Python suites | Passed: 10 / 8 / 11 tests |
| Frontend contracts and production build | Passed |
| Web integration suite, including browser tests | 40 passed; 4 existing optional tests skipped |
| External inventory/routing never invokes exe.dev transport | Passed |
| Missing binding, wrong key, changed machine binding and SSH trust | Failed closed |
| Registration refuses existing cell adoption and conflicting VM identity | Passed |
| Unsupported grants, server deletion and child doors | Refused |
| Real Pi health and credential prompt/cancellation through SSH | Passed; no model credential submitted |
| Stale approval after identity rename | Refused; display name restored |
| Live streamed chat over the supervisor's SSH relay | Passed |
| Apply during a running chat | Refused |
| Supervisor restart while the chat continues | Passed; no duplicated user/tool/answer sequence |
| Chat rename, archive and unarchive | Passed |
| Reviewed grant addition/removal and unchanged no-op apply | Passed |
| Failed SSH connection, wrong machine identity and trust drift | Access refused; healthy after binding restoration |
| Stopped chat diagnosis and explicit start recovery | Passed |
| Actual browser using the OVH connection | Chat/settings render; unsupported lifecycle controls absent |
| Final executable setup and full OVH reboot | Passed; changed boot ID and exact binary verified |
| Host services start automatically after reboot | Environment, Git broker, proxy and signer active |
| Reconnected chat metadata and test history after reboot | Preserved |
| Normal Pi restored after fixture tests | Passed; test service override absent |
| Private Git fetch and authenticated dry-run push after reboot | Passed; no remote ref created |
| Credential isolation, original identity/workspace/software and signed boundary checks | Passed |
| Production deployment comparison | Five existing cells and 38 chat IDs preserved; all six cells healthy |
| Production assets, unauthenticated access and foreign-origin refusal | Passed |
| External anonymous and forged-identity requests | Redirected to authentication; inventory not exposed |
| Production identity context reaches OVH guest | Passed |

One parallel regression run hit the existing publishing bridge's immediate
dead-socket assertion. That test passed in isolation; complete serial runs,
including the final source revision, passed. No publishing runtime change was
made to suppress the failure.

## Scope and deployment

Live streaming tests used the repository's deterministic Pi fixture inside the
disposable canary, with the real supervisor, SSH relay, chat process and guest
environment. No live model inference was performed. Real Pi's credential prompt
and cancellation were exercised separately, and its normal service was restored.
GitHub write validation used `--dry-run`; actual remote ref updates and branch
protection hooks were not exercised.

Acceptance ran first in a separate supervisor tree/state directory and service,
under production-style systemd restrictions. A dedicated supervisor key stayed
on that machine; only its public key was authorized on OVH. The test supervisor
was stopped before production registration. Production state was backed up,
the exact built artifacts were verified and activated, and the previous release
was retained for rollback. Deployment checks covered the existing inventory,
chat IDs, health, assets and access controls.

External hosts support standalone chat, streaming, provider settings,
health/recovery, renaming and reviewed grant updates. Provider-owned server
creation/deletion, cross-provider child doors, and exe.dev integration/publishing
grants remain unsupported and are refused. Migrating an existing production cell
still requires a separate cutover preserving its identity, credentials, signed
history, chats, workspace and installed software.

See [registration instructions](../../deploy/ssh-hosts.md#registering-with-a-supervisor)
and [reproduction steps](README.md#supervisor-connection). Raw identifiers,
receipts, screenshots, keys and deployment backups remain in private storage.
