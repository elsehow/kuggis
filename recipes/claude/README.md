# Claude Code recipe

```sh
kuggis cell create example --repo owner/repo --default-agent claude --json
kuggis login example --agent claude
kuggis chat open example main --agent claude
```

The wizard installs the checksum-pinned native Claude Code binary alongside
Codex. `release.json` pins the version, the download URL under
`downloads.claude.ai/claude-code-releases/VERSION/linux-x64/claude`, and the
SHA-256 from that release's signed `manifest.json`. `install.py` verifies the
checksum and reported version, installs under `~/.local/lib/claude/VERSION`, and
links `~/.local/bin/claude`. Kuggis owns that launcher link and sets
`DISABLE_UPDATES`, so the pinned release stays selected until the recipe changes.

## Authentication

Claude Code uses its own native login. Kuggis supplies no API key, token or
gateway, so it has no model access until you sign in.

`kuggis login CELL --agent claude` grants `api.anthropic.com`, `claude.ai` and
`platform.claude.com` through the cell proxy (only the proxy restarts) and
reports whether the cell is already signed in. Then open a chat: Claude Code
shows a sign-in link; open it on your laptop and paste the code it gives you
back into the chat. `--status` checks the shared login with `claude auth status`;
`--plan` previews the grants. Use `/login` inside a chat to switch accounts.

Credentials live in the cell user's `~/.claude/.credentials.json`, refreshed by
Claude Code inside the cell. Every Claude Code chat and ordinary guest shell
shares them; nothing is copied from your laptop. Optional endpoints from Claude
Code's [network requirements](https://code.claude.com/docs/en/network-config#network-access-requirements)
(plugin marketplaces, telemetry, claude.ai connectors) are not granted; add any
you need with `kuggis CELL --grant network HOST:443`.

## Chats and authority

The deployment boundary supplies authority: chats run with
`--dangerously-skip-permissions` inside the isolated guest, like the Codex
recipe's unrestricted mode. `settings.json` is merged into `~/.claude/settings.json`,
preserving other settings.

`kuggis-claude CHAT` keeps one explicitly bound session per chat in
`~/.kuggis/agents/claude/CHAT/kuggis-session.json`. A new chat starts with
`--session-id`; a chat with a saved transcript resumes with `--resume`. A native
SessionStart hook, passed through a per-chat `--settings` file, rebinds the chat
on `/clear`, resume and forks. No shared `--continue` is used, so chats in the
same checkout never pick up each other's conversations.

Kuggis writes cell guidance to `~/AGENTS.md`. The installer creates
`~/.claude/CLAUDE.md` importing it when that file does not already exist.

## Add to an existing cell

`admin upgrade` does not install agent recipes. On the operator machine,
download the URL in `release.json` and check its SHA-256, then:

```sh
kuggis file put CELL claude .setup/claude/agent.pkg --replace
for f in release.json settings.json install.py launch.py smoke.py; do
  kuggis file put CELL recipes/claude/$f .setup/claude/$f --replace
done
kuggis cell shell CELL
```

Inside that shell:

```sh
python3 .setup/claude/install.py .setup/claude/agent.pkg
~/.local/bin/claude --version
exit
```

Register the recipe with `kuggis cell configure CELL --file patch.json --restart --bin LINUX_BINARY`,
where the patch adds this entry to the declaration's existing `agents` object:

```json
{"agents": {"claude": {
  "launch": ["/home/agent/.local/bin/kuggis-claude", "default"],
  "chat_launch": ["/home/agent/.local/bin/kuggis-claude", "{chat}"]
}}}
```

Changing `agents` restarts the cell. Existing chats keep their saved agent.

## Test status

Not yet exercised on a live cell. The launcher's session binding, the login
network grants and the creation sequence are covered by unit tests with fake
effects. The download URL pattern, the first-run screens under
`--dangerously-skip-permissions`, and the in-chat sign-in through tmux still
need a live acceptance run. After signing in, run the real model/tool check
inside the guest as `agent`:

```sh
python3 ~/.local/share/kuggis/setup/agents/claude/smoke.py
```
