# The Thinker

An agent that reads your BigBrain vault and the web, and has no way to send
either anywhere. It runs on the laptop, as a separate macOS user, in Claude
Code's restricted mode. It thinks; you act. What it wants done lands in its
outbox for you to read.

    kuggis think --setup     build or update the room (asks for your password)
    kuggis think --check     the boundary check: OS level, then a prompt to paste
    kuggis think             enter the room

## What holds it

| Layer | What it does |
|---|---|
| The `thinker` user | A standard account. Your home is `chmod 700`, so it can't read your vault, projects, transcripts, keys or keychain. |
| `/opt/kuggis-thinker` | Root-owned: the Claude Code binary, settings, MCP config, the fetch guard and the launcher. Nothing the Thinker writes changes how it starts. |
| `claude --restricted` | Ignores user, project and local settings. Keeps the file tools inside the workspace. Refuses bypassPermissions. |
| `--tools` | Read, Edit, Write, Glob, Grep, WebSearch, WebFetch. No shell, so there's nothing to sandbox. |
| Token login | `kuggis token`'s Claude token. It loads no claude.ai connectors (Gmail, Drive and the rest are each a way out). |
| `fetch-guard` | WebFetch opens a URL only if it came from outside the model, exactly as given: a WebSearch result's links, a BigBrain note, or your message. Public hosts only. Fails closed. |
| `--strict-mcp-config` | MCP servers only from the room's `mcp.json`. |

## What it can still do

- **Search the web.** The query goes to Anthropic's search, not to an
  attacker.
- **Fetch pages** at URLs it was given. It can't follow links inside a page.
  WebFetch's result is a model's summary written to the Thinker's own prompt,
  so it could echo a URL the Thinker made up. Paste the link instead.
- **Write in its workspace and its outbox**
  (`/Users/thinker/workspace/outbox`, readable by you).
- Once it's connected, **read the vault and drop into it** through
  BigBrain's door, with its own token. Until BigBrain serves MCP over HTTP
  (elsehow/BigBrain#302), the room has no vault: the stdio server would
  need the vault on disk.

A leak that remains: a hostile page offering many links could leak slowly
through which of them gets fetched. It's low bandwidth, and needs links the
Thinker was given.

## Updating

Rerun `kuggis think --setup` after a Claude Code update. It copies your
current binary into the room; the room's copy never updates itself.
