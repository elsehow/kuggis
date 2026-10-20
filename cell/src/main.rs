//! `cell`: the typed core on the command line. Every subcommand is a thin
//! call into the library; the library holds every check.

use cell::exe::{Integration, Listing, Observed};
use cell::proxy::{system_resolver, Policy, Proxy};
use cell::{Cell, Hash, Invalid, Key, Name, Plan, Said, Secret, Statement, Tree};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

const USAGE: &str = "\
usage:
  cell key new PATH               mint a secret at PATH (0600); print its public key
  cell key pub PATH               print the public key of the secret at PATH
  cell say CELL SEQ SECRET        sign the Said on stdin as CELL, sequence SEQ
  cell open KEY                   verify the Statement on stdin under KEY; print what it said
  cell allow CELLFILE             print the hosts a cell's proxy may tunnel to
  cell tree DIR                   check DIR as one tree: DIR/x.json sits under
                                  DIR's own cell, DIR/x/ holds x's children
  cell plan DIR NAME [--have OBSERVED] [--box HASH]
                                  diff NAME in the tree at DIR against the edge as
                                  `cell observe` wrote it; print the plan
  cell observe [--from DIR]       read exe.dev (or DIR/ls.json + DIR/int.json) as
                                  cells and doors
  cell proxy --listen ADDR --cell CELLFILE [--private-ok]
                                  run the wall's proxy for that cell
";

type Fallible = Result<(), Box<dyn Error>>;

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
        Some("observe") => observe(rest),
        Some("proxy") => proxy(rest),
        _ => {
            eprint!("{USAGE}");
            Err("no such command".into())
        }
    }
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> Result<&'a str, Box<dyn Error>> {
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

fn stdin_json<T: serde::de::DeserializeOwned>() -> Result<T, Box<dyn Error>> {
    let mut s = String::new();
    io::stdin().read_to_string(&mut s)?;
    Ok(serde_json::from_str(&s)?)
}

fn load_cell(path: impl AsRef<Path>) -> Result<Cell, Box<dyn Error>> {
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

fn load_tree(dir: &str) -> Result<Tree, Box<dyn Error>> {
    let mut cells = Vec::new();
    walk(Path::new(dir), None, &mut cells)?;
    Ok(Tree::new(cells)?)
}

fn tree(args: &[String]) -> Fallible {
    let tree = load_tree(arg(args, 0, "DIR")?)?;
    for c in tree.iter() {
        let parent = tree
            .parent(&c.name)
            .map_or("-".to_string(), |p| p.name.to_string());
        let room = if c.is_room() { " room" } else { "" };
        println!(
            "{}\t{}\t{} grants\t{}{}",
            c.name,
            parent,
            c.grants.len(),
            c.hash(),
            room
        );
    }
    Ok(())
}

/// The directory is the tree: `DIR/x.json` is a cell under `DIR`'s own
/// cell, and `DIR/x/` holds x's children. Re-parenting is `mv`. Dot
/// entries and files that are not `.json` are skipped.
fn walk(dir: &Path, under: Option<&Name>, out: &mut Vec<(Cell, Option<Name>)>) -> Fallible {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if file.starts_with('.') {
            continue;
        }
        let at = |e: Invalid| format!("{}: {e}", path.display());
        if path.is_dir() {
            let name: Name = file.parse().map_err(at)?;
            walk(&path, Some(&name), out)?;
        } else if let Some(stem) = file.strip_suffix(".json") {
            let name: Name = stem.parse().map_err(at)?;
            let cell = load_cell(&path)?;
            if cell.name != name {
                return Err(format!("{}: holds cell {}", path.display(), cell.name).into());
            }
            out.push((cell, under.cloned()));
        }
    }
    Ok(())
}

fn plan(args: &[String]) -> Fallible {
    let tree = load_tree(arg(args, 0, "DIR")?)?;
    let name: Name = arg(args, 1, "NAME")?.parse()?;
    let want = tree
        .get(&name)
        .ok_or_else(|| format!("{name}: not in the tree"))?;
    let children: BTreeSet<Name> = tree.children(&name).map(|c| c.name.clone()).collect();
    let seen: Observed = match flag(args, "--have") {
        Some(path) => serde_json::from_str(&fs::read_to_string(path)?)?,
        None => Observed::default(),
    };
    let on_box = flag(args, "--box").map(str::parse::<Hash>).transpose()?;
    let plan = Plan::new(want, &children, seen.of(&name), on_box.as_ref());
    println!("{}", serde_json::to_string_pretty(&plan)?);
    eprintln!("approve with {}", plan.hash());
    Ok(())
}

fn observe(args: &[String]) -> Fallible {
    let (ls, ints) = match flag(args, "--from") {
        Some(dir) => (
            fs::read_to_string(Path::new(dir).join("ls.json"))?,
            fs::read_to_string(Path::new(dir).join("int.json"))?,
        ),
        None => (
            exe(&["ls", "--json"])?,
            exe(&["integrations", "list", "--json"])?,
        ),
    };
    let listing: Listing = serde_json::from_str(&ls)?;
    let ints: Vec<Integration> = serde_json::from_str(&ints)?;
    let seen = cell::exe::observe(&listing.vms, &ints);
    println!("{}", serde_json::to_string_pretty(&seen)?);
    Ok(())
}

fn exe(args: &[&str]) -> Result<String, Box<dyn Error>> {
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "exe.dev"])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "ssh exe.dev {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
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
