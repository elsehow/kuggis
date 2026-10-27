use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kuggis"));
    // Rendering must not invoke SSH, agent executables or a package installer.
    command.env("PATH", "");
    command
}

fn fixture() -> String {
    format!(
        "{}/../../examples/workspace.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn validates_and_renders_without_external_programs() {
    let output = command().args(["validate", &fixture()]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("not installed"));
    let output = command().args(["render", &fixture()]).output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "configuration_only");
}

#[test]
fn transfer_recipe_is_json_and_does_not_transfer_anything() {
    let output = command()
        .args(["ssh-command", &fixture(), "put", "inputs/file.bin"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["stdin"], "file_frame");
    assert_eq!(value["requires_installed_host_helper"], true);
}

#[test]
fn invalid_calls_and_unavailable_transport_fail_without_prompting() {
    for args in [
        vec!["setup".to_owned(), fixture()],
        vec!["connect".to_owned(), fixture()],
        vec!["validate".to_owned(), fixture(), "--approve".into()],
        vec![
            "ssh-command".to_owned(),
            fixture(),
            "put".into(),
            "../escape".into(),
        ],
    ] {
        let output = command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
