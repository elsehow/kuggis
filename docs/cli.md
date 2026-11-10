# Kuggis command guide

Positionals select a cell and chat: `kuggis CELL CHAT` opens the agent's native
terminal UI over SSH. Trailing verbs act on that chat; flags select cell
operations. `kuggis --help` lists the syntax. A cell argument accepts a saved
name or a workspace JSON path. There is no Kuggis browser chat UI.

```sh
kuggis --list                       # saved cells, without connecting
kuggis fri                          # list this cell's chats
kuggis fri --list --json             # the same inventory for scripts
kuggis fri research --agent codex    # create/open a terminal chat
kuggis fri research                  # resume the same chat
kuggis fri research archive --json   # stop and hide it, retaining history
kuggis fri --put ./image.png ./notes.md   # upload one or more files
kuggis fri --put --paste --chat research
kuggis fri --get output.pdf --save-as ./report.pdf
```

Chat names are exact validated names, not titles or fuzzy matches. Opening a new
name creates a chat using `--agent` or the cell's configured default. Opening an
archived name restores it. `open` is also accepted as an explicit trailing verb.
Supported chat verbs are `open` and `archive`; unknown verbs, including `delete`
and `rename`, fail without attaching or changing a chat. Permanent history
deletion is not implemented. Listing never starts an agent.

Cell operations use the flag immediately after CELL:

| Operation | Syntax |
| --- | --- |
| Create | `kuggis --create NAME --repo OWNER/REPO [OPTIONS]` |
| List cells | `kuggis --list [--json]` |
| List chats | `kuggis CELL --list [--json]` |
| Upload | `kuggis CELL --put LOCAL... [--save-as REMOTE...] [--chat CHAT] [--replace] [--json]` |
| Clipboard upload | `kuggis CELL --put --paste [--chat CHAT] [--json]` |
| Download | `kuggis CELL --get REMOTE... [--save-as LOCAL...] [--replace] [--json]` |

`--put` and `--get` take any number of items; every path after the flag is an
item, not a destination. Without `--save-as`, each item keeps its file name:
uploads go to the workspace (or the chat with `--chat`), downloads to the
current directory. `--save-as` takes one destination per item, in order, and
must come after the items. Items transfer one at a time in the order given and
stop at the first failure, which reports the items already completed. Existing
files are not overwritten without `--replace`. The grouped `kuggis file put CELL
LOCAL [REMOTE]` and `kuggis file get CELL REMOTE LOCAL` forms still take a single
item with a positional destination.
| Shell | `kuggis CELL --shell` |
| Execute | `kuggis CELL --exec -- PROGRAM [ARG...]` |
| Status | `kuggis CELL --status [--check] [--json]` |
| Boundary checks | `kuggis CELL --check [--json]` |
| Agent login | `kuggis CELL --login --agent codex\|claude [OPTIONS]` |
| App preview | `kuggis CELL --web [PREVIEW] [--port PORT] [--no-open] [--plan]` |
| Configure | `kuggis CELL --configure --file PATCH.json [OPTIONS]` |
| Upgrade | `kuggis CELL --upgrade --bin BIN [--plan] [--restart] [--json]` |
| Delete | `kuggis CELL --delete [--yes] [--json]` |
| Access | `kuggis CELL --access ACTION [OPTIONS]` |
| Grant | `kuggis CELL --grant repo OWNER/REPO --role read\|develop [--plan] [--json]` |
| Grant network | `kuggis CELL --grant network HOST:PORT [--plan] [--json]` |
| Revoke | `kuggis CELL --revoke repo OWNER/REPO [--plan] [--json]` |
| Revoke network | `kuggis CELL --revoke network HOST:PORT [--plan] [--json]` |

Access actions are `list`, `grant`, `revoke`, `check`, and `configure`.
`--grant` and `--revoke` are direct shortcuts for the matching access actions. Options
and behavior are shared with the compatible commands below. Provider sign-in and
app previews can use a browser; chats always use the terminal.
Existing groups and legacy command words remain reserved at the start of a
command. For a cell named `chat`, `login`, or another reserved word, use its
workspace JSON path, or `kuggis chat open CELL CHAT`.
`--help` is available after a cell or chat. Arguments after `--exec --` pass
literally to the guest, including `--help` and shell metacharacters.

The five command groups remain available for compatibility and detailed help:

| Group | Purpose |
| --- | --- |
| `cell` | Create, inspect, configure, and enter cells |
| `chat` | List, open, and archive agent conversations |
| `access` | Inspect, grant, revoke, and check repository or network access |
| `file` | Upload and download files |
| `admin` | Configure shared GitHub infrastructure, upgrade runtimes, and debug |

## Everyday work

```sh
kuggis --create fri --repo forecastingresearch/airo --github-access write
kuggis --list
kuggis fri --status --check
kuggis fri --login --agent codex
kuggis fri --list
kuggis fri research --agent codex
kuggis fri --shell
kuggis fri --exec -- git status
kuggis fri --web
kuggis fri --put ./results.csv ./notes.md --save-as inputs/results.csv inputs/notes.md
kuggis fri --get outputs/report.pdf outputs/data.csv
kuggis fri research archive
```

Creating a cell is the existing exe onboarding workflow. It enrolls the cell
with the configured GitHub broker. For the SSH backend, `--github-access read`
seeds the `read` role; `write` seeds `develop`, including PRs and issues, with API
merging disabled. Existing creation receipts preserve later repository grants.
An interrupted creation that needs to update an existing cell requires an
explicit `cell create ... --restart` retry. Creating a fresh cell does not.

`cell list` reads the operator's saved inventory; it does not probe every host.
Creation saves a cell's local record before allocating anything, so a cancelled
or failed creation stays listed. Each cell shows its setup record: `ready`, or
`setup incomplete` with what creation recorded making, in order (host, GitHub
enrollment, repository grant). `outcome unknown` marks a step that started
without a confirmed result. `created: nothing` means no remote resource was
recorded and the local directory is all there is. `--json` carries the same
record as `setup`. It reflects creation only, not later grants or live state;
use `cell status` and `access list` for those.
`cell status` reads live service state. `--check` also runs the existing boundary
checks. Chat, shell, file, and preview commands retain their existing behavior.

## Agent login

```sh
kuggis login fri --agent codex
kuggis login fri --agent claude
kuggis login fri --agent codex --status
kuggis login fri --agent claude --plan
```

Run these on your laptop. Every agent authenticates with its own native login;
Kuggis supplies no model credentials or managed model gateway, so a new cell has
no model access until you sign in. Login grants only that provider's endpoints
through the cell proxy, without restarting the cell.

**Codex** uses ChatGPT browser authentication. Kuggis carries the normal OAuth
callback over SSH into the guest, checks provider network access, and shares the
native login across the cell's Codex chats and shells. Device-code
authentication is not required. Each cell retains its own credentials, refreshed
by Codex inside that cell.

**Claude Code** signs in inside its own terminal. `kuggis login CELL --agent claude`
grants `api.anthropic.com`, `claude.ai` and `platform.claude.com`, then reports
whether the cell is already signed in. To sign in, open a chat with
`kuggis CELL CHAT --agent claude`: Claude Code shows a link; open it on your
laptop and paste the code it gives you back into the chat. No callback relay is
needed. Use `/login` in a chat to switch accounts. The login lives in the cell
user's `~/.claude` and is shared by every Claude Code chat and shell.

The remaining paragraphs describe the Codex adapter's options.

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
The cell-wide provider is always `openai`; chats created on the retired exe
gateway use it when relaunched. Per-chat databases remain separate native stores, while auth
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

`admin upgrade` also removes the retired exe model gateway grant from the cell
and says so in its plan. Agents that were still using it need `kuggis login`
afterwards; their history is kept.

Upgrade brings the cell's built-in agents (Codex and Claude Code) to the
releases pinned in this build. The plan lists each agent's installed and pinned
version. Applying downloads any needed release to the operator machine first,
upgrades and restarts the cell, then installs the release as the guest agent
user and registers agents the cell did not have yet. This is also how to update
an agent on a cell: install a Kuggis build with a newer pin and run upgrade.
Agent logins and chat history are kept; a newly registered agent needs
`kuggis login`. If the agent install fails after the restart, the runtime is
already upgraded; rerun the same command. Cells without an `agents` section
(legacy launch-only configurations) are reported as unmanaged and their agents
are left alone. Pi is never installed or changed by upgrade.

`python3 scripts/check-agent-releases.py` compares the pinned releases with
upstream and exits 1 when a pin is stale; a weekly CI job runs it for the
built-in agents. `--update` rewrites stale pins, which then need a live test.

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

## Deleting a cell

```sh
kuggis fri --delete          # show what would be removed; changes nothing
kuggis fri --delete --yes    # delete
```

Delete undoes creation in reverse. It revokes every repository grant the cell
holds at the broker, deletes the exe.dev host, then removes the cell's local
record. Without `--yes` it only reports those steps. Deleting the host is
permanent: workspace files, chat history and agent logins go with it, so `--get`
anything worth keeping first. Sessions are not signed out at the model provider.

The host is deleted only when the provider inventory shows the one this cell's
creation made. A host with the same name from elsewhere stops the command with
nothing removed; a host that is already gone is skipped. Cells that were not made
by `create` have their host left running and reported as such. The broker keeps
the cell's enrollment record, which authorizes nothing once its grants are gone.

A failed step leaves the local record in place, so the same command can be
rerun. A cancelled creation that made nothing remote is removed without
contacting the provider. Delete takes a saved cell name, not a workspace path.

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
