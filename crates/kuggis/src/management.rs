//! Operator-only management; no administrative capability is installed in guests.
use crate::{onboarding, ssh::Invocation, Error, Result, Workspace};
use serde_json::json;
use std::{
    io::Write,
    process::{Command, Stdio},
};

pub const HOST: &str = include_str!("../runtime/manage_host.py");

pub fn run(args: &[String]) -> Result<()> {
    let mut payload = json!({"args": args, "binary": std::env::current_exe()?,
        "state_root": onboarding::state_root()?,
        "settings": onboarding::state_root()?.join("github-operator.json")});
    if args.get(1).map(String::as_str) != Some("list") || args[0] != "cell" {
        let target = args.get(2).ok_or_else(|| Error("expected CELL".into()))?;
        let path = onboarding::config_path(target)?;
        let workspace = Workspace::parse(&std::fs::read_to_string(&path)?)?;
        payload["config"] = json!(path);
        let invocation = Invocation::for_management(&workspace);
        payload["host"] = json!([vec![invocation.program.to_owned()], invocation.args].concat());
        for action in ["status", "attach"] {
            let invocation = Invocation::for_github(&workspace, action);
            payload["ssh"][action] =
                json!([vec![invocation.program.to_owned()], invocation.args].concat());
        }
    }
    let program = format!(
        "{}\n{}\nimport json\nsys.exit(management_main(json.load(sys.stdin)))\n",
        crate::github::bootstrap(crate::github::OPERATOR),
        include_str!("../runtime/manage.py")
    );
    let mut child = Command::new("python3")
        .args(["-c", &program])
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error("missing management input".into()))?
        .write_all(&serde_json::to_vec(&payload)?)?;
    let status = child.wait()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(2));
    }
    Ok(())
}
