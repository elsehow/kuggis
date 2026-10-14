<!-- box -->
# You're on a box

This machine is an exe.dev VM for one project, {{REPO_SLUG}}, driven from
the user's laptop with `kuggis`. Nothing personal lives here: no email,
calendar, Drive, browser sign-ins or BigBrain vault. If a task needs them,
say so and stop instead of looking for a way in.

- **Your worktree is your session.** `kuggis` started you in
  `{{REPO}}/.claude/worktrees/<session>` on a branch of the same name. That
  satisfies any "one session, one worktree" rule in the project's own
  CLAUDE.md, so don't make another. Never edit `{{REPO}}` itself; it stays
  parked on the default branch.
- **GitHub goes through exe.dev.** The remote is
  `https://github.int.exe.xyz/{{REPO_SLUG}}.git`, and `gh` works because
  `GH_HOST=github.int.exe.xyz`. There is no GitHub token on this machine,
  and access covers this one repo.
- **Uploads land in `~/inbox`.** When the user pastes a path from there,
  read it.
- **Finishing.** Push the branch and open a PR, then tell the user it's
  ready. You act on GitHub as exe.dev's bot and `main` requires the user's
  approval, so you can't merge it; don't look for a way around that. The
  user ends the session with `kuggis done`, which refuses while work is
  uncommitted or unpushed.
- **Say when you need the user.** When you stop to ask something, can't
  go on without them, or are done, run
  `~/.box/needs-you question|blocked|done "one line"` before you end the
  turn. That marks the session as waiting on them and pings their phone.
  Don't when you're only waiting on your own work (a run, a build, a
  monitor): end the turn, and carry on when it finishes.
- **Commits carry a `Box:` trailer** naming this VM and session, added by
  a git hook. Leave it in place.
<!-- /box -->
