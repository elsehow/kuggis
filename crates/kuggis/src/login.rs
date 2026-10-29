//! Browser login on the operator machine, native credentials inside the cell.
use crate::{onboarding, ssh::Invocation, ssh::Request, Error, Result, Workspace};
use serde_json::json;
use std::{
    io::Write,
    process::{Command, Stdio},
};

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() || args.iter().any(|s| s == "--help" || s == "-h") {
        println!("{}", crate::cli::LOGIN_HELP);
        return Ok(());
    }
    // Validate before looking up a cell or making any remote changes.
    let mut agent = None;
    let mut words = args[1..].iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--agent" if agent.is_none() => agent = words.next().map(String::as_str),
            "--no-open" | "--force" | "--status" | "--plan" | "--json" => {}
            _ => return Err(Error(crate::cli::LOGIN_HELP.into())),
        }
    }
    if agent != Some("codex") {
        return Err(Error(
            "login currently supports --agent codex; other agents keep their native login flows"
                .into(),
        ));
    }
    let path = onboarding::config_path(&args[0])?;
    let workspace = Workspace::parse(&std::fs::read_to_string(&path)?)?;
    let host = Invocation::for_management(&workspace);
    let mut payload = json!({"args": args, "config": path,
        "binary": std::env::current_exe()?, "state_root": onboarding::state_root()?,
        "settings": onboarding::state_root()?.join("github-operator.json"),
        "host": ([vec![host.program.to_owned()], host.args].concat()),
        "launcher": include_str!("../../../recipes/codex/launch.py")});
    for mode in ["prepare", "login", "callback", "status"] {
        let invocation = Invocation::for_request(
            &workspace,
            &Request::Exec(vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                include_str!("../runtime/login_guest.py").into(),
                mode.into(),
            ]),
        );
        payload["guest"][mode] =
            json!([vec![invocation.program.to_owned()], invocation.args].concat());
    }
    let program = format!(
        "{}\n{}\n{}\nraise SystemExit(login_main(json.load(sys.stdin)))\n",
        crate::github::bootstrap(crate::github::OPERATOR),
        include_str!("../runtime/manage.py"),
        include_str!("../runtime/login.py")
    );
    let mut child = Command::new("python3")
        .args(["-c", &program])
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error("missing login input".into()))?
        .write_all(&serde_json::to_vec(&payload)?)?;
    let status = child.wait()?;
    if !status.success() {
        // The adapter already emitted one structured error (or a readable one).
        std::process::exit(status.code().unwrap_or(2));
    }
    Ok(())
}
