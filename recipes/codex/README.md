# Codex recipe

```sh
kuggis cell create example --repo owner/repo --json
kuggis login example --agent codex
kuggis chat open example main --agent codex
kuggis chat open example research --agent codex
```

The wizard installs Pi and the checksum-pinned **complete** Codex package, including its
code-mode host. Installing only the standalone executable can produce fluent
answers while tool execution fails. The readiness check requires a successful
shell tool call and independently verifies the resulting file.

The deployment boundary supplies authority: native Codex uses `approval_policy =
"never"` and `sandbox_mode = "danger-full-access"` inside the isolated guest. The
recipe records native trust for the configured workspace and routes Responses
requests through exe's managed gateway; no laptop login
or management credentials are copied. Native web search is initially disabled;
shell-based fetching follows the cell's configured network policy.

Run `kuggis login CELL --agent codex` on your laptop to use your ChatGPT account.
Kuggis opens the normal browser sign-in and relays its localhost callback over
SSH into the guest. Device codes and manual callback copying are unnecessary.
It grants `auth.openai.com:443`, `chatgpt.com:443`, and the exact public workspace
backend returned by native account discovery. Only the outbound proxy restarts
when grants change; the cell and chat processes stay running. `--plan` previews
the change, `--no-open` prints the browser URL, `--status` checks the shared login,
and `--force` signs in again to switch accounts. A cached ChatGPT login is reused.

All chats and ordinary guest shells use the cell user's `~/.codex` for native
authentication. Codex owns credential storage and refresh; credentials never
need to be copied from your laptop or into individual chats. Login selects the
native `openai` provider for the cell. Before login, the recipe still supports
the managed exe gateway.

`kuggis-codex CHAT` retains each chat's native databases under
`~/.kuggis/agents/codex/CHAT/` using Codex's `sqlite_home` setting. Existing
rollouts, paginated histories, credentials and legacy config files are retained
in place. Per-chat settings are copied once to native profiles at
`~/.codex/kuggis-CHAT.config.toml`; `/model` choices continue to persist. An
explicit session ID binds each chat, and a native SessionStart hook updates it
on new conversations, resume, `/new`, and forks. No shared `resume --last` is used.
Chats run with `--no-daemon` so their native profile and database settings stay
independent. The hook runs without another trust prompt inside the existing
guest boundary, using the same unrestricted guest authority as the agent.

Login installs this launcher without stopping active chats. Exit Codex and
reopen an already-running chat to adopt the shared account; attaching to its
existing tmux pane does not replace the running process. The first handover
rechecks its legacy session ID so work done after login setup is retained.
The old launcher and shared configuration are backed up before replacement.

`models.json` is a filtered copy of the official
[0.165.0 bundled catalog](https://github.com/openai/codex/blob/rust-v0.165.0/codex-rs/models-manager/models.json).
It keeps Sol 6.1, Terra 6.1, Astra 6, and Sol 6; preserves their upstream
instructions and capabilities; and clears upgrade and service-tier suggestions.
Those routes were advertised by the acceptance gateway. The file is a pinned
snapshot, not live account discovery. Recheck gateway availability when updating
the recipe. See OpenAI's [gateway configuration](https://learn.chatgpt.com/docs/enterprise/connect-to-a-gateway)
and [catalog specification](https://learn.chatgpt.com/docs/model-catalogs).

For an existing SSH cell, install the recipe as `agent` inside the guest, using
the package specified in `release.json`. Adapt the gateway URL in `config.toml`
to the deployment environment before installation. Register this recipe in the
workspace declaration, preserving the existing Pi recipe exactly:

```json
"codex": {
  "launch": ["/home/agent/.local/bin/kuggis-codex", "default"],
  "chat_launch": ["/home/agent/.local/bin/kuggis-codex", "{chat}"]
}
```

When adding `agents` to a legacy configuration, Kuggis requires one recipe whose
launch arguments exactly match the installed default and named launch recipes.
It binds the existing default and Pi history directories to that recipe before
applying a new default. Existing conversations therefore keep their agent.

Pi-to-Codex migration is an explicit operator operation: finish the active turn,
snapshot the Pi JSONL, write a handoff with source identity and checksum, seed a
native Codex session, and only then replace the saved chat binding. Keep the
original transcript. This transfers working context; it does not import a Pi
transcript as a native Codex history. Never rewrite a binding while its old
agent is still working.
