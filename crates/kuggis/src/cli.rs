//! Positional cell/chat commands, with compatible grouped and legacy spellings.
use crate::{Error, Result};

pub const HELP: &str = "Kuggis — cells for autonomous work

  kuggis CELL CHAT [--agent AGENT]     open or resume a terminal chat
  kuggis CELL CHAT archive [--json]    stop and archive a chat
  kuggis CELL [--list] [--json]        list chats without starting an agent
  kuggis --list [--json]              list saved cells
  kuggis --create NAME [OPTIONS]      create a cell
  kuggis CELL --put LOCAL [REMOTE] [--chat CHAT] [--replace] [--json]
  kuggis CELL --put --paste [--chat CHAT] [--json]
  kuggis CELL --get REMOTE LOCAL [--replace] [--json]
  kuggis CELL --shell                 open a guest shell
  kuggis CELL --exec -- PROGRAM [ARG...]
  kuggis CELL --status [--check] [--json]
  kuggis CELL --login --agent codex|claude
  kuggis CELL --web [PREVIEW] [--port PORT] [--no-open] [--plan]
  kuggis CELL --configure --file PATCH.json [--plan] [--restart --bin BIN]
  kuggis CELL --upgrade --bin BIN [--plan] [--restart] [--json]
  kuggis CELL --access list|grant|revoke|check|configure [OPTIONS]
  kuggis CELL --grant repo OWNER/REPO --role read|develop [--plan] [--json]
  kuggis CELL --grant network HOST:PORT [--plan] [--json]
  kuggis CELL --revoke repo OWNER/REPO [--plan] [--json]
  kuggis CELL --revoke network HOST:PORT [--plan] [--json]

Opening a new chat creates it; opening an existing chat resumes it over SSH.
CELL accepts a saved name or workspace JSON path. Chat names are exact names.

Compatible command groups:
  kuggis login CELL --agent codex|claude
  kuggis cell    create, list, status, configure, shell, exec, web
  kuggis chat    list, open, archive
  kuggis access  list, grant, revoke, check, configure
  kuggis file    put, get
  kuggis admin   github, upgrade, debug

Run kuggis GROUP --help for commands and examples.
Existing top-level commands remain compatibility aliases.";

pub const LOGIN_HELP: &str = "kuggis login CELL --agent codex|claude [--no-open] [--force] [--json]
kuggis login CELL --agent codex|claude --status [--json]
kuggis login CELL --agent codex|claude --plan [--json]
Every agent uses its own native login; a new cell has no model access until you sign in.
All chats and shells of that agent in the cell share the login. Existing chats retain their history.
codex: sign in using your laptop browser. Running chats use it after exiting Codex and reopening.
  --no-open prints the browser URL; --force signs in again to switch accounts.
claude: grants Claude Code's endpoints, then you finish its sign-in inside a chat (link and pasted code).
Login grants the provider's HTTPS endpoints without restarting the cell.
CELL accepts a saved name or workspace JSON path.";

pub enum Route {
    Help(&'static str),
    Legacy(Vec<String>),
    Manage(Vec<String>),
}

pub fn group(name: &str) -> bool {
    matches!(name, "cell" | "chat" | "access" | "file" | "admin")
}

/// These words retain their original meaning at the start of a command.
/// For a cell with a colliding name, use its workspace JSON path.
pub fn legacy_command(name: &str) -> bool {
    matches!(
        name,
        "login"
            | "create"
            | "github"
            | "repo"
            | "normalize-nft"
            | "validate"
            | "render"
            | "bundle"
            | "setup"
            | "connect"
            | "archive"
            | "shell"
            | "check"
            | "exec"
            | "ssh-command"
            | "put"
            | "get"
            | "proxy"
            | "web"
    )
}

fn positional(args: &[String]) -> Result<Route> {
    let words = |prefix: &[&str], rest: &[String]| {
        prefix
            .iter()
            .map(|s| s.to_string())
            .chain(rest.iter().cloned())
            .collect()
    };
    let Some(cell) = args.first() else {
        return Ok(Route::Help(HELP));
    };
    if cell == "--list" {
        if args[1..].iter().any(|s| s != "--json") || args.len() > 2 {
            return Err(Error("expected kuggis --list [--json]".into()));
        }
        return Ok(Route::Manage(words(&["cell", "list"], &args[1..])));
    }
    if cell == "--create" {
        return Ok(Route::Legacy(words(&["create"], &args[1..])));
    }
    if cell.starts_with('-') {
        return Err(Error(format!(
            "unknown operation {cell}; run kuggis --help"
        )));
    }
    let Some(operation) = args.get(1).map(String::as_str) else {
        return Ok(Route::Legacy(words(&["connect", cell], &[])));
    };
    let rest = &args[2..];
    match operation {
        "--help" | "-h" => Ok(Route::Help(HELP)),
        "--list" => {
            if rest.len() > 1 || rest.first().is_some_and(|s| s != "--json") {
                return Err(Error("expected kuggis CELL --list [--json]".into()));
            }
            Ok(Route::Legacy(words(&["connect", cell], rest)))
        }
        "--json" if rest.is_empty() => Ok(Route::Legacy(words(&["connect", cell, "--json"], rest))),
        "--put" | "--get" => Ok(Route::Manage(words(&["file", &operation[2..], cell], rest))),
        "--shell" | "--exec" | "--web" | "--login" => {
            Ok(Route::Legacy(words(&[&operation[2..], cell], rest)))
        }
        "--status" | "--configure" => {
            Ok(Route::Manage(words(&["cell", &operation[2..], cell], rest)))
        }
        "--check" => Ok(Route::Manage(words(
            &["cell", "status", cell, "--check"],
            rest,
        ))),
        "--upgrade" => Ok(Route::Manage(words(&["admin", "upgrade", cell], rest))),
        "--access" => {
            let action = rest.first().ok_or_else(|| Error(help("access").into()))?;
            route(&words(&["access", action, cell], &rest[1..]))
        }
        "--grant" | "--revoke" => route(&words(&["access", &operation[2..], cell], rest)),
        flag if flag.starts_with('-') => Err(Error(format!(
            "unknown cell operation {flag}; run kuggis CELL --help"
        ))),
        chat => {
            crate::config::Name::try_from(chat.to_owned())?;
            match rest.first().map(String::as_str) {
                Some("archive") => Ok(Route::Manage(words(
                    &["chat", "archive", cell, chat],
                    &rest[1..],
                ))),
                Some("open") => Ok(Route::Legacy(words(
                    &["connect", cell, "--chat", chat],
                    &rest[1..],
                ))),
                Some("--help" | "-h") => Ok(Route::Help(help("chat"))),
                Some(action) if !action.starts_with('-') => Err(Error(format!(
                    "unknown chat action {action}; supported actions: open, archive"
                ))),
                _ => Ok(Route::Legacy(words(
                    &["connect", cell, "--chat", chat],
                    rest,
                ))),
            }
        }
    }
}

pub fn help(name: &str) -> &'static str {
    match name {
        "cell" => "kuggis cell create NAME --repo OWNER/REPO [--github-access read|write]
                   [--host exe] [--default-agent codex|claude]
                   [--cpus N] [--memory-gib N] [--file SETUP.json]
                   [--ssh-config FILE] [--bin LINUX_BINARY] [--plan] [--json] [--restart]
kuggis cell list [--json]
kuggis cell status CELL [--check] [--json]
kuggis cell configure CELL --file PATCH.json [--plan] [--restart --bin LINUX_BINARY] [--json]
kuggis cell shell CELL
kuggis cell exec CELL -- PROGRAM [ARG...]
kuggis cell web CELL [PREVIEW] [--port PORT] [--no-open] [--plan]
CELL accepts a saved name or a workspace JSON path.
configure merges a JSON object into live settings; arrays replace the specified array.
Plans report restart requirements. A cell restart requires --restart.",
        "chat" => "kuggis chat list CELL [--json]
kuggis chat open CELL CHAT [--agent AGENT]
kuggis chat archive CELL CHAT [--json]
Opening a new chat creates it; opening an existing chat resumes it.",
        "access" => "kuggis access list CELL [--json]
kuggis access grant CELL repo OWNER/REPO --role read|develop [--plan] [--json]
kuggis access grant CELL network HOST:PORT [--plan] [--json]
kuggis access revoke CELL repo OWNER/REPO [--plan] [--json]
kuggis access revoke CELL network HOST:PORT [--plan] [--json]
kuggis access check CELL repo OWNER/REPO [--json]
kuggis access check CELL network HOST:PORT [--json]
kuggis access configure CELL repo OWNER/REPO --issues none|read|write --pull-requests none|read|write [--merge] [--json]
Repository roles set Git, PR and issue access together; API merging is disabled.
Network grants allow one public destination/port, without adding credentials.",
        "file" => "kuggis file put CELL LOCAL_FILE [GUEST_PATH] [--chat CHAT] [--replace] [--json]
kuggis file put CELL --paste [--chat CHAT] [--json]
kuggis file get CELL GUEST_PATH LOCAL_FILE [--replace] [--json]",
        _ => "kuggis admin github connect --url HTTPS_ORIGIN --admin-token-file FILE
kuggis admin github attach CELL
kuggis admin github api-status
kuggis admin github status
kuggis admin github check --repo OWNER/REPO --access read|write
kuggis admin github serve --config BROKER_JSON
kuggis admin upgrade CELL --bin LINUX_BINARY [--plan] [--restart] [--json]
kuggis admin debug validate|render|bundle CELL
kuggis admin debug ssh-command CELL REQUEST [ARG...]
Shared broker setup stays on the operator machine. Upgrade plans require a live host.",
    }
}

pub fn route(args: &[String]) -> Result<Route> {
    let area = args.first().map(String::as_str).unwrap_or("");
    if !group(area) {
        return positional(args);
    }
    if args.len() == 1
        || args
            .get(1)
            .is_some_and(|s| matches!(s.as_str(), "--help" | "-h"))
    {
        return Ok(Route::Help(help(area)));
    }
    let action = args[1].as_str();
    if args
        .iter()
        .skip(2)
        .take_while(|s| s.as_str() != "--")
        .any(|s| s == "--help" || s == "-h")
    {
        return Ok(Route::Help(help(area)));
    }
    let legacy = |verb: &str, rest: &[String]| {
        Route::Legacy(
            std::iter::once(verb.to_owned())
                .chain(rest.iter().cloned())
                .collect(),
        )
    };
    match (area, action) {
        ("cell", "create" | "shell" | "exec" | "web") => Ok(legacy(action, &args[2..])),
        ("cell", "list" | "status" | "configure")
        | ("access", _)
        | ("file", "put" | "get")
        | ("chat", "archive")
        | ("admin", "upgrade") => Ok(Route::Manage(args.to_vec())),
        ("chat", "list") => {
            let rest = args[2..].to_vec();
            // A plain chat list must never enter a terminal session.
            if rest.len() > 2 || rest.get(1).is_some_and(|s| s != "--json") {
                return Err(Error(help(area).into()));
            }
            Ok(legacy("connect", &rest))
        }
        ("chat", "open") if args.len() >= 4 => {
            let mut rest = vec![args[2].clone(), "--chat".into(), args[3].clone()];
            rest.extend_from_slice(&args[4..]);
            Ok(legacy("connect", &rest))
        }
        ("admin", "github") => Ok(legacy("github", &args[2..])),
        ("admin", "debug")
            if args.get(2).is_some_and(|s| {
                matches!(s.as_str(), "validate" | "render" | "bundle" | "ssh-command")
            }) =>
        {
            Ok(Route::Legacy(args[2..].to_vec()))
        }
        _ => Err(Error(help(area).into())),
    }
}
