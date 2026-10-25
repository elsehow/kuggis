# Shared repository-scoped Git

Both exe.dev and standalone SSH hosts use the same host-side Git broker.
A `code` grant authorizes Git clone/fetch and, when `write` is true, push.
It does not add GitHub to the general CONNECT allowlist. Guest root can change
Git settings, but the host still checks every repository and Git operation.

```json
{"code":{"repo":"example/project","write":false}}
```

With `via`, only the carried repository is reachable. For example,
`{"code":{"repo":"example/project","write":true,"via":"fork/project"}}`
allows reading and pushing `fork/project`; it does not allow reading or writing
`example/project` without a separate grant. Repository names compare without
case, consistently with the existing `Repo` type.

The broker implements [Git smart HTTP](https://git-scm.com/docs/http-protocol):
reference advertisements and upload-pack/receive-pack requests only. It fixes
the upstream to `https://github.com`, validates public DNS addresses and TLS,
and refuses redirects, arbitrary queries, other endpoints, and mismatched
methods/content types. Client authentication, cookies and forwarding headers
are discarded. Only the host's credential for that exact repository is used.
Errors and redirect responses carry no upstream body, cookie, or credential.

## Credentials

Public repositories can be read without credentials. Private reads and pushes
need a repository-scoped GitHub credential. A fine-grained token should select
only the intended repositories and the required Contents read/write permission;
a GitHub App installation token can also be used. Follow
[GitHub's token guidance](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).
Tokens retain their upstream restrictions and expiration. This implementation
does not mint or refresh GitHub App tokens automatically.

On the trusted host, pipe a token from a private source into:

```sh
sudo cell git credential example/project < /private/path/to/token
sudo systemctl restart cell-git
```

The token is read from stdin, never from a command-line argument. The command
stores it under its exact repository in root-owned mode-0600
`/etc/cell/git-credentials.json`, using a lock and atomic replacement. Setup
creates an empty credential store if absent. A write grant without a configured
credential fails preflight before stopping the existing cell.

`cell-git.service` runs as a separate unprivileged host user and receives a
private systemd `LoadCredential` copy. Neither credential location is mounted
into the guest. Restart the service after rotating credentials. Revoking a
grant changes authorization; deleting a token alone does not revoke anonymous
read access to a public repository that remains granted.

## Routing and enforcement

The guest connects to `http://10.203.0.1:3129/OWNER/REPO.git` over its private
veth. Host nftables permits that port only from the guest or host loopback and
blocks it on public interfaces. The broker's output is limited to TLS, its
configured DNS resolvers, and guest responses. Its systemd service has a
512 MiB memory limit and accepts at most 16 concurrent requests.

Guest setup installs Git URL rewrites for GitHub HTTPS/SSH URLs and old
`https://github.int.exe.xyz/` remotes. Existing clones and remote configuration
stay intact. The broker URL bypasses the general CONNECT proxy. New clones
use the broker directly. Rewrites are convenience, not the security boundary.

The current transfer bounds are 128 MiB per request and 1 GiB per response.
Chunked Git pushes and gzip-compressed negotiation work within those bounds.
Git LFS and GitHub REST/GraphQL
operations (including `gh pr` and issue management) are not implemented by this
Git transport and gain no implicit access. Submodules need their own grants.
Do not add broad GitHub host grants as a workaround for scoped operations.

## Retiring the platform Git path

The planner no longer creates or attaches exe.dev GitHub integrations. Existing
attachments are read only to plan their removal. An upgrade with legacy Git
requires a reviewed Linux `--bin`; setup runs before detachment. Detachment
also requires the pinned cell's latest signed check to prove that its scoped
broker passes. An old binary without that evidence cannot remove the old path.

Install the required host credentials before upgrading existing private/write
cells. Validate the canary and account for any GitHub API workflows before
rolling out to production cells. This is one implementation for all hosts;
legacy platform attachments are migration state, not a selectable backend.

## Validation

`python3 cell/tests/git_broker.py -v` runs real Git clients against disposable
`git http-backend` repositories and synthetic credentials. It covers clone,
fetch, read-only push refusal, chunked write pushes, fork isolation, revocation,
credential/header isolation, private-address refusal and redirects. The Rust
suite includes these tests and checks platform reconciliation and upgrade
ordering. Live `cell check` tests the broker from the guest network namespace,
including advertised read/write operations for each declared repository.

The standalone canary also passed live private clone/fetch, read-only push
refusal with a write-capable token, and an authenticated dry-run push after
enabling its write grant. No remote refs were changed. See the
[acceptance results](../bench/environment/GIT-REPORT.md) and
[reproduction steps](../bench/environment/README.md#scoped-git-upgrade).
