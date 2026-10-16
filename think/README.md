# The Thinker

An agent that reads your BigBrain vault, the web and your kuggis sessions,
and has no way to send any of it anywhere. It runs on the laptop, as a
separate macOS user, in Claude Code's restricted mode. It thinks and briefs;
you let each brief through.

    kuggis think --setup     build or update the room (asks for your password)
    kuggis think --check     the boundary check: OS level, then a prompt to paste
    kuggis think             the cockpit: the chat, the watcher, the home screen
    kuggis dispatch          decide what waits for you, one request at a time
    kuggis url <link>        vouch for a URL so the Thinker may fetch it

## The door

The room has one way out: **requests**, files the Thinker writes to its
outbox, each with a front-matter `kind`. `kuggis watch` carries them by kind,
and answers in the room's inbox with a **reply** named by the request's
SHA-256. Every decision is logged in `~/.config/kuggis/dispatch.log` with that
hash, so a request that changes after you saw it shows up again.

| kind | rule |
|---|---|
| `brief` | waits for you; then goes into a kuggis session |
| `search` | goes; the URLs it returns become known |
| `fetch` | goes if its URL is known (a search returned it, or you ran `kuggis url`); otherwise waits for you |

Inside the room, `think/door.py serve` offers the same door as MCP tools:
`cells`, `sessions` (every session with its state and last screen, open PRs,
and the Thinker's own waiting requests), `brief`, `outcome`, `search` and
`fetch`. The state comes from `inbox/state.json`, which the watcher rewrites
every few seconds from one round of ssh to the VMs. The one file `door.py`
defines the format and serves both sides of it.

## The cockpit

`kuggis think` opens a tmux session: the chat in one window, the home screen
in the next (`C-b n`), the watcher in a third. The bar turns orange when a
request waits for you. On the home screen a waiting request is a row with a
red dot; Enter opens `kuggis dispatch`, which shows the request in full plus
anything in it that could carry data out (URLs, long opaque strings,
invisible characters, an odd size), then:

- **d** dispatches a brief (uploads it to the project's VM and starts the
  session with it, or types it into the running session) or does a fetch;
- **e** edits a copy of a brief first; your copy is what goes;
- **s** skips it for now; **r** rejects it for good.

Without tmux (`brew install tmux`) the chat runs alone and the watcher behind
it; decide requests with `kuggis dispatch` in another terminal.

A brief is how private facts would reach a public repo, and the VMs have open
internet and no vault, so your read is the check. Merges happen inside the
cells by their own rules; the Thinker doesn't review PRs.

## What holds it

| Layer | What it does |
|---|---|
| The `thinker` user | A standard account. Your home is `chmod 700`, so it can't read your vault, projects, transcripts, keys or keychain. |
| `/opt/kuggis-thinker` | Root-owned: the Claude Code binary, settings, MCP config, the door, the fetch guard and the launcher. Nothing the Thinker writes changes how it starts. |
| `claude --restricted` | Ignores user, project and local settings. Keeps the file tools inside the workspace. Refuses bypassPermissions. |
| `--tools` | Read, Edit, Write, Glob, Grep, WebSearch, WebFetch. No shell. |
| Token login | `kuggis token`'s Claude token. It loads no claude.ai connectors (Gmail, Drive and the rest are each a way out). |
| `fetch-guard` | WebFetch opens a URL only if it came from outside the model, exactly as given. Public hosts only. Fails closed. |
| `--strict-mcp-config` | MCP servers only from the room's `mcp.json`: the door, and BigBrain's once connected. |

## What it can still do

- **Search the web** through the door (a search key in
  `~/.config/kuggis/secrets/thinker.env`, `TAVILY_API_KEY`) or Claude's own
  WebSearch, whose query goes to Anthropic.
- **Fetch pages** at URLs a search returned or you vouched for. It can't
  follow links inside a page by itself.
- **Write in its workspace and its outbox**
  (`/Users/thinker/workspace/outbox`, readable by you).
- Once it's connected, **read the vault and drop into it** through
  BigBrain's door, with its own token. Until BigBrain serves MCP over HTTP in
  a release (elsehow/BigBrain#302), the room has no vault.

A leak that remains: a hostile page offering many links could leak slowly
through which of them gets fetched. It's low bandwidth, and needs links the
Thinker was given.

## Updating

Rerun `kuggis think --setup` after a Claude Code update or a change to
`think/`. It copies your current binary and the room's files; the room never
updates itself.
