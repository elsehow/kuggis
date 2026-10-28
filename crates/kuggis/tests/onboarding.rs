use kuggis::{
    onboarding::CreateRequest,
    ssh::{Invocation, Request},
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kuggis-onboarding-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self, cwd: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kuggis"));
        command
            .current_dir(self.0.join(cwd))
            .env("PATH", "")
            .env_remove("KUGGIS_STATE_DIR")
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .stdin(Stdio::null());
        command
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

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
        first["resources"][1]["endpoints"][0]["host"],
        json!("github.com")
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

#[test]
fn named_workspaces_and_create_plans_use_the_same_location_from_any_directory() {
    let dir = Directory::new();
    let workspace = dir.0.join("config/kuggis/workspaces/demo");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir(dir.0.join("first-project")).unwrap();
    fs::create_dir(dir.0.join("second-project")).unwrap();
    fs::write(workspace.join("setup.json"), input().to_string()).unwrap();
    let config = CreateRequest::parse(input())
        .unwrap()
        .workspace(&workspace.join("ssh-config"))
        .unwrap();
    fs::write(
        workspace.join("workspace.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    for cwd in ["first-project", "second-project"] {
        let output = dir
            .command(cwd)
            .args(["validate", "demo"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Resume inputs are found too; the caller need not repeat --repo.
        let plan = dir
            .command(cwd)
            .args(["create", "demo", "--plan", "--json"])
            .output()
            .unwrap();
        assert!(
            plan.status.success(),
            "{}",
            String::from_utf8_lossy(&plan.stderr)
        );
        let plan: Value = serde_json::from_slice(&plan.stdout).unwrap();
        assert_eq!(
            plan["workspace"]["ssh_config"],
            workspace.join("ssh-config").to_str().unwrap()
        );
        assert_eq!(plan["request"]["repo"], "owner/project");
    }
}

#[test]
fn state_override_and_explicit_paths_are_preserved_and_missing_names_are_actionable() {
    let dir = Directory::new();
    let config = CreateRequest::parse(input())
        .unwrap()
        .workspace(Path::new("/operator/ssh-config"))
        .unwrap();
    let custom = dir.0.join("custom/demo");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        custom.join("workspace.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let output = dir
        .command("")
        .env("KUGGIS_STATE_DIR", "custom")
        .args(["validate", "demo"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = dir
        .command("")
        .args(["validate", "custom/demo/workspace.json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = dir
        .command("")
        .env("KUGGIS_STATE_DIR", dir.0.join("custom"))
        .args(["validate", "missing"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(error.contains("workspace 'missing' not found"), "{error}");
    assert!(
        error.contains(
            dir.0
                .join("custom/missing/workspace.json")
                .to_str()
                .unwrap()
        ),
        "{error}"
    );
    assert!(error.contains("KUGGIS_STATE_DIR"), "{error}");
    // An unset/empty override selects the per-user default, never a local .kuggis directory.
    let output = dir
        .command("")
        .env("KUGGIS_STATE_DIR", "")
        .args(["validate", "demo"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(
            dir.0
                .join("config/kuggis/workspaces/demo/workspace.json")
                .to_str()
                .unwrap()
        ),
        "{error}"
    );
}
