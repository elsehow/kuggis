# Kuggis command guide

Kuggis has five public command groups. `kuggis --help` lists them; each group has
its own `--help`. The top-level `login` command signs an agent into a cell.
A cell argument accepts a saved name or a workspace JSON path.

| Group | Purpose |
| --- | --- |
| `cell` | Create, inspect, configure, and enter cells |
| `chat` | List, open, and archive agent conversations |
| `access` | Inspect, grant, revoke, and check repository or network access |
| `file` | Upload and download files |
| `admin` | Configure shared GitHub infrastructure, upgrade runtimes, and debug |

## Everyday work

```sh
kuggis cell create fri --repo forecastingresearch/airo --github-access write
kuggis cell list
kuggis cell status fri --check
kuggis login fri --agent codex
kuggis chat list fri
kuggis chat open fri research --agent codex
kuggis cell shell fri
kuggis cell exec fri -- git status
kuggis cell web fri
kuggis file put fri ./results.csv inputs/results.csv
kuggis file get fri outputs/report.pdf ./report.pdf
kuggis chat archive fri research
```

Creating a cell is the existing exe onboarding workflow. It enrolls the cell
with the configured GitHub broker. For the SSH backend, `--github-access read`
seeds the `read` role; `write` seeds `develop`, including PRs and issues, with API
merging disabled. Existing creation receipts preserve later repository grants.
An interrupted creation that needs to update an existing cell requires an
explicit `cell create ... --restart` retry. Creating a fresh cell does not.

`cell list` reads the operator's saved inventory; it does not probe every host.
`cell status` reads live service state. `--check` also runs the existing boundary
checks. Chat, shell, file, and preview commands retain their existing behavior.

## Agent login

```sh
kuggis login fri --agent codex
kuggis login merrillfront --agent codex
kuggis login fri --agent codex --status
kuggis login fri --agent codex --plan
```

Run these on your laptop. The current adapter supports Codex with ChatGPT
browser authentication. Kuggis carries the normal OAuth callback over SSH into
the guest, checks provider network access, and shares the native login across
the cell's Codex chats and shells. Device-code authentication is not required.
Each cell retains its own credentials, refreshed by Codex inside that cell.

`--no-open` prints the sign-in URL instead of opening your browser. `--force`
requests a fresh sign-in to switch accounts; otherwise a cached ChatGPT login is
verified and reused. `--json` emits a browser-authorization event when needed,
followed by the final report; callback codes and credentials are never printed.
`--status` checks the shared credential cache without opening a browser or
changing the provider. `--plan` reports network grants and the launcher migration
without applying either.

Existing chats keep their native history and model preferences. Login updates
the launcher and outbound proxy as needed, without restarting the cell or its
chat processes. Exit and reopen already-running Codex processes to use the new
account. New chats and plain `codex` in a fresh guest shell use the same login.
The chosen cell-wide provider is `openai`; legacy exe-backed chats inherit it
when relaunched. Per-chat databases remain separate native stores, while auth
uses the shared `~/.codex` home. See the [Codex recipe](../recipes/codex/README.md).

## Repository access

```sh
kuggis access grant fri repo forecastingresearch/airo --role develop
kuggis access grant fri repo owner/reference --role read --plan
kuggis access list fri
kuggis access check fri repo forecastingresearch/airo
kuggis access revoke fri repo owner/reference
```

| Role | Git | PRs and issues | API merging |
| --- | --- | --- | --- |
| `read` | Read | Read | Disabled |
| `develop` | Read/write | Read/write | Disabled |

Grants are per cell and repository, cover all its PRs and issues, and persist.
The broker saves both scopes atomically. Reapplying a role resets both levels and
disables API merging. Removing a repository removes Git and API authority.
For an already enrolled cell, these operations restart no services. Roles require the SSH backend
and a broker version that supports them. The App backend retains the legacy
`repo add --access` interface.

The first repository grant enrolls an already-installed cell if it has no local
broker receipt. The host must already have the GitHub relay runtime; the command
does not silently upgrade or restart the cell. Initial enrollment restarts the
proxy and GitHub relay; the plan reports that possible proxy interruption.
Configure shared broker credentials
once using `admin github connect`; complete its API login as described in the
[GitHub guide](github.md). Network access, broker login, and upstream account
permissions are distinct prerequisites.

`access check ... repo` checks the cell's broker grant, upstream Git read access,
and broker API login when applicable. It reports API repository access and Git
write as **not tested**. It never posts a comment, creates a PR, or pushes a ref.
Granting a repository does not clone it.

For independent API permissions after granting a repository:

```sh
kuggis access configure fri repo forecastingresearch/airo \
  --issues write --pull-requests read
```

Both levels are required; omitted `--merge` disables API merging. Git-only
adjustments remain available through `repo add ... --access read|write` and
preserve API permissions. GitHub branch rules still govern Git pushes.

## Network access

```sh
kuggis access grant fri network api.anthropic.com:443 --plan
kuggis access grant fri network api.anthropic.com:443
kuggis access check fri network api.anthropic.com:443
kuggis access revoke fri network api.anthropic.com:443
```

A network grant is one exact public hostname/IP and port. Use bracketed IPv6
addresses; URLs, paths, wildcards, and port zero are rejected. Private destination
pins remain an explicit workspace configuration feature. Existing pins are
preserved when re-granting an already-declared endpoint.

The command reads the authoritative configuration from the host, preserves other
resources and credentials, validates the candidate, and applies it only if the
host revision is unchanged. It restarts **only the outbound proxy**, which briefly
interrupts existing outbound connections. It verifies the cell service did not
restart. A proxy activation failure restores the old configuration and restarts
the old proxy policy.

After application, a trusted probe in the guest network namespace attempts a
CONNECT through the proxy. Grant verification checks reachability; revoke
verification checks refusal. It does not read or send application credentials.
A grant may be saved even when its upstream is unreachable: the command reports
`applied_verification_failed` and exits nonzero rather than claiming readiness.
Application authentication remains `not_checked`.

An unchanged grant does not restart anything. Internet-egress cells cannot revoke
one public destination using an allowlist removal; first explicitly configure
restricted egress. The command explains this instead of promising a deny rule.

The local workspace file is synchronized after a successful host application;
its previous contents are retained in `workspace.json.previous.json`. Live host
settings take precedence over a stale local cache. Concurrent local edits prevent
publication and produce an error stating that the host change was already applied.
Configuration paths and credential references are preserved; credential values
are never read by this workflow. No guest receives operator authority.

## Configuration and runtime updates

```sh
kuggis cell configure fri --file changes.json --plan
kuggis cell configure fri --file changes.json
```

The input is a JSON merge patch against live settings. Objects merge recursively,
`null` removes a field, and an explicitly supplied array replaces that entire
array. Use `access grant/revoke` for individual endpoints instead of replacing a
resources array. Cell identity and SSH routing cannot be changed by a patch.

Plans report changed fields and the required restart: `none`, `proxy`, or `cell`.
Endpoint-only changes use the same transaction as network grants. Other changes
use the full installer and require a matching Linux binary and explicit restart:

```sh
kuggis cell configure fri --file changes.json --restart --bin ./kuggis-linux-x86_64
kuggis admin upgrade fri --bin ./kuggis-linux-x86_64 --plan
kuggis admin upgrade fri --bin ./kuggis-linux-x86_64 --restart
```

Full installation is disruptive: running agent processes stop. It checks the
expected host revision under the installer lock before stopping anything. An
installation failure retains the existing fail-closed recovery behavior; it is
not a transactional rollback of the entire guest filesystem. Inspect status and
resolve the failure before retrying with explicit restart authorization.

These new operator commands work with an existing installed host runtime; a
cell upgrade is not required just to use network management. New installer code
is embedded in the updated operator, so even the legacy `setup` spelling refuses
to update an existing cell without `--restart`. Older operator binaries cannot
enforce this new guard; use the updated packaged client.

## Automation and compatibility

Management, creation, chat listing/archiving, and file transfers support `--json`.
New management reports include `ok`; errors include a message and nonzero exit
status. Existing JSON schemas for chat inventories, previews, and GitHub commands
remain compatible. `cell web --plan` already emits JSON. Interactive chat/shell
commands retain terminal I/O; `cell exec ... -- PROGRAM` preserves the program's
output. They do not wrap interactive streams in JSON.

`--plan` never changes permissions, enrolls a cell, restarts a service, or writes
configuration. Management plans read live host/broker state; they require access
to the relevant control plane. Creation plans remain offline configuration plans.
Noninteractive management commands never prompt.

| Existing command | Preferred command |
| --- | --- |
| `create` | `cell create` |
| `connect CELL` | `chat list CELL` |
| `connect CELL --chat CHAT` | `chat open CELL CHAT` |
| `archive CELL --chat CHAT` | `chat archive CELL CHAT` |
| `shell`, `exec`, `web` | `cell shell`, `cell exec`, `cell web` |
| `check CELL` | `cell status CELL --check` |
| `put`, `get` | `file put`, `file get` |
| `repo add CELL REPO --role ROLE` | `access grant CELL repo REPO --role ROLE` |
| `repo api CELL REPO ...` | `access configure CELL repo REPO ...` |
| `repo list`, `repo remove` | `access list`, `access revoke CELL repo REPO` |
| `github ...` | `admin github ...` |
| `validate`, `render`, `bundle`, `ssh-command` | `admin debug ...` |
| `setup CELL --bin BIN --restart` | `admin upgrade CELL --bin BIN --restart` |

Legacy spellings remain accepted. Host-service commands remain compatible but
are omitted from everyday help. Shared GitHub broker service upgrades are still
operator deployment tasks; `admin upgrade CELL` upgrades a cell runtime, not the
dedicated broker service.
