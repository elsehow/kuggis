use kuggis::{
    onboarding::CreateRequest,
    ssh::{Invocation, Request},
};
use serde_json::{json, Value};
use std::{
    path::Path,
    process::{Command, Stdio},
};

fn input() -> Value {
    json!({"name":"demo", "repo":"owner/project","github_access":"write"})
}

#[test]
fn setup_inputs_validate_before_effects_and_compile_deterministically() {
    let request = CreateRequest::parse(input()).unwrap();
    let config = Path::new("/operator/demo/ssh-config");
    let first = serde_json::to_value(request.workspace(config).unwrap()).unwrap();
    assert_eq!(
        first,
        serde_json::to_value(request.workspace(config).unwrap()).unwrap()
    );
    assert_eq!(first["ssh"], "kuggis-demo.exe.xyz");
    assert_eq!(
        first["private_web"],
        json!([{"name":"preview","guest_port":3000,"host_port":8000}])
    );
    assert_eq!(first["memory_bytes"], 6_u64 * 1024 * 1024 * 1024);
    assert_eq!(
        first["resources"][1]["endpoints"][0]["destination"]["addresses"],
        json!(["169.254.169.254"])
    );
    assert!(!first["launch"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "--model"));
    for (key, value) in [
        ("name", json!("../production")),
        ("name", json!("x".repeat(41))),
        ("repo", json!("https://github.com/owner/repo")),
        ("repo", json!("owner/../repo")),
        ("repo", json!("owner/repo.git")),
        ("github_access", json!("admin")),
        ("host", json!("unimplemented")),
        ("agent", json!("unimplemented")),
        ("model", json!("unimplemented")),
        ("memory_gib", json!(0)),
        ("cpus", json!(-1)),
        ("version", json!(2)),
        ("approve", json!(true)),
    ] {
        let mut bad = input();
        bad[key] = value;
        assert!(CreateRequest::parse(bad).is_err(), "{key}");
    }
}

#[test]
fn unattended_plan_needs_no_external_programs_and_missing_input_is_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_kuggis"))
        .args([
            "create",
            "demo",
            "--repo",
            "owner/project",
            "--github-access",
            "write",
            "--plan",
            "--json",
        ])
        .env("PATH", "")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["status"], "configuration_only");
    assert_eq!(plan["request"]["github_access"], "write");
    let missing = Command::new(env!("CARGO_BIN_EXE_kuggis"))
        .args(["create", "demo", "--json"])
        .env("PATH", "")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(error["ok"], false);
    assert!(error["error"]["message"].as_str().unwrap().contains("repo"));
}

#[test]
fn exec_is_quoted_non_tty_guest_entry_not_a_host_shell() {
    let request = CreateRequest::parse(input()).unwrap();
    let workspace = request
        .workspace(Path::new("/operator/ssh-config"))
        .unwrap();
    let invocation = Invocation::for_request(
        &workspace,
        &Request::Exec(vec!["/bin/echo".into(), "'; touch /HOST; $(whoami)".into()]),
    );
    assert!(invocation.args.contains(&"-T".into()));
    assert!(!invocation.args.contains(&"-tt".into()));
    let command = invocation.args.last().unwrap();
    assert!(command.contains("'exec' 'demo' '--' '/bin/echo'"));
    assert!(command.contains("'\"'\"'"));
    assert!(invocation.requires_installed_host_helper);
}
