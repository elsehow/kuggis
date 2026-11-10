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

#[test]
fn grouped_help_and_debug_are_discoverable_without_external_programs() {
    let output = command().arg("--help").output().unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    for group in ["cell", "chat", "access", "file", "admin"] {
        assert!(help.contains(&format!("kuggis {group}")));
        assert!(command()
            .args([group, "--help"])
            .output()
            .unwrap()
            .status
            .success());
    }
    assert!(!help.contains("normalize-nft"));
    assert!(help.contains("kuggis login CELL --agent codex"));
    let login_help = command().args(["login", "--help"]).output().unwrap();
    assert!(login_help.status.success());
    assert!(String::from_utf8_lossy(&login_help.stdout).contains("laptop browser"));
    let output = command()
        .args(["admin", "debug", "render", &fixture()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["status"],
        "configuration_only"
    );
    let output = command()
        .args(["cell", "web", &fixture(), "--plan"])
        .output()
        .unwrap();
    // Planning a preview never invokes SSH or a browser.
    assert!(output.status.success());
}

#[test]
fn grouped_routes_preserve_literal_guest_arguments_and_chat_selection() {
    use kuggis::cli::{route, Route};
    let strings = |words: &[&str]| words.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let Route::Legacy(args) = route(&strings(&[
        "cell",
        "exec",
        "fri",
        "--",
        "printf",
        "--help",
        "$(literal)",
    ]))
    .unwrap() else {
        panic!("expected legacy route")
    };
    assert_eq!(
        args,
        strings(&["exec", "fri", "--", "printf", "--help", "$(literal)"])
    );
    let Route::Legacy(args) = route(&strings(&[
        "chat", "open", "fri", "research", "--agent", "codex",
    ]))
    .unwrap() else {
        panic!("expected chat route")
    };
    assert_eq!(
        args,
        strings(&["connect", "fri", "--chat", "research", "--agent", "codex"])
    );
    assert!(route(&strings(&["chat", "list", "fri", "--chat", "research"])).is_err());
}

#[test]
fn positional_routes_select_scope_and_preserve_arguments() {
    use kuggis::cli::{route, Route};
    for (input, expected, managed) in [
        (
            vec!["fri", "research"],
            vec!["connect", "fri", "--chat", "research"],
            false,
        ),
        (
            vec!["fri", "research", "--agent", "codex"],
            vec!["connect", "fri", "--chat", "research", "--agent", "codex"],
            false,
        ),
        (
            vec!["fri", "research", "open"],
            vec!["connect", "fri", "--chat", "research"],
            false,
        ),
        (
            vec!["fri", "research", "archive", "--json"],
            vec!["chat", "archive", "fri", "research", "--json"],
            true,
        ),
        (vec!["fri"], vec!["connect", "fri"], false),
        (
            vec!["fri", "--list", "--json"],
            vec!["connect", "fri", "--json"],
            false,
        ),
        (
            vec!["--list", "--json"],
            vec!["cell", "list", "--json"],
            true,
        ),
        (
            vec!["--create", "fri", "--plan"],
            vec!["create", "fri", "--plan"],
            false,
        ),
        (
            vec![
                "fri",
                "--put",
                "a b.png",
                "images/a'b.png",
                "--chat",
                "vt",
                "--replace",
            ],
            vec![
                "file",
                "put",
                "fri",
                "--items",
                "a b.png",
                "images/a'b.png",
                "--chat",
                "vt",
                "--replace",
            ],
            true,
        ),
        (
            vec!["fri", "--put", "--paste", "--chat", "vt"],
            vec!["file", "put", "fri", "--items", "--paste", "--chat", "vt"],
            true,
        ),
        (
            vec!["fri", "--get", "out.pdf", "--save-as", "local.pdf"],
            vec![
                "file",
                "get",
                "fri",
                "--items",
                "out.pdf",
                "--save-as",
                "local.pdf",
            ],
            true,
        ),
        (
            vec!["fri", "--exec", "--", "printf", "--help", "$(literal)"],
            vec!["exec", "fri", "--", "printf", "--help", "$(literal)"],
            false,
        ),
        (
            vec!["fri", "--check"],
            vec!["cell", "status", "fri", "--check"],
            true,
        ),
        (
            vec![
                "fri",
                "--access",
                "grant",
                "network",
                "example.com:443",
                "--plan",
            ],
            vec![
                "access",
                "grant",
                "fri",
                "network",
                "example.com:443",
                "--plan",
            ],
            true,
        ),
    ] {
        let args = input.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let route = route(&args).unwrap();
        match route {
            Route::Manage(args) if managed => assert_eq!(args, expected),
            Route::Legacy(args) if !managed => assert_eq!(args, expected),
            _ => panic!("unexpected route for {input:?}"),
        }
    }
}

#[test]
fn unknown_operations_and_actions_fail_before_loading_a_cell() {
    for args in [
        vec!["missing", "research", "delete"],
        vec!["missing", "research", "typo"],
        vec!["missing", "../escape"],
        vec!["missing", "--lst"],
        vec!["missing", "--list", "research"],
        vec!["--list", "missing"],
    ] {
        let output = command().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("No such file"),
            "{args:?}"
        );
    }
    for args in [vec!["fri", "--help"], vec!["fri", "research", "--help"]] {
        assert!(command().args(args).output().unwrap().status.success());
    }
}
