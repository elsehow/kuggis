# Codex recipe

```sh
kuggis create example --repo owner/repo --json
kuggis connect example --chat main --agent codex
kuggis connect example --chat research --agent codex
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

`kuggis-codex CHAT` gives each chat a native Codex home under
`~/.kuggis/agents/codex/CHAT/`. It starts a new conversation on first connection,
then uses `codex resume --last --all --include-non-interactive` (so prepared
handoffs are resumable too). Each chat has its own sessions and config;
`/model` choices survive reconnects. Config is seeded once from
`~/.local/share/kuggis/codex/config.toml`. This first recipe uses managed gateway
authentication; shared native OAuth login management is not implemented.

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
