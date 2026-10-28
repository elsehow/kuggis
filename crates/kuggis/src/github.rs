//! Host-independent GitHub operator commands and embedded broker runtime.
use crate::{onboarding, ssh::Invocation, Error, Result, Workspace};
use serde_json::json;
use std::{
    io::Write,
    process::{Command, Stdio},
};

pub const COMMON: &str = include_str!("../runtime/github_common.py");
pub const BROKER: &str = include_str!("../runtime/github_broker.py");
pub const OPERATOR: &str = include_str!("../runtime/github_operator.py");
pub const HOST: &str = include_str!("../runtime/github_host.py");
pub const GUEST: &str = include_str!("../runtime/github_guest.py");
pub const WIRE: &str = include_str!("../runtime/github_wire.py");
pub const SSH: &str = include_str!("../runtime/github_ssh.py");
pub const GATEWAY: &str = include_str!("../runtime/github_gateway.py");
pub const GIT: &str = include_str!("../runtime/github_git.py");
pub const API: &str = include_str!("../runtime/github_api.py");
pub const COLLAB: &str = include_str!("../runtime/github_collab.py");

fn bootstrap(source: &str) -> String {
    let mut program = String::from("import sys, types\n");
    for (name, module) in [
        ("github_common", COMMON),
        ("github_wire", WIRE),
        ("github_ssh", SSH),
        ("github_gateway", GATEWAY),
        ("github_api", API),
    ] {
        program.push_str(&format!(
            "m = types.ModuleType({})\nsys.modules[{}] = m\nexec({}, m.__dict__)\n",
            json!(name),
            json!(name),
            json!(module)
        ));
    }
    program.push_str(&format!("exec({})\n", json!(source)));
    program
}

pub fn run(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("github")
        && args.get(1).map(String::as_str) == Some("serve")
    {
        let status = Command::new("python3")
            .args(["-c", &bootstrap(BROKER)])
            .args(&args[2..])
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(Error("GitHub broker stopped with an error".into()))
        };
    }
    let mut payload =
        json!({"args": args, "settings": onboarding::state_root()?.join("github-operator.json")});
    let workspace_command = (args.first().map(String::as_str) == Some("repo")
        || args.get(1).map(String::as_str) == Some("attach"))
        && !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
    if workspace_command {
        let target = args
            .get(2)
            .ok_or_else(|| Error("expected CONFIG_OR_NAME".into()))?;
        let path = onboarding::config_path(target)?;
        let workspace = Workspace::parse(&std::fs::read_to_string(&path)?)?;
        payload["config"] = json!(path);
        for action in ["status", "attach"] {
            let invocation = Invocation::for_github(&workspace, action);
            payload["ssh"][action] =
                json!([vec![invocation.program.to_owned()], invocation.args].concat());
        }
    }
    let program = format!(
        "{}\nimport json\nsys.exit(main(json.load(sys.stdin)))\n",
        bootstrap(OPERATOR)
    );
    let mut child = Command::new("python3")
        .args(["-c", &program])
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error("missing GitHub control input".into()))?
        .write_all(&serde_json::to_vec(&payload)?)?;
    let status = child.wait()?;
    if !status.success() {
        return Err(Error("GitHub operation did not complete".into()));
    }
    Ok(())
}
