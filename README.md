# kuggis

Claude Code, Codex, and Pi sessions on exe.dev VMs: one VM per project, one tmux session
and git worktree per session. Run `kuggis help` for the commands.

Setup on the laptop, once:

1. `ln -s ~/Projects/kuggis/kuggis ~/.local/bin/kuggis`
2. `ssh exe.dev integrations setup github` — link GitHub to exe.dev
3. `kuggis token` — mint a Claude token into the keychain
4. Optional, for `--codex` sessions: `ssh exe.dev integrations setup chatgpt`,
   then `ssh exe.dev integrations add llm --name codex --openai=chatgpt
   --openai-account=<name> --anthropic=disabled --fireworks=disabled
   --attach tag:box`. Codex then runs on your ChatGPT plan with no OpenAI
   credential on the VM.
5. Optional: put an ntfy topic in `~/.config/kuggis/ntfy` and subscribe to it
   on your phone, to get pinged when a session waits on you.

Provision a project's VM with `kuggis new <project> <owner/repo>`. Day to day,
use cells:

```sh
kuggis cells                         # cells and their projects
kuggis ls                            # sessions grouped by cell
kuggis ls forecasting                # one cell
kuggis forecasting my-task           # start or resume a session
kuggis done forecasting my-task      # close it, preserving work checks
kuggis cell forecasting              # read-only definition
```

A cell containing several projects requires `--project NAME`, for example
`kuggis forecasting my-task --project airo`. This also works with `done`,
`put`, `sync` and `sh`; for `sh`, put the selector before the remote command.
There is no automatic fan-out or guess about which VM should receive a
command. `sync` without a target retains its existing all-project behavior.
Existing project commands such as `kuggis airo my-task` keep working. If a
project and a cell share a name, the existing project command wins; an
explicit `--project` selector can target another member of the cell.

`kuggis` on its own shows the same cell groups, session states and pending
briefs as the web dashboard. Enter attaches to a session, opens the Thinker,
dispatches pending briefs, or shows a cell definition. Ctrl-x closes a
session (`kuggis done`), and the list refreshes itself. Opening the cell home
does not query GitHub or start reviews. Detach with `C-b d` to come back to it.
It uses [fzf](https://github.com/junegunn/fzf) when installed (`brew install
fzf`), and a numbered menu otherwise.

`kuggis prs` keeps the previous PR-oriented home screen. Ctrl-t on a PR asks
the Thinker to review it (on Opus; `KUGGIS_REVIEW_MODEL` changes that).
Merging a PR offers to close its session.

`kuggis web` opens the cell dashboard on 127.0.0.1:7480
(`KUGGIS_WEB_PORT` changes it). Cells stay in a fixed order; sessions stay
alphabetical within their project, even when their state changes. A red dot
means the session needs you (including a stopped session or an unreachable
VM). Click a row, or focus it and press Enter, to copy its command. Session
commands use the cell name, adding `--project` when needed. Operator and
unassigned sessions retain their project commands.

The Thinker appears under trusted when installed or when it has requests
waiting. A brief waiting for you appears in its destination's cell with its
waiting age; clicking one copies `kuggis dispatch`. Unknown destinations
appear under unassigned. The Thinker row copies `kuggis think`. Opening this
overview only reads session state and request metadata: it never starts a
review or queries GitHub. A failed refresh keeps the last snapshot visibly
marked as stale. `kuggis think` is a tmux cockpit: the Thinker's chat, this
home screen one window over, and `kuggis watch` carrying the Thinker's
requests (think/README.md).

The **PRs** link opens the existing review tools at `/reviews`: ask for a
Thinker review (T), merge (M), send a review to its session (S), or close a
session (X). AIRO agents review and merge on their VM; they don't need this
human review loop.
Only a page holding the token in `~/.config/kuggis/web.token` can use the API;
`kuggis web` opens it with that token. The themes are BigBrain's.

### Verifiable development cells

A version 1 cell file is only a display grouping. Version 2 is a development
contract: every project inherits the same merge rule, review requirement,
spending rule and owner-controlled crossings. `cells/contract.schema.json`
defines its shape; `cells/forecasting.example.json` is a public example for
AIRO and agent-box. Actual definitions live in private config, not this repo.
No cell can include kuggis itself, share a VM with another project, or silently
reassign an existing project to a different repo or machine.

Preview onboarding from a reviewed definition:

```sh
kuggis onboard --policy cells/forecasting.example.json
kuggis onboard --policy cells/forecasting.example.json --json
# After reviewing the exact proposal, repeat with --apply <printed SHA256>.
```

Planning reads GitHub and VM configuration but changes neither. The plan names
each development fork, its upstream, its VM, its explicit offline test command,
and all proposed actions. Applying may create forks/VMs, deploy adapters, open
CI setup PRs, and change GitHub protection; it requires the hash of the current
plan. A changed policy, repository head, runtime installation or observed
configuration invalidates that approval. Partial setup is saved as incomplete;
rerunning the planner reports remaining work. Existing unmanaged VMs are never
overwritten automatically. New forks explicitly name their upstream and owner.

Onboarding has a deliberate prerequisite: the managed test and review workflows
must be reviewed and merged, and tests green on the default branch **before**
reconciliation removes human approval. The test workflow uses the distinct `cell-test`
check, so it cannot collide with an existing `test` workflow. It has read-only
repository permissions, no configured secrets, and pinned actions. The supplied
test command runs in GitHub CI, never on the operator laptop; inspect it before
applying. It must not make paid calls. Kuggis preserves existing unrelated boolean
protections and stops for additional rulesets/access restrictions rather than
silently removing them.

```sh
kuggis reconcile forecasting        # preview what still needs doing
kuggis audit forecasting            # verify and refresh readiness; no remote changes
```

A managed project's merge rule is always squash, zero human PR approvals,
`cell-test` and `cell-review` from GitHub Actions. A separate managed agent job
on the VM supplies the review verdict. All three runtimes receive the same
generated instructions. The reviewer records an exact-head verdict with
`~/.box/pr-gate.py review PR HEAD pass|fail`; the author merges with
`~/.box/pr-gate.py merge PR HEAD`. Missing, skipped, neutral, failing or pending
checks block the helper. It refuses author self-review, pins the merge head,
and confirms the merge. The operator audits branch protection before launch;
GitHub enforces it at merge time. The VM bot cannot read administration settings.
It also cannot write statuses, so the reviewer posts a bounded verdict comment.
The review workflow accepts verdicts only from that bot, chooses the latest
verdict for the current PR head, and publishes the status. It runs from the
default branch with only issue/PR read and status-write permissions, without
checking out PR code or using configured secrets. The merge helper requires the
status to match the latest verdict, so an older pass cannot mask a newer failure.
These are quality checks within the cell; a compromised cell can fabricate its
own review, so they are not a
security boundary.

CLI and dashboard show incomplete setup, verification expiry, or **development
verified; isolation not certified**. Readiness observations expire after 15
minutes or any policy/runtime change. A failed audit invalidates earlier green
observations. Starting a managed job performs a fresh verification; attaching
to an already-running session remains available for recovery. The dashboard
reads cached observations and never changes settings or starts reviews.

The scope is development readiness, not the full security architecture: this
release does not certify egress isolation, inspect every credential, enforce
provider budgets, automatically carry publication/submission requests, or
implement seal/unseal. Spending remains `ask`, and all declared crossings remain
owner-operated. Legacy display groupings are explicitly **setup unverified**.
There is no generic green “safe cell” badge.

### Adopt existing work as a job

After the destination's development contract is verified:

```sh
kuggis adopt /path/to/worktree --cell forecasting --agent pi --brief handoff/HANDOFF.md
# Review the selected commits and files, then repeat with --apply <printed SHA256>.
```

The source remote selects the declared project when unambiguous; use
`--project NAME` to disambiguate. The branch name becomes the job/session name,
or supply `--session NAME`. The source must have committed tracked changes.
Only commits after the local `origin/<default-branch>` merge base enter the
bundle. Untracked files are omitted unless explicitly selected with repeatable
`--include FILE` or `--brief FILE`. There is no automatic copying of credentials,
configuration, full transcripts, or experiment directories. Selected files must
be regular files inside the worktree; symlinks are refused. Review selected
content for suitability before moving it into a public development cell.

Approval binds the source head, selected commits, artifact bytes, cell policy,
repo, VM, runtime and job name. Apply verifies destination readiness again. The
receiver verifies the package hashes, base ancestry and exported head, refuses
collisions, and imports the branch without running repository code. A repeated
approved transfer is a no-op. The source remains intact. It creates a durable
job identity and a first-message handoff, but does not start an agent or call a
model. Open the printed kuggis command when ready; the first launch creates its
worktree, runs its existing project setup, and resumes work from the brief.

### Agent runtimes

```sh
kuggis forecasting task-a --agent claude
kuggis forecasting task-b --agent codex
kuggis forecasting task-c --agent pi
```

`--claude`, `--codex`, and `--pi` are shorthand. Without a flag, new sessions
use Claude and existing sessions remember their agent. Exit the current agent
before switching a session's runtime; its files stay, but each runtime resumes
its own conversation. Missing executables and unknown names fail explicitly.
Dispatch briefs accept all three names in `agent:` and refuse to deliver into
a running session with a different agent.

After syncing, all three receive the same project profile and `needs-you`
instructions. Claude and Codex hooks, and a Pi extension loaded by the launcher,
report a common lifecycle. Review Codex's new hooks with `/hooks` on first start;
untrusted hooks are skipped by Codex. Use a current Codex with lifecycle hooks
and Pi 1.1+ (the `agent_settled` event). Pi question/permission extensions can
emit `kuggis:needs-you` with `{why: "question"}` or `{why: "permission"}` on
`pi.events`; arbitrary third-party dialogs cannot be detected automatically.
For every runtime, `~/.box/needs-you question|permission|blocked|done "one line"`
stays visible until the next user prompt. If Codex stays at `starting`, check
that its hooks are trusted and enabled. Completing a turn alone never pages.

Sync configures the adapters, but does not install agents or change Pi's model,
provider, or authentication. Pi must already have an approved provider on the
VM. Switching runtime grants no new repo or credential access. Agent hooks
report status; they do not enforce cell walls or approve crossings.

Session events are written in shadow mode to `~/.box/events.jsonl`, alongside
the existing state files. Each line follows `vm/job-event.schema.json`: schema version, persistent VM source
ID, locked monotonic sequence, stable session ID, per-launch run ID, agent,
action, and optional reason enum. No prompts, tool payloads, or notification
text enter the log. Reattaching preserves the session ID; closing and reusing a
name creates a new one. Late hooks from an earlier run are ignored.

```sh
kuggis sh forecasting 'python3 -I ~/.box/events.py check'
```

This read-only diagnostic validates and folds the VM log and compares it to
the retained state files; it exits nonzero on rejected events or differences.
Already-running sessions should be restarted after sync to load the adapters.
The dashboard still reads the compatibility files and checks tmux liveness.
This is the session portion of the event-log migration: continuous laptop
transport, the other event types, and retiring old state await the shadow trial.

Adapter interfaces: [Codex hooks](https://learn.chatgpt.com/docs/hooks),
[Claude hooks](https://code.claude.com/docs/en/hooks), and
[Pi extensions](https://pi.dev/docs/latest/extensions).

### Cell display definitions

The UI reads the existing session snapshot and cached development-readiness
observations, **not a typed event log**, and does not enforce cell isolation or
crossing rules. ForecastBench, publishing, releases, isolation audits and seal are not
implemented. Only pending briefs currently produce crossing rows.

Without configuration, familiar project names (or the `airo`/`bigbrain`
profiles) supply display groupings. Unknown projects remain unassigned;
kuggis itself always appears under operator, outside cells. Click a heading
to inspect its read-only definition; inferred groupings are explicitly marked
as having no enforced definition.

Optional private display definitions live in
`~/.config/kuggis/cells/<cell>.json` (or `$KUGGIS_CONFIG/cells`). For example:

```json
{"version": 1, "cell": "forecasting", "projects": ["airo", "agent-box"]}
```

These small JSON files need no extra dependencies and are **grouping and routing metadata**,
separate from the future enforced YAML policy in the plan. Their format is in
`web/cell-display.schema.json`; an optional `definition` object can hold notes
to inspect. The dialog shows the exact file and SHA256, never an enforcement
commit. Explicit membership replaces inference for that cell. Duplicate
membership, invalid files, cell names that collide with CLI commands, and
attempts to put kuggis inside a cell surface as errors. The CLI and web use
the same membership and validation. Configured cells remain visible even
without sessions. The fixed order
is trusted, forecasting, merrillfront-dev, bigbrain-dev, bigbrain-others, hiring,
then custom cells alphabetically, then operator and unassigned.

Keep these definitions private; do not commit personal data or credentials.
No configuration is written by the dashboard. After upgrading kuggis, restart
an already-running `kuggis web` process to load the new server.

The Thinker, an agent that
reads your vault and the web from a contained room on the laptop, is in
`think/README.md`.

What lands on a VM: the Claude token (model requests only; claude.ai
connectors stay off). What never does: a GitHub token (exe.dev's GitHub
integration handles git and `gh` for one repo), your SSH keys, API keys.

`vm/` is copied to `~/.box` on each VM by `kuggis sync`; `profiles/<project>.sh`
holds per-project setup (`box_setup` once, `box_worktree` per session).

Inside a session, the tmux bar shows `project · session` in a colour fixed
per project, any other session on that VM that is waiting on you (`⚑ name 10m`),
and whether the worktree has uncommitted or unpushed work. `✓ pushed` means
`kuggis done` will go through. Terminal tabs are titled `project/session`.

The tool and its laptop config (`~/.config/kuggis`) are `kuggis`. The on-VM
plumbing deliberately kept the `box` prefix — the on-VM dir (`~/.box`), the
`BOX_*` session variables and the `box` VM tag — because renaming those would
have disturbed VMs that already exist. It's invisible from the laptop.

## Recovery

- **Laptop slept, terminal frozen.** The agent kept working on the VM, and
  `kuggis` reattaches on its own once ssh gives up on the dead connection (~45s
  after wake). Press Enter, `~`, `.` to skip the wait. Ctrl-C while it's
  reconnecting stops it; detaching (`C-b d`) leaves without reconnecting.
- **`kuggis` shows a session as `stopped`.** Its process is gone (VM restart, or
  the agent exited). `kuggis <project> <name>` starts it again: the conversation
  resumes (`claude --continue`, `codex resume --last`, or Pi’s session history) and the worktree's files
  are as they were. Whatever command was mid-run is lost, and the agent waits
  for you, so tell it to carry on.
- **`waiting on you`** means a question, a permission prompt, or explicit
  `needs-you`. An ordinary finished turn is idle. You get an
  ntfy ping unless you typed in that session in the last two minutes.
- **`kuggis done` refuses.** The worktree has uncommitted or unpushed work. Push
  it, or pass `--force` to discard it. This repository ignores Python bytecode
  caches so running its tests does not block session cleanup. On an older
  worktree, inspect `git status --short` before removing generated caches.
