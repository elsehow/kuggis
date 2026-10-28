use kuggis::{
    web::{Options, Preview},
    Workspace,
};
use serde_json::{json, Value};
use std::process::Command;

fn workspace(mappings: Value) -> Workspace {
    Workspace::parse(
        &json!({"version":2,"name":"demo","ssh":"demo-host",
        "ssh_config":"/operator/ssh-config","launch":["bash"],"private_web":mappings})
        .to_string(),
    )
    .unwrap()
}

#[test]
fn selection_requires_a_declared_unambiguous_preview() {
    let one = json!({"name":"app","guest_port":8787,"host_port":8000});
    assert!(Preview::select(&workspace(json!([])), &Options::default()).is_err());
    let two = workspace(json!([one, {"name":"docs","guest_port":3001,"host_port":8001}]));
    assert!(Preview::select(&two, &Options::default()).is_err());
    assert!(Preview::select(&two, &Options::parse(&["missing".into()]).unwrap()).is_err());
    let plan = Preview::select(
        &two,
        &Options::parse(&["docs".into(), "--port".into(), "18080".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(plan.url, "http://127.0.0.1:18080/");
    assert_eq!(plan.guest_port.get(), 3001);
    assert!(plan
        .ssh
        .args
        .contains(&"127.0.0.1:18080:127.0.0.1:8001".into()));
    for arg in [
        "StrictHostKeyChecking=yes",
        "ForwardAgent=no",
        "ExitOnForwardFailure=yes",
        "/operator/ssh-config",
    ] {
        assert!(plan.ssh.args.contains(&arg.to_owned()));
    }
    assert!(!plan.ssh.requires_installed_host_helper);
}

#[test]
fn invalid_web_options_are_rejected() {
    for args in [
        vec!["--port"],
        vec!["--port", "0"],
        vec!["--port", "65536"],
        vec!["--port", "-1"],
        vec!["--public"],
        vec!["app", "docs"],
        vec!["--no-open", "--no-open"],
    ] {
        assert!(Options::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err());
    }
}

#[test]
fn plan_needs_neither_ssh_nor_browser() {
    let output = Command::new(env!("CARGO_BIN_EXE_kuggis"))
        .args([
            "web",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/workspace.json"),
            "--plan",
        ])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["url"], "http://127.0.0.1:8000/");
    assert_eq!(plan["guest_port"], 3000);
}
