//! `cell`: the typed core on the command line. Every subcommand is a thin
//! call into the library; the library holds every check.

use cell::apply::{Edge, Fallible};
use cell::door::{Door, Knock};
use cell::exe::{Integration, Listing, Observed, Ssh};
use cell::grant::Provider;
use cell::proxy::{system_resolver, Policy, Proxy};
use cell::{Approved, Cell, Hash, Key, Name, Plan, Said, Secret, Statement, Tree};
use cell::{RequestId, SessionId, SessionRef};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "\
usage:
  cell key new PATH               mint a secret at PATH (0600); print its public key
  cell key pub PATH               print the public key of the secret at PATH
  cell say CELL SEQ SECRET        sign the Said on stdin as CELL, sequence SEQ
  cell open KEY                   verify the Statement on stdin under KEY; print what it said
  cell allow CELLFILE             print the hosts a cell's proxy may tunnel to
  cell tree DIR                   check DIR as one tree: DIR/x.json sits under
                                  DIR's own cell, DIR/x/ holds x's children
  cell plan DIR [NAME] [--bin LINUX_CELL] [--have OBSERVED [--box HASH]]
                                  plan every cell in the tree at DIR, or NAME alone,
                                  against exe.dev and the boxes (or against OBSERVED
                                  as `cell observe` wrote it); print the plans and
                                  the hash that approves them; with LINUX_CELL, a box
                                  running another binary needs setup
  cell apply DIR [NAME] --approve HASH [--bin LINUX_CELL] [--pins DIR]
                                  run the approved plans one action at a time,
                                  re-planning before each; LINUX_CELL is shipped to
                                  a box for setup (give plan the same one); keys are
                                  pinned in DIR/.pins
  cell adopt VM NAME              tag an existing VM as NAME's root
  cell observe [--from DIR]       read exe.dev (or DIR/ls.json + DIR/int.json) as
                                  cells and doors
  cell proxy --listen ADDR --cell CELLFILE [--private-ok]
                                  run the wall's proxy for that cell
  cell setup CELLFILE             as root on a box: build the wall for that cell,
                                  then check it and sign what was seen
  cell check                      as root on a box: probe the wall as the agent,
                                  sign the result as Checked, append it to the log
  cell probe                      as the agent on a box: try everything it must
                                  not be able to do; one JSON line per probe
  cell door --listen ADDR --cell CELLFILE --said DIR --inbox DIR
                                  serve the statement log; file what a parent says
  cell ship --cell CELLFILE --secret FILE --outbox DIR --said DIR [--once]
                                  sign what the agent drops in the outbox into the log
  cell knock --inbox DIR          as root on a box: file the Said on stdin in the inbox
  cell session new                generate a stable session ID
  cell brief DIR NAME SESSION_ID TEXT [--request REQUEST_ID]
                                  brief NAME's SESSION, over ssh, as you
  cell pull DIR NAME [--since N]  pull NAME's log, verify it under its pinned key, print it
  cell login DIR NAME             open Pi on NAME's box as the agent, to sign in
  cell ui TREE [--bin LINUX_CELL] [--listen 127.0.0.1:8080]
                                  browse cells and their chats over SSH
  cell chat --local DIR [--listen 127.0.0.1:8080] [--state DIR] [--assets DIR] [--pi PATH]
                                  serve the React chat UI with persistent local Pi sessions
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("cell: {e}");
        std::process::exit(2);
    }
}

fn run(args: &[String]) -> Fallible {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("key") => key(rest),
        Some("say") => say(rest),
        Some("open") => open(rest),
        Some("allow") => allow(rest),
        Some("tree") => tree(rest),
        Some("plan") => plan(rest),
        Some("apply") => apply(rest),
        Some("adopt") => adopt(rest),
        Some("observe") => observe(rest),
        Some("proxy") => proxy(rest),
        Some("setup") => setup(rest),
        Some("check") => checked(cell::wall::check()?),
        Some("probe") => probe(),
        Some("door") => door(rest),
        Some("ship") => ship(rest),
        Some("knock") => knock(rest),
        Some("brief") => brief(rest),
        Some("session") if rest == ["new"] => {
            println!("{}", SessionId::generate());
            Ok(())
        }
        Some("pull") => pull(rest),
        Some("login") => login(rest),
        Some("chat") => chat(rest),
        Some("ui") => ui(rest),
        Some("relay") => {
            cell::operator::relay(Path::new(
                flag(rest, "--socket").unwrap_or("/run/cell-chat/api.sock"),
            ))?;
            Ok(())
        }
        Some("prepare") => {
            println!(
                "{}",
                cell::wall::prepare_pi(&load_cell(arg(rest, 0, "CELLFILE")?)?)?.display()
            );
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            Err("no such command".into())
        }
    }
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> Fallible<&'a str> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| format!("missing {what}").into())
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn chat(args: &[String]) -> Fallible {
    use cell::chat::ExecutionMode;
    let required = |flag_name| {
        flag(args, flag_name)
            .map(PathBuf::from)
            .ok_or_else(|| format!("cell chat requires {flag_name} PATH"))
    };
    let declaration = flag(args, "--cell").map(load_cell).transpose()?;
    let (mode, workspace) = if let Some(declaration) = declaration {
        if flag(args, "--local").is_some() || flag(args, "--listen").is_some() {
            return Err("cell chat uses --workspace and a private --socket; --local/--listen are local-only".into());
        }
        (
            ExecutionMode::Cell {
                declaration,
                socket: required("--socket")?,
                agent_dir: required("--agent-dir")?,
                inbox: required("--inbox")?,
                outbox: required("--outbox")?,
            },
            required("--workspace")?,
        )
    } else {
        for name in [
            "--socket",
            "--agent-dir",
            "--inbox",
            "--outbox",
            "--workspace",
        ] {
            if flag(args, name).is_some() {
                return Err(format!("{name} requires --cell").into());
            }
        }
        (
            ExecutionMode::Local {
                listen: flag(args, "--listen").unwrap_or("127.0.0.1:8080").parse()?,
            },
            PathBuf::from(
                flag(args, "--local")
                    .ok_or("chat requires --local DIR or --cell FILE --workspace DIR")?,
            ),
        )
    };
    let state = flag(args, "--state")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join(".kuggis/chat"));
    let config = cell::chat::Config {
        mode,
        workspace,
        state,
        assets: flag(args, "--assets").unwrap_or("web/dist").into(),
        pi: flag(args, "--pi").unwrap_or("pi").into(),
    };
    tokio::runtime::Runtime::new()?.block_on(cell::chat::serve(config))
}

fn ui(args: &[String]) -> Fallible {
    let config = cell::operator::Config {
        tree: PathBuf::from(arg(args, 0, "TREE")?),
        state: flag(args, "--state").unwrap_or(".kuggis/operator").into(),
        assets: flag(args, "--assets").unwrap_or("web/dist").into(),
        listen: flag(args, "--listen").unwrap_or("127.0.0.1:8080").parse()?,
        binary: flag(args, "--bin").map(PathBuf::from),
        sockets: flag(args, "--socket-dir").map(PathBuf::from),
    };
    tokio::runtime::Runtime::new()?.block_on(cell::operator::serve(config))
}

fn stdin_json<T: serde::de::DeserializeOwned>() -> Fallible<T> {
    let mut s = String::new();
    io::stdin().read_to_string(&mut s)?;
    Ok(serde_json::from_str(&s)?)
}

fn load_cell(path: impl AsRef<Path>) -> Fallible<Cell> {
    let path = path.as_ref();
    let s = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn key(args: &[String]) -> Fallible {
    match arg(args, 0, "new|pub")? {
        "new" => {
            let path = arg(args, 1, "PATH")?;
            let secret = Secret::generate();
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            writeln!(f, "{secret}")?;
            println!("{}", secret.public());
        }
        "pub" => {
            let secret: Secret = fs::read_to_string(arg(args, 1, "PATH")?)?.parse()?;
            println!("{}", secret.public());
        }
        other => return Err(format!("key {other}: no such subcommand").into()),
    }
    Ok(())
}

fn say(args: &[String]) -> Fallible {
    let cell: Name = arg(args, 0, "CELL")?.parse()?;
    let seq: u64 = arg(args, 1, "SEQ")?.parse()?;
    let secret: Secret = fs::read_to_string(arg(args, 2, "SECRET")?)?.parse()?;
    let said: Said = stdin_json()?;
    println!(
        "{}",
        serde_json::to_string(&Statement::sign(&secret, cell, seq, said))?
    );
    Ok(())
}

fn open(args: &[String]) -> Fallible {
    let key: Key = arg(args, 0, "KEY")?.parse()?;
    let statement: Statement = stdin_json()?;
    let said = statement.open(&key)?;
    println!("{}", serde_json::to_string(said)?);
    Ok(())
}

fn allow(args: &[String]) -> Fallible {
    for h in load_cell(arg(args, 0, "CELLFILE")?)?.allowlist() {
        println!("{h}");
    }
    Ok(())
}

fn load_tree(dir: &str) -> Fallible<Tree> {
    Ok(cell::tree::load(Path::new(dir))?.0)
}

fn tree(args: &[String]) -> Fallible {
    let tree = load_tree(arg(args, 0, "DIR")?)?;
    for c in tree.iter() {
        let parent = tree
            .parent(&c.name)
            .map_or("-".to_string(), |p| p.name.to_string());
        println!(
            "{}\t{}\t{} grants\t{}",
            c.name,
            parent,
            c.grants.len(),
            c.hash()
        );
    }
    Ok(())
}

/// The directory is the tree: `DIR/x.json` is a cell under `DIR`'s own
/// cell, and `DIR/x/` holds x's children. Re-parenting is `mv`. Dot
/// entries and files that are not `.json` are skipped.
/// The cells named on the command line: NAME alone, or every cell in the tree.
fn named(tree: &Tree, args: &[String]) -> Fallible<Vec<Name>> {
    match args.get(1).filter(|a| !a.starts_with("--")) {
        Some(n) => {
            let name: Name = n.parse()?;
            tree.get(&name)
                .ok_or_else(|| format!("{name}: not in the tree"))?;
            Ok(vec![name])
        }
        None => Ok(tree.iter().map(|c| c.name.clone()).collect()),
    }
}

fn pins(dir: &str, args: &[String]) -> PathBuf {
    flag(args, "--pins").map_or_else(|| Path::new(dir).join(".pins"), PathBuf::from)
}

fn plan(args: &[String]) -> Fallible {
    let dir = arg(args, 0, "DIR")?;
    let tree = load_tree(dir)?;
    let names = named(&tree, args)?;
    let plans: BTreeMap<Name, Plan> = match flag(args, "--have") {
        Some(path) => {
            let seen: Observed = serde_json::from_str(&fs::read_to_string(path)?)?;
            let on_box = flag(args, "--box").map(str::parse::<Hash>).transpose()?;
            names
                .iter()
                .map(|n| {
                    let want = tree.get(n).expect("named from the tree");
                    let children = tree.children(n).map(|c| c.name.clone()).collect();
                    let on_box = on_box.map(|h| (h, Hash::ZERO));
                    let plan = Plan::new(want, &children, seen.of(n), on_box, None);
                    (n.clone(), plan)
                })
                .collect()
        }
        None => {
            let ssh = Ssh {
                bin: flag(args, "--bin").map(PathBuf::from),
            };
            cell::apply::plans(&ssh, &pins(dir, args), &tree, &names)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&plans)?);
    eprintln!("approve with {}", Approved::hash(&plans));
    Ok(())
}

fn apply(args: &[String]) -> Fallible {
    let dir = arg(args, 0, "DIR")?;
    let tree = load_tree(dir)?;
    let names = named(&tree, args)?;
    let approval: Hash = flag(args, "--approve")
        .ok_or("apply: --approve HASH, as `cell plan` printed it")?
        .parse()?;
    let ssh = Ssh {
        bin: flag(args, "--bin").map(PathBuf::from),
    };
    let pins = pins(dir, args);
    let plans = cell::apply::plans(&ssh, &pins, &tree, &names)?;
    let approved = Approved::new(plans, &approval)?;
    let n = cell::apply::apply(&ssh, &pins, &tree, &approved, &mut io::stdout())?;
    println!("settled after {n} action{}", if n == 1 { "" } else { "s" });
    Ok(())
}

fn adopt(args: &[String]) -> Fallible {
    let vm = arg(args, 0, "VM")?;
    let name: Name = arg(args, 1, "NAME")?.parse()?;
    Ssh { bin: None }.run(&["tag", vm, &format!("{}{name}", cell::cell::TAG_PREFIX)])?;
    println!("{vm} is now the root of {name}; plan it");
    Ok(())
}

fn observe(args: &[String]) -> Fallible {
    let (vms, ints) = match flag(args, "--from") {
        Some(dir) => {
            let ls = fs::read_to_string(Path::new(dir).join("ls.json"))?;
            let ints = fs::read_to_string(Path::new(dir).join("int.json"))?;
            let listing: Listing = serde_json::from_str(&ls)?;
            let ints: Vec<Integration> = serde_json::from_str(&ints)?;
            (listing.vms, ints)
        }
        None => Ssh { bin: None }.listing()?,
    };
    cell::exe::cell_vms(&vms)?;
    let seen = cell::exe::observe(&vms, &ints);
    println!("{}", serde_json::to_string_pretty(&seen)?);
    Ok(())
}

fn setup(args: &[String]) -> Fallible {
    let path = Path::new(arg(args, 0, "CELLFILE")?);
    checked(cell::wall::setup(path)?)
}

/// The providers with a credential on the box, one word each.
fn signed_in(signed: &BTreeSet<Provider>) -> String {
    if signed.is_empty() {
        return "nothing".into();
    }
    let names: Vec<String> = signed.iter().map(ToString::to_string).collect();
    names.join(" ")
}

/// Print a Checked statement's report and where it was written.
fn checked(statement: Statement) -> Fallible {
    let secret: Secret = fs::read_to_string(Path::new(cell::wall::ETC).join("secret"))?.parse()?;
    let Said::Checked {
        cell,
        ok,
        report,
        signed,
        ..
    } = statement.open(&secret.public())?
    else {
        return Err("not a Checked statement".into());
    };
    println!("{report}");
    println!(
        "{}: cell {} on seq {}, key {}, signed in: {}",
        if *ok { "WALL OK" } else { "WALL BROKEN" },
        cell,
        statement.seq,
        secret.public(),
        signed_in(signed)
    );
    if !*ok {
        return Err("the wall did not hold; see the report".into());
    }
    Ok(())
}

fn probe() -> Fallible {
    for p in cell::wall::probe() {
        println!("{}", serde_json::to_string(&p)?);
    }
    Ok(())
}

fn door(args: &[String]) -> Fallible {
    let listen = flag(args, "--listen").ok_or("door: --listen ADDR")?;
    let cell = load_cell(flag(args, "--cell").ok_or("door: --cell CELLFILE")?)?;
    let said = flag(args, "--said").ok_or("door: --said DIR")?;
    let inbox = flag(args, "--inbox").ok_or("door: --inbox DIR")?;
    let listener = TcpListener::bind(listen)?;
    eprintln!("cell {}: door on {}", cell.name, listener.local_addr()?);
    Arc::new(Door::new(&cell, said, inbox)).serve(listener)?;
    Ok(())
}

fn ship(args: &[String]) -> Fallible {
    let cell = load_cell(flag(args, "--cell").ok_or("ship: --cell CELLFILE")?)?;
    let secret: Secret =
        fs::read_to_string(flag(args, "--secret").ok_or("ship: --secret FILE")?)?.parse()?;
    let outbox = Path::new(flag(args, "--outbox").ok_or("ship: --outbox DIR")?);
    let said = Path::new(flag(args, "--said").ok_or("ship: --said DIR")?);
    if args.iter().any(|a| a == "--once") {
        for (name, outcome) in cell::ship::ship(&cell, &secret, outbox, said)? {
            match outcome {
                Ok(seq) => println!("{name} -> {seq}"),
                Err(why) => println!("{name} refused: {why}"),
            }
        }
        return Ok(());
    }
    eprintln!(
        "cell {}: shipping {} into {}",
        cell.name,
        outbox.display(),
        said.display()
    );
    cell::ship::serve(&cell, &secret, outbox, said, Duration::from_millis(500))?;
    Ok(())
}

fn knock(args: &[String]) -> Fallible {
    let inbox = Path::new(flag(args, "--inbox").ok_or("knock: --inbox DIR")?);
    let said: Said = stdin_json()?;
    let knock = Knock { from: None, said };
    println!("{}", cell::door::file_knock(inbox, &knock)?);
    Ok(())
}

/// The VM that is NAME's root, from a live listing.
fn root_of(ssh: &Ssh, tree: &Tree, name: &Name) -> Fallible<String> {
    tree.get(name)
        .ok_or_else(|| format!("{name}: not in the tree"))?;
    let (vms, _) = ssh.listing()?;
    cell::exe::cell_vm(&vms, name)
        .map_err(io::Error::other)?
        .map(|vm| vm.vm_name.clone())
        .ok_or_else(|| format!("{name}: no VM carries its tag; apply it first").into())
}

fn brief(args: &[String]) -> Fallible {
    let tree = load_tree(arg(args, 0, "DIR")?)?;
    let name: Name = arg(args, 1, "NAME")?.parse()?;
    let session: SessionId = arg(args, 2, "SESSION_ID")?.parse()?;
    let request = flag(args, "--request")
        .map(str::parse)
        .transpose()?
        .unwrap_or_else(RequestId::generate);
    let text = arg(args, 3, "TEXT")?.to_string();
    let ssh = Ssh { bin: None };
    let vm = root_of(&ssh, &tree, &name)?;
    let said = Said::Brief {
        request,
        session: SessionRef {
            cell: name.clone(),
            id: session,
        },
        text,
    };
    eprintln!("request {request}; reuse --request {request} when retrying this brief");
    let n = ssh.knock(&vm, &said)?;
    println!("{name}: inbox {n}");
    Ok(())
}

fn pull(args: &[String]) -> Fallible {
    let dir = arg(args, 0, "DIR")?;
    let tree = load_tree(dir)?;
    let name: Name = arg(args, 1, "NAME")?.parse()?;
    let since: u64 = flag(args, "--since").map_or(Ok(0), str::parse)?;
    let ssh = Ssh { bin: None };
    let vm = root_of(&ssh, &tree, &name)?;
    let Some(log) = cell::apply::pull(&ssh, &pins(dir, args), &name, &vm)? else {
        return Err(format!("{name}: not set up yet").into());
    };
    for (statement, said) in log.iter().filter(|(s, _)| s.seq > since) {
        let line = match said {
            Said::Checked {
                ok, binary, signed, ..
            } => format!(
                "checked: {}, binary {}, signed in: {}",
                if *ok { "ok" } else { "BROKEN" },
                &binary.to_string()[..12],
                signed_in(signed)
            ),
            other => other.to_string(),
        };
        println!("{}\t{line}", statement.seq);
    }
    Ok(())
}

fn login(args: &[String]) -> Fallible {
    let tree = load_tree(arg(args, 0, "DIR")?)?;
    let name: Name = arg(args, 1, "NAME")?.parse()?;
    let vm = root_of(&Ssh { bin: None }, &tree, &name)?;
    eprintln!("opening Pi as the agent on {vm}; use /login there");
    let status = std::process::Command::new("ssh")
        .args([
            "-t",
            &format!("{vm}.exe.xyz"),
            "if test -f /etc/cell/environment-initialized; then sudo machinectl shell agent@cell-env /bin/bash -lc 'exec /usr/local/lib/cell/pi-1.3.0/pi'; else sudo -iu agent /usr/local/lib/cell/pi-1.3.0/pi; fi",
        ])
        .status()?;
    if !status.success() {
        return Err(format!("ssh exited with {status}").into());
    }
    Ok(())
}

fn proxy(args: &[String]) -> Fallible {
    let listen = flag(args, "--listen").ok_or("proxy: --listen ADDR")?;
    let cell = load_cell(flag(args, "--cell").ok_or("proxy: --cell CELLFILE")?)?;
    let mut policy = Policy::for_cell(&cell);
    policy.private_ok = args.iter().any(|a| a == "--private-ok");
    let listener = TcpListener::bind(listen)?;
    eprintln!(
        "cell {}: proxy on {} for {} hosts",
        cell.name,
        listener.local_addr()?,
        policy.hosts.len()
    );
    for h in &policy.hosts {
        eprintln!("  {h}");
    }
    Arc::new(Proxy::new(policy, system_resolver())).serve(listener)?;
    Ok(())
}
