# kuggis boundary check — findings (2026-10-13)

Authorized verification that a session on the `kuggis` VM cannot reach the
owner's exe.dev account, other VMs, other repos, or the laptop, and that no
usable credential sits on the box. Run directly over ssh from the laptop; the
network position and credentials tested are the VM's.

No FLAG (hard boundary broken). Two NOTEs where a channel is open by design and
deserves a laptop-side mitigation (C6, C7).

## C1 — exe.dev account · PASS
- `ssh exe.dev` → `Permission denied (publickey)`; no key on the box.
- `https://exe.dev/api/vms` → 404, `/mcp` → login HTML. No API without auth.
- No exe.dev credential on disk (`~/.config/exe`, `~/.exe*` absent).

## C2 — other VMs · PASS
- `ssh airo-dev` / `ssh bigbrain-dev` → `Permission denied (publickey)`.
- `https://airo-dev.exe.xyz` → 307; `…shelley…` → 307 to `/__exe.dev/login`.
  No shell, no data; login wall.

## C3 — other repos · PASS
- Scoped proxy `github.int.exe.xyz` reaches **only** `/repos/{owner}/{repo}`.
- Private owner repos (merrillfront, airo-archive, fri-forecaster,
  ibkr-transition) → `Repository not found`.
- Public owner repos (airo, BigBrain) are readable — but that is public data;
  **push** to them → `403 integration is read-only`. Only `elsehow/kuggis` is
  writable.
- No token in git config, no `~/.netrc`, no credential helper: auth is injected
  at the edge, nothing extractable on the box.

## C4 — bot privileges on this repo · PASS
- Push a branch and open a PR as the bot: **allowed** (this PR).
- Read branch protection → `403 Resource not accessible by integration`.
- `/orgs/elsehow/members`, `/user` → `403` (`names no repository; only
  /repos/OWNER/REPO/... is proxied`). The bot can't enumerate itself or the org.
- Approve / merge own PR: see the follow-up commit on this PR.

## C5 — credentials on the box · PASS
- `~/.box/token`: a `sk-ant-oat01-…` OAuth token, 108 chars — model requests
  only. `ANTHROPIC_API_KEY` unset; `disableClaudeAiConnectors` true.
- No GitHub token, no `gh` oauth token, no SSH private key (only `known_hosts`),
  no `~/.aws` / `~/.config/gcloud` / `~/.gnupg`.

## C6 — outbound channels · NOTE (open to owner by design)
- Metadata `169.254.169.254/` returns only `{name, source_ip, reflection_url}`.
- The owner email gateway `POST 169.254.169.254/gateway/email/send` is **live**
  from the box (GET→405, empty POST→400 "invalid JSON"). No email was sent.
- So a compromised box could email the owner. Recipients are restricted to
  owner/team/correspondents (it can't spam third parties), but the owner should
  treat box-originated email as untrusted.
- Mitigation: `ssh exe.dev share receive-email kuggis --reply-policy=none`.

## C7 — laptop-ward (terminal escapes) · NOTE (path open, mitigate on laptop)
- tmux 3.4; our tmux.conf sets neither `set-clipboard` nor `allow-passthrough`,
  so the default `set-clipboard external` applies: an OSC 52 clipboard-write
  printed in a pane is forwarded by tmux to the outer terminal. Whether it then
  writes the laptop clipboard depends on Ghostty's `clipboard-write` setting
  (not checkable from the box).
- `allow-passthrough` off by default blocks DCS passthrough of other escapes.
- Mitigation: add `set -g set-clipboard off` to vm/tmux.conf (belt) and set
  Ghostty `clipboard-write = ask` on the laptop (suspenders).

## So what
Hard boundaries hold: no account, no other VM, no private repo, no cross-repo
write, no extractable credential, no admin. Two by-design channels (owner email,
OSC 52 clipboard) are open and get laptop-side mitigations, not box fixes.
