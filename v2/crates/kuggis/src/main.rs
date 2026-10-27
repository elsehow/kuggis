use kuggis::{
    client,
    config::{GuestPath, Name},
    deploy::Bundle,
    environment::Environment,
    ssh::{Invocation, Request},
    Error, Workspace,
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

const USAGE: &str = "kuggis v2 — configured autonomous workspaces

  kuggis create [NAME] [--repo OWNER/REPO] [--host exe] [--default-agent pi|codex]
                [--github-access read|write] [--model exe-managed]
                [--file SETUP_JSON] [--json] [--plan]
                [--cpus N] [--memory-gib N]
                [--ssh-config FILE] [--bin LINUX_BINARY]
  kuggis exec CONFIG_OR_NAME -- PROGRAM [ARG...]
  kuggis web CONFIG_OR_NAME [PREVIEW] [--port PORT] [--no-open] [--plan]
  kuggis validate CONFIG
  kuggis render CONFIG
  kuggis bundle CONFIG
  kuggis setup CONFIG --bin LINUX_BINARY
  kuggis connect CONFIG_OR_NAME [--chat NAME] [--agent AGENT]
  kuggis connect CONFIG_OR_NAME --json
  kuggis shell CONFIG
  kuggis check CONFIG
  kuggis put CONFIG LOCAL_FILE GUEST_PATH [--replace]
  kuggis get CONFIG GUEST_PATH LOCAL_FILE [--replace]
  kuggis ssh-command CONFIG connect|shell|check
  kuggis ssh-command CONFIG connect [--chat NAME] [--agent AGENT]
  kuggis ssh-command CONFIG put GUEST_PATH [--replace]
  kuggis ssh-command CONFIG get GUEST_PATH

Host services: proxy CONFIG; normalize-nft (JSON on stdin).
render, bundle and ssh-command are descriptions, not live boundary checks.
Setup needs a dedicated Ubuntu 24.04 host and preconfigured SSH authentication.";

enum Operation {
    Chats(bool),
    Validate,
    Render,
    Bundle,
    Setup(PathBuf),
    Remote(Request),
    Put {
        local: PathBuf,
        guest: GuestPath,
        replace: bool,
    },
    Get {
        local: PathBuf,
        guest: GuestPath,
        replace: bool,
    },
    SshCommand(Request),
    Proxy,
    Web(kuggis::web::Options),
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.first().is_some_and(|s| s == "create") && args.iter().any(|s| s == "--json");
    if let Err(error) = run(args) {
        if json {
            println!(
                "{}",
                serde_json::json!({"ok": false, "error": {"code": "invalid_request", "message": error.to_string()}})
            );
        } else {
            eprintln!("kuggis: {error}");
        }
        std::process::exit(2);
    }
}

fn run(args: Vec<String>) -> kuggis::Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["help"] || args == ["create", "--help"] {
        println!("{USAGE}");
        return Ok(());
    }
    if args[0] == "create" {
        return kuggis::onboarding::create(&args[1..]);
    }
    if args == ["normalize-nft"] {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(Error("nftables report too large".into()));
        }
        return print_json(&kuggis::deploy::normalize_nft(&serde_json::from_slice(
            &bytes,
        )?)?);
    }
    let config = args.get(1).ok_or_else(|| Error(USAGE.into()))?;
    let operation = parse_operation(&args[0], &args[2..])?;
    let workspace = load(&kuggis::onboarding::config_path(config)?)?;
    match operation {
        Operation::Chats(json) => client::chats(&workspace, config, json)?,
        Operation::Validate => println!(
            "{}: valid v2 configuration (not installed)",
            workspace.name().as_str()
        ),
        Operation::Render => print_json(&Environment::describe(&workspace))?,
        Operation::Bundle => print_json(&Bundle::compile(&workspace))?,
        Operation::Setup(binary) => client::setup(&workspace, &binary)?,
        Operation::Remote(request) => client::enter(&workspace, request)?,
        Operation::Put {
            local,
            guest,
            replace,
        } => client::put(&workspace, &local, guest, replace)?,
        Operation::Get {
            local,
            guest,
            replace,
        } => client::get(&workspace, guest, &local, replace)?,
        Operation::SshCommand(request) => {
            print_json(&Invocation::for_request(&workspace, &request))?
        }
        Operation::Proxy => kuggis::proxy::serve(workspace)?,
        Operation::Web(options) => kuggis::web::run(&workspace, &options)?,
    }
    Ok(())
}

fn load(path: &Path) -> kuggis::Result<Workspace> {
    let mut json = String::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut json)?;
    if json.len() > 1024 * 1024 {
        return Err(Error("configuration exceeds 1 MiB".into()));
    }
    Workspace::parse(&json)
}

fn parse_operation(verb: &str, args: &[String]) -> kuggis::Result<Operation> {
    let path = |value: &String| GuestPath::try_from(value.clone());
    match (verb, args) {
        ("validate", []) => Ok(Operation::Validate),
        ("render", []) => Ok(Operation::Render),
        ("bundle", []) => Ok(Operation::Bundle),
        ("proxy", []) => Ok(Operation::Proxy),
        ("web", rest) => Ok(Operation::Web(kuggis::web::Options::parse(rest)?)),
        ("setup", [flag, binary]) if flag == "--bin" => Ok(Operation::Setup(binary.into())),
        ("connect", []) => Ok(Operation::Chats(false)),
        ("connect", [flag]) if flag == "--json" => Ok(Operation::Chats(true)),
        ("connect", rest) => Ok(Operation::Remote(connect_request(rest)?)),
        ("shell", []) => Ok(Operation::Remote(Request::Shell)),
        ("check", []) => Ok(Operation::Remote(Request::Check)),
        ("exec", [separator, program, rest @ ..]) if separator == "--" => {
            if program.is_empty()
                || program.starts_with('-')
                || args.iter().any(|s| s.contains('\0'))
            {
                return Err(Error("expected a guest executable and argv".into()));
            }
            let mut argv = vec![program.clone()];
            argv.extend_from_slice(rest);
            Ok(Operation::Remote(Request::Exec(argv)))
        }
        ("ssh-command", rest) => Ok(Operation::SshCommand(parse_request(rest)?)),
        ("put" | "get", [first, second]) => {
            if verb == "put" {
                Ok(Operation::Put {
                    local: first.into(),
                    guest: path(second)?,
                    replace: false,
                })
            } else {
                Ok(Operation::Get {
                    local: second.into(),
                    guest: path(first)?,
                    replace: false,
                })
            }
        }
        ("put" | "get", [first, second, flag]) if flag == "--replace" => {
            if verb == "put" {
                Ok(Operation::Put {
                    local: first.into(),
                    guest: path(second)?,
                    replace: true,
                })
            } else {
                Ok(Operation::Get {
                    local: second.into(),
                    guest: path(first)?,
                    replace: true,
                })
            }
        }
        _ => Err(Error(USAGE.into())),
    }
}

fn parse_request(args: &[String]) -> kuggis::Result<Request> {
    let path = |value: &String| GuestPath::try_from(value.clone());
    match args {
        [verb, rest @ ..] if verb == "connect" => connect_request(rest),
        [verb] if verb == "shell" => Ok(Request::Shell),
        [verb] if verb == "check" => Ok(Request::Check),
        [verb, value] if verb == "put" => Ok(Request::Put {
            path: path(value)?,
            replace: false,
        }),
        [verb, value, flag] if verb == "put" && flag == "--replace" => Ok(Request::Put {
            path: path(value)?,
            replace: true,
        }),
        [verb, value] if verb == "get" => Ok(Request::Get { path: path(value)? }),
        _ => Err(Error(USAGE.into())),
    }
}

fn connect_request(args: &[String]) -> kuggis::Result<Request> {
    let (mut chat, mut agent) = (None, None);
    let mut words = args.iter();
    while let Some(flag) = words.next() {
        let value = words
            .next()
            .ok_or_else(|| Error("expected a name after --chat or --agent".into()))?;
        let target = match flag.as_str() {
            "--chat" if chat.is_none() => &mut chat,
            "--agent" if agent.is_none() => &mut agent,
            _ => {
                return Err(Error(
                    "expected connect [--chat NAME] [--agent AGENT]".into(),
                ))
            }
        };
        *target = Some(Name::try_from(value.clone())?);
    }
    Ok(match (chat, agent) {
        (Some(chat), Some(agent)) => Request::SelectChat { chat, agent },
        (None, Some(_)) => {
            return Err(Error(
                "--agent requires --chat NAME; bare connect lists chats".into(),
            ))
        }
        (Some(chat), None) => Request::Chat(chat),
        (None, None) => Request::Chats,
    })
}

fn print_json(value: &impl serde::Serialize) -> kuggis::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    out.write_all(b"\n")?;
    Ok(())
}
