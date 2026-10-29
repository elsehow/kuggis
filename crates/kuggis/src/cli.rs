//! Public command groups. Legacy spellings remain accepted by the main router.
use crate::{Error, Result};

pub const HELP: &str = "Kuggis — cells for autonomous work

  kuggis login CELL --agent codex
  kuggis cell    create, list, status, configure, shell, exec, web
  kuggis chat    list, open, archive
  kuggis access  list, grant, revoke, check, configure
  kuggis file    put, get
  kuggis admin   github, upgrade, debug

Run kuggis GROUP --help for commands and examples.
Existing top-level commands remain compatibility aliases.";

pub const LOGIN_HELP: &str = "kuggis login CELL --agent codex [--no-open] [--force] [--json]
kuggis login CELL --agent codex --status [--json]
kuggis login CELL --agent codex --plan [--json]
Sign in using your laptop browser. All Codex chats and shells in this cell share the login.
Existing chats retain their history. Running chats use it after exiting Codex and reopening.
--no-open prints the browser URL; --force signs in again to switch accounts.
Login grants the provider's HTTPS endpoints and updates the guest launcher without restarting the cell.
CELL accepts a saved name or workspace JSON path.";

pub enum Route {
    Help(&'static str),
    Legacy(Vec<String>),
    Manage(Vec<String>),
}

pub fn group(name: &str) -> bool {
    matches!(name, "cell" | "chat" | "access" | "file" | "admin")
}

pub fn help(name: &str) -> &'static str {
    match name {
        "cell" => "kuggis cell create NAME --repo OWNER/REPO [--github-access read|write]
                   [--host exe] [--default-agent pi|codex] [--model exe-managed]
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
