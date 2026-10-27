use kuggis::{
    config::Name,
    onboarding::CreateRequest,
    ssh::{Invocation, Request},
    Workspace,
};
use serde_json::{json, Value};
use std::{path::Path, process::Command};

fn declaration() -> Value {
    json!({"version":2,"name":"demo","ssh":"demo-host","launch":["pi","--continue"]})
}

#[test]
fn agent_recipes_are_validated_and_selected_without_shell_arguments() {
    let mut input = declaration();
    input["agents"] =
        json!({"codex":{"launch":["codex-chat","default"],"chat_launch":["codex-chat","{chat}"]}});
    assert!(Workspace::parse(&input.to_string()).is_err());
    input["default_agent"] = json!("missing");
    assert!(Workspace::parse(&input.to_string()).is_err());
    input["default_agent"] = json!("codex");
    let workspace = Workspace::parse(&input.to_string()).unwrap();
    let selected = Invocation::for_request(
        &workspace,
        &Request::SelectChat {
            chat: Name::try_from("vt".to_owned()).unwrap(),
            agent: Name::try_from("codex".to_owned()).unwrap(),
        },
    );
    assert!(selected
        .args
        .last()
        .unwrap()
        .ends_with("'--chat' 'vt' '--agent' 'codex'"));
    input["agents"]["codex"]["launch"] = json!([]);
    assert!(Workspace::parse(&input.to_string()).is_err());
    input["agents"]["codex"]["launch"] = json!(["codex-chat", "default"]);
    input["agents"]["codex"]["chat_launch"] = json!(["codex-chat"]);
    assert!(Workspace::parse(&input.to_string()).is_err());
}

#[test]
fn onboarding_installs_both_recipes_and_selects_only_the_default() {
    let request =
        CreateRequest::parse(json!({"name":"demo","repo":"owner/repo","agent":"codex"})).unwrap();
    let value = serde_json::to_value(
        request
            .workspace(Path::new("/operator/ssh-config"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["default_agent"], "codex");
    assert_eq!(
        value["agents"]["codex"]["chat_launch"],
        json!(["/home/agent/.local/bin/kuggis-codex", "{chat}"])
    );
    assert!(value["agents"].get("pi").is_some());
    assert!(value.to_string().contains("pi-models"));
    let alias =
        CreateRequest::parse(json!({"name":"demo","repo":"owner/repo","default_agent":"codex"}))
            .unwrap();
    assert_eq!(
        serde_json::to_value(alias.workspace(Path::new("/operator/ssh-config")).unwrap()).unwrap(),
        value
    );
}

#[test]
fn listing_is_noninteractive_and_legacy_default_requires_explicit_selection() {
    let workspace = Workspace::parse(&declaration().to_string()).unwrap();
    assert!(workspace.chat_launch().is_none());
    assert!(serde_json::to_value(&workspace)
        .unwrap()
        .get("chat_launch")
        .is_none());
    let plain = Invocation::for_request(&workspace, &Request::Chats);
    let default = Invocation::for_request(
        &workspace,
        &Request::Chat(Name::try_from("default".to_owned()).unwrap()),
    );
    assert_eq!(plain.args[0], "-T");
    assert!(plain.args.last().unwrap().ends_with("'chats' 'demo'"));
    assert_eq!(default.args[0], "-tt");
    assert!(default
        .args
        .last()
        .unwrap()
        .ends_with("'connect' 'demo' '--chat' 'default'"));
    let named = Invocation::for_request(
        &workspace,
        &Request::Chat(Name::try_from("research".to_owned()).unwrap()),
    );
    assert!(named
        .args
        .last()
        .unwrap()
        .ends_with("'connect' 'demo' '--chat' 'research'"));
    assert!(named.args.contains(&"-tt".into()));
}

#[test]
fn recipe_requires_a_valid_argv_and_an_explicit_chat_binding() {
    for bad in [
        json!([]),
        json!(["", "{chat}"]),
        json!(["{chat}", "x"]),
        json!(["pi", "--continue"]),
        json!(["pi", "bad\u{0000}{chat}"]),
        json!(["pi", 42]),
    ] {
        let mut input = declaration();
        input["chat_launch"] = bad;
        assert!(Workspace::parse(&input.to_string()).is_err());
    }
    let request = CreateRequest::parse(json!({"name":"demo","repo":"owner/repo"})).unwrap();
    let workspace = request
        .workspace(Path::new("/operator/ssh-config"))
        .unwrap();
    let value = serde_json::to_value(workspace).unwrap();
    assert_eq!(
        value["launch"],
        json!(["/home/agent/.local/bin/pi", "--offline", "--continue"])
    );
    assert!(value["chat_launch"]
        .as_array()
        .unwrap()
        .contains(&json!("/home/agent/.kuggis/chats/{chat}")));
}

#[test]
fn cli_validates_chat_names_and_renders_without_programs() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/workspace.json");
    let cli = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kuggis"));
        c.env("PATH", "");
        c
    };
    let output = cli()
        .args(["connect", fixture, "--agent", "codex"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--agent requires --chat"));
    let output = cli()
        .args(["ssh-command", fixture, "connect"])
        .output()
        .unwrap();
    let listing: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listing["args"][0], "-T");
    for name in ["../escape", "-x", "a;id", "", "a/b", "UPPER", "trailing-"] {
        let output = cli()
            .args(["connect", fixture, "--chat", name])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid Name"));
    }
    let output = cli()
        .args(["connect", fixture, "--chat", "research"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("chat_launch"));
    let output = cli()
        .args(["ssh-command", fixture, "connect", "--chat", "research"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["args"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .ends_with("'--chat' 'research'"));
}
