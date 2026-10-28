# GitHub access on every host

Kuggis uses one broker on exe.dev and ordinary SSH hosts. Every cell has its own
repository grants, with independent read/write access and no fixed repository
count. `repo add`, `repo remove`, and `repo list` manage those grants.

The **SSH gateway** works with a developer's GitHub SSH identity. No organization
GitHub App installation is needed. The broker holds the SSH private key and
enforces cell policy before opening GitHub connections. Guests receive only
short-lived credentials for this gateway, never the developer key or an SSH agent
socket. GitHub's existing account permissions and organization policies still apply.

An optional **GitHub App** backend supports repository-scoped GitHub API tokens
where App installation is available. Both backends use the same cell/repository
commands; use separate broker databases and cell enrollments for each backend.

## Install an SSH gateway

Use a trusted, always-on machine outside the guests. A dedicated exe.dev box is
suitable. Install Python 3.11+, Git, OpenSSH, and the Linux Kuggis binary. The
broker needs outbound SSH to `github.com:22`. Run it as a dedicated service user.

Create a dedicated SSH key on that machine, in a protected directory owned by
the service user. This unattended service uses a private key file, with no agent
or passphrase prompt. Keep the private key there. On your operator machine, add
its **public** key to your GitHub account using your existing `gh` login:

```sh
gh ssh-key add ./kuggis-broker.pub --type authentication --title kuggis-broker
```

GitHub documents [adding a key through gh](https://cli.github.com/manual/gh_ssh-key_add).
If required, [authorize that key for organization SSO](https://docs.github.com/en/enterprise-cloud%40latest/authentication/authenticating-with-single-sign-on/authorizing-an-ssh-key-for-use-with-single-sign-on).
The broker gets the repositories that account can use; it can only delegate a
subset to cells. It does not bypass organization restrictions.

Populate a protected `known_hosts` file using GitHub's
[published SSH host keys](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints).
Do not accept an unverified keyscan result. The broker disables personal SSH
configuration, agent forwarding, connection sharing, and interactive prompts.

Create a random admin credential, for example with `openssl rand -hex 32`, and
save it in a mode-0600 file. Transfer it securely to your operator machine. It
authorizes grants for all cells and must never go into a guest or host relay.

Create a mode-0600 configuration owned by the broker service user:

```json
{
  "transport": "ssh-gateway",
  "ssh_key_file": "/etc/kuggis-github/developer-key",
  "known_hosts_file": "/etc/kuggis-github/known_hosts",
  "admin_token_file": "/etc/kuggis-github/admin-token",
  "database": "/var/lib/kuggis-github/broker.sqlite3",
  "tls_cert": "/etc/kuggis-github/fullchain.pem",
  "tls_key": "/etc/kuggis-github/tls-key.pem",
  "listen": "127.0.0.1",
  "port": 8443
}
```

Private keys and the admin credential must be owned by the service user and mode
0600 or 0400. Its database directory must be mode 0700 and owned by that user.
Protect parent directories from untrusted writes. `known_hosts` must be an owned
regular file, not writable by other users. Back up the database securely.

Run under your service manager:

```sh
kuggis github serve --config /etc/kuggis-github/broker.json
```

The broker serves HTTPS directly. With a publicly trusted certificate, bind a
reachable address and connect to that port directly. The loopback example above
requires a reverse proxy. All client connections require trusted TLS; no insecure
mode or credential-bearing redirect is supported.

### Dedicated exe.dev box

The exe edge supplies public HTTPS and forwards HTTP to the box. Run a local
reverse proxy on the forwarded HTTP port, connecting to the loopback HTTPS broker.
For example, with nginx and a broker certificate for `localhost` trusted by the
specified CA file:

```nginx
server {
    listen 8000;
    location / {
        proxy_pass https://127.0.0.1:8443;
        proxy_ssl_verify on;
        proxy_ssl_server_name on;
        proxy_ssl_name localhost;
        proxy_ssl_trusted_certificate /etc/kuggis-github/broker-ca.pem;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_buffering off;
        proxy_read_timeout 960s;
        proxy_send_timeout 960s;
    }
}
```

Route the exe hostname to port 8000. Its edge must allow machine requests without
an interactive login; the broker authenticates each request itself. Protect the
HTTP leg from access outside the box/edge. The edge and reverse proxy must support
WebSocket upgrades, preserve Authorization, and stay connected for Git transfers.
Use the public HTTPS hostname with `github connect`. The exe edge path has not
been validated by the local integration tests; test a disposable repository
through that public hostname before attaching production cells.

## Connect the operator and cells

On your own computer:

```sh
kuggis github connect --url https://YOUR-BROKER-HOSTNAME \
  --admin-token-file /secure/local/github-admin-token
kuggis github check --repo owner/project-a --access write
```

`connect` verifies the broker and records its URL and admin credential file path
in the workspace state directory. With SSH, `check` verifies that the broker can
read the repository; GitHub checks actual write permission on push.

For an existing cell, first install matching updated client/Linux artifacts with
`kuggis setup CONFIG --bin LINUX_BINARY`. This one-time upgrade restarts the guest
and mounts the credential socket. Then attach each cell once:

```sh
kuggis github attach fri
kuggis github attach merrillfront
kuggis repo add fri owner/project-a --role read
kuggis repo add fri owner/project-b --role develop
kuggis repo add merrillfront owner/project-a --role develop
kuggis repo list fri
kuggis repo remove fri owner/project-a
```

All commands accept a workspace JSON path instead of its name. On the SSH gateway,
choose a role to set Git and PR/issue access in one atomic grant:

| Role | Git | PRs and issues | API merging |
| --- | --- | --- | --- |
| `read` | Read | Read | Disabled |
| `develop` | Read/write | Read/write | Disabled |

For example, `kuggis repo add fri forecastingresearch/forecastbench-sim --role develop`
connects that repository to FRI for development. A grant covers all its PRs and
issues and persists until changed or removed. No cell restart or per-repository
login is needed. The broker needs its one-time API login described below before
API requests can succeed. Roles can be saved before that login is complete.

Reapplying a role replaces both permission levels and disables API merging,
including on an existing grant. `--role read` therefore downgrades Git and API
access together. `repo list` displays the effective permission fields; there is
no separate role label that could become stale after a custom permission change.
The operator client and broker must both support roles; existing cells need no
runtime upgrade for this command when they already have the API adapter.

For custom permissions, `repo add --access read|write` sets Git access only,
preserving existing API permissions; `repo api` sets API permissions independently.
`--role` cannot be combined with `--access` or `--workflows`. Roles require the SSH
gateway backend; the optional GitHub App backend retains `--access` commands.
SSH **write includes workflow-file changes**, subject to upstream GitHub rules. There is no
separate workflow-file restriction in this mode; `--workflows` is rejected.

Repo commands do not require a live cell host. Preserve the adjacent
`workspace.json.github.json` receipt identifying its broker enrollment. A failed
attach retains a private enrollment file for retry, removed after success.

New `kuggis create NAME --repo OWNER/REPO` workspaces use the configured broker
and seed one grant. Preflight checks happen before allocating an exe VM. Repeating
creation preserves later operator changes. Granting a repository does not clone it.

## Inside an SSH-backed cell

Use normal GitHub remotes:

```sh
git clone git@github.com:owner/project-b.git
git clone https://github.com/owner/project-a.git
git -C project-b fetch
git -C project-b push
```

Kuggis installs a Git remote helper and rewrites the standard HTTPS, SSH URL, and
`git@github.com:` prefixes to its gateway transport. It streams native Git pack
data over authenticated TLS WebSockets. Submodule repositories need their own
grants. Attach adds the gateway endpoint to the guest's outbound network policy.

SSH keys authorize Git. For PRs and issues, enable the broker's API provider below.
Git LFS, arbitrary REST/GraphQL, release assets, and other gh commands are not
provided by this adapter. Plain `gh` is not given a global login.

## PR and issue access through the same broker

Install GitHub CLI in `/usr/local/bin` or `/usr/bin` on the broker. Create a private
mode-0700 directory owned by its service user, for example
`/var/lib/kuggis-github/gh`. Add this optional field to its mode-0600 configuration:

```json
"gh_config_dir": "/var/lib/kuggis-github/gh"
```

Restart the broker after changing its configuration. As the broker service user,
with `umask 077` and that `GH_CONFIG_DIR`, run:

```sh
gh auth login --hostname github.com --git-protocol ssh --skip-ssh-key --web --scopes repo
```

Complete GitHub's device/browser authorization using the account whose API access
you intend to delegate. Organization OAuth restrictions and SSO still apply.
Keep `hosts.yml` owned by the service user and mode 0600. This API login is separate
from the SSH key; no GitHub API token is returned to a cell. Subsequent logins or
token rotation take effect on the next request without restarting the broker.

The deployed `kuggis-github.exe.xyz` host provides a helper that runs this login
with the correct service identity, directory, and file permissions:

```sh
ssh -t kuggis-github.exe.xyz sudo /usr/local/bin/kuggis-github-login
```

Verify authentication from the configured operator, then set per-repository API
permissions independently of Git access:

```sh
kuggis github api-status
kuggis repo api fri forecastingresearch/airo --issues write --pull-requests write
kuggis repo api fri owner/reference --issues write --pull-requests read
```

These independent API controls are optional when using `repo add --role`.
The repository must already be attached with `repo add`. New Git-only grants
created with `--access` default API permissions to `none`. Each `repo api`
command sets both API levels; add `--merge` only when PR merging should also be
allowed. Omitting `--merge` disables it. `repo add` preserves API permissions;
`repo remove` removes Git and API authority together. Replacing a removed grant
starts with the permissions specified by the new role, or API access disabled
when using `--access`.

Inside a connected cell:

```sh
kuggis-gh --repo forecastingresearch/airo -- pr list
kuggis-gh --repo forecastingresearch/airo -- pr view 123
kuggis-gh --repo forecastingresearch/airo -- pr create \
  --title 'Fix the importer' --body-file description.md --base main --head fix-importer
kuggis-gh --repo forecastingresearch/airo -- issue create \
  --title 'Importer fails on empty input' --body 'Steps to reproduce...'
kuggis-gh --repo forecastingresearch/airo -- pr review 123 --comment --body 'Review notes'
```

The SSH-mode adapter outputs JSON. It supports `list`, `view`, `create`, `edit`,
`close`, `reopen`, `comment`, and `comments` for PRs and issues. PRs additionally
support `diff` (file patches), `reviews`, `review`, and `merge`. Use `--help` after
the resource/action for available arguments. List operations have explicit
`--limit` (1–100) and `--page`; issue lists exclude PRs, so a page may contain fewer
issues than the requested limit. Bodies support `--body-file FILE` or `-` for stdin.
The adapter does not reproduce every GitHub CLI flag or command.

PR creation requires explicit `--base` and `--head` branch names. For a fork, add
`--head-repo OWNER/FORK`; the same cell must have a grant for both base and head
repositories, and they must belong to the same GitHub fork network. Push the
branch using Git first. The API provider cannot create an ungranted fork or
increase the account's upstream GitHub permissions.

Merging requires both pull-request write and the separate merge grant, plus
`--match-head-commit FULL_SHA` to prevent merging a changed head accidentally.
GitHub branch protection still applies. This controls API merging; a Git write
grant separately allows pushes permitted by GitHub.

The broker validates a defined operation and its fields, constructs the REST
endpoint itself, and checks repository grants on every request. It rejects raw
URLs, arbitrary API paths, GraphQL, and caller-supplied cell identities. Issue
endpoints cannot be used to bypass PR permissions. Repository IDs are pinned on
first API use; renames/path reuse require explicit operator reconciliation.

API requests serialize with policy changes. A revocation waits for an already
authorized operation to finish before acknowledging; upstream changes already
made cannot be undone by revocation. Requests are never automatically retried.
After an ambiguous timeout, inspect the issue/PR before retrying a creation or
comment. Git access continues working while the API login is missing or expired.

## SSH isolation and revocation

Host root stores a cell-specific broker credential at
`/etc/kuggis/github-connection.json`. The guest can only request a repository
through the host's Unix socket. It cannot choose the cell identity or permissions.
Neither broker admin credentials nor the upstream SSH key go to cell hosts.

Gateway tokens expire after 15 minutes and are stored as hashes in the database.
They identify one cell/repository. Only `git-upload-pack` is allowed for read;
write additionally allows `git-receive-pack`. The destination is fixed to GitHub,
with validated repository names and no arbitrary SSH commands. Sessions are
bounded globally and per cell. Transfers must finish within the token lifetime.

Removing or changing a grant atomically invalidates its gateway tokens. Each new
connection checks current policy; an active stream rechecks every half second and
is terminated on expiry or revocation. Upstream writes already accepted cannot be
rolled back. Local clones and public readability are unaffected. A broker outage
prevents Git access, and a normal restart preserves grants and revoked state.

SSH identifies repositories by **owner/name**, not immutable GitHub IDs. Review
grants after repository transfers, renames, or path reuse. This backend assumes
the broker's SSH identity and machine are trusted; compromising them exposes that
identity's broader GitHub authority.

## Optional GitHub App backend

Where installation is possible, register an App with Contents, Issues, and Pull
requests read/write, plus optional Workflows write. Install it on the required
accounts/repositories. Its installation defines the maximum available authority;
cells still receive only explicit grants. Replace `transport`, `ssh_key_file`, and
`known_hosts_file` in the configuration with:

```json
"app_id": "123456",
"private_key_file": "/etc/kuggis-github/app.pem"
```

This backend needs OpenSSL and HTTPS to `api.github.com`. It mints installation
tokens for one numeric repository ID with explicit permissions and approximately
one-hour expiry. Both read/write presets include Contents, Issues, and Pull
requests. `--workflows` can additionally allow workflow edits on a write grant.

Guests use a GitHub-specific HTTPS credential helper and can run:

```sh
kuggis-gh --repo owner/project-b -- pr list
kuggis-gh --repo owner/project-b -- api repos/owner/project-b/issues
```

The helper does not persist tokens or put them into remote URLs. The gh adapter
sets a token only for that invocation; plain `gh` has no global login. GitHub API
tokens may be copied by the guest, within their granted scope. Network access to
GitHub and its API is added during attach; asset hosts may need extra endpoints.

Changes first persist the new policy and mark outstanding tokens for revocation,
then revoke them through GitHub. Failures appear in `pending_revocations` and block
issuance for that grant until retried or expired. Retry the same change. The
database preserves reduced authority and pending work across restarts.

## Migrate existing credentials

Remove an old `github.int.exe.xyz` resource and detach that cell's exe GitHub
integration before attach. Kuggis refuses declarations that still include it.
Do not remove a shared integration used by another cell. Retire broader guest
Git credentials and conflicting Git URL rewrites on other hosts too. Kuggis does
not silently delete unrelated credentials or rewrite existing repository files.

Transport mode is pinned to a broker database and host enrollment. Switching an
already-enrolled cell from App to SSH requires a deliberate new enrollment and
retirement of the old credentials/grants; merely changing the backend config is
rejected. Keep database backups and enrollment receipts together. Restoring an old
database can restore old grants and credentials; reconcile policy and rotate
affected credentials before returning a restored broker to service.
