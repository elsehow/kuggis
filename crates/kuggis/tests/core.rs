use kuggis::{
    config::{GuestPath, Host},
    environment::{Environment, HOST_HELPER},
    network::{public_address, Policy},
    ssh::{Invocation, Request},
    Workspace,
};
use serde_json::{json, Value};
use std::{net::IpAddr, process::Command};

const EXAMPLE: &str = include_str!("../../../examples/workspace.json");

fn example() -> Value {
    serde_json::from_str(EXAMPLE).unwrap()
}

fn workspace(value: Value) -> Workspace {
    Workspace::parse(&value.to_string()).unwrap()
}

fn rejected(value: Value) {
    assert!(
        Workspace::parse(&value.to_string()).is_err(),
        "accepted {value}"
    );
}

fn host(value: &str) -> Host {
    value.to_owned().try_into().unwrap()
}

fn addresses(values: &[&str]) -> Vec<IpAddr> {
    values.iter().map(|value| value.parse().unwrap()).collect()
}

#[test]
fn example_round_trips_without_reading_credential_sources() {
    let original = Workspace::parse(EXAMPLE).unwrap();
    let encoded = serde_json::to_string(&original).unwrap();
    let decoded = Workspace::parse(&encoded).unwrap();
    assert_eq!(decoded.name(), original.name());
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}

#[test]
fn no_legacy_or_approval_fields_are_accepted() {
    for field in ["parent", "children", "grants", "approve", "approval_mode"] {
        let mut value = example();
        value[field] = json!("anything");
        rejected(value);
    }
    for version in [0, 1, 3] {
        let mut value = example();
        value["version"] = json!(version);
        rejected(value);
    }
}

#[test]
fn public_ingress_and_unknown_nested_fields_are_not_options() {
    let mut value = example();
    value["private_web"][0]["public"] = json!(true);
    rejected(value);
    let mut value = example();
    value["resources"][0]["endpoints"][0]["wildcard"] = json!(true);
    rejected(value);
}

#[test]
fn names_targets_and_hosts_cannot_be_commands_or_paths() {
    for bad in [
        "",
        "../escape",
        "a/b",
        "-oProxyCommand=evil",
        "a\nb",
        "a;id",
    ] {
        let mut value = example();
        value["name"] = json!(bad);
        rejected(value);
    }
    for bad in ["", "-Fconfig", "host command", "a;id", "user@host", "$(id)"] {
        let mut value = example();
        value["ssh"] = json!(bad);
        rejected(value);
    }
    for bad in [
        "*.example.com",
        "host:443",
        "UPPER.example",
        "host.",
        "a..b",
        "https://host",
    ] {
        assert!(Host::try_from(bad.to_owned()).is_err(), "{bad}");
    }
}

#[test]
fn file_paths_cannot_walk_out_of_the_guest_workspace() {
    for bad in [
        "",
        "/etc/passwd",
        "../file",
        "a/../file",
        "a/./b",
        "a//b",
        "a/",
        "a\\b",
        "a\0b",
        "a\nb",
    ] {
        assert!(GuestPath::try_from(bad.to_owned()).is_err(), "{bad:?}");
    }
    assert!(GuestPath::try_from("reports/my report's final.pdf".to_owned()).is_ok());
}

#[test]
fn credentials_are_host_references_with_nonoverlapping_guest_destinations() {
    for bad in ["relative/path", "/etc/../secret", "/", "/etc//secret"] {
        let mut value = example();
        value["resources"][1]["credentials"][0]["source"] = json!(bad);
        rejected(value);
    }
    for target in [".ssh/deploy", ".ssh", ".ssh/deploy/child"] {
        let mut value = example();
        value["resources"][0]["credentials"] = json!([{
            "source": "/etc/kuggis/another", "target": target
        }]);
        rejected(value);
    }
    let mut value = example();
    value["resources"][0]["credentials"] = json!([{
        "source": "/etc/kuggis/another", "target": ".ssh/deployment"
    }]);
    workspace(value); // A shared string prefix is not a shared path component.
}

#[test]
fn duplicate_resources_and_endpoint_policies_are_rejected() {
    let mut value = example();
    value["resources"][1]["name"] = value["resources"][0]["name"].clone();
    rejected(value);
    let mut value = example();
    value["resources"][1]["endpoints"][0] = value["resources"][0]["endpoints"][0].clone();
    rejected(value);
}

#[test]
fn endpoint_policy_does_not_form_a_host_port_cross_product() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    let policy = Policy::new(&workspace);
    assert!(policy.allows_resolved(&host("github.com"), 443, &addresses(&["140.82.112.3"])));
    assert!(policy.allows_resolved(
        &host("production.example"),
        22,
        &addresses(&["10.20.30.40"])
    ));
    assert!(!policy.allows_resolved(&host("github.com"), 22, &addresses(&["140.82.112.3"])));
    assert!(!policy.allows_resolved(
        &host("production.example"),
        443,
        &addresses(&["10.20.30.40"])
    ));
    assert!(!policy.allows_resolved(
        &host("door-anything.int.exe.xyz"),
        443,
        &addresses(&["169.254.169.254"])
    ));
}

#[test]
fn dns_must_be_nonempty_and_every_address_must_pass() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    let policy = Policy::new(&workspace);
    assert!(!policy.allows_resolved(&host("github.com"), 443, &[]));
    assert!(!policy.allows_resolved(
        &host("github.com"),
        443,
        &addresses(&["140.82.112.3", "127.0.0.1"])
    ));
    assert!(!policy.allows_resolved(
        &host("production.example"),
        22,
        &addresses(&["10.20.30.41"])
    ));
    assert!(!policy.allows_resolved(
        &host("production.example"),
        22,
        &addresses(&["10.20.30.40", "1.1.1.1"])
    ));
}

#[test]
fn public_policy_excludes_local_metadata_and_transition_addresses() {
    for value in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.1.2.3",
        "224.0.0.1",
        "240.0.0.1",
        "192.0.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "::",
        "::1",
        "fe80::1",
        "fd00::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "64:ff9b::a9fe:a9fe",
        "2002:7f00:1::",
        "2001::1",
        "2001:db8::1",
        "3fff::1",
    ] {
        assert!(!public_address(value.parse().unwrap()), "{value}");
    }
    for value in ["1.1.1.1", "140.82.112.3", "2606:4700:4700::1111"] {
        assert!(public_address(value.parse().unwrap()), "{value}");
    }
}

#[test]
fn link_local_platform_services_require_exact_explicit_pins() {
    let mut config = example();
    let endpoint = json!({"host": "llm.int.exe.xyz", "port": 443});
    config["resources"][0]["endpoints"] = json!([endpoint]);
    let public = workspace(config.clone());
    assert!(!Policy::new(&public).allows_resolved(
        &host("llm.int.exe.xyz"),
        443,
        &addresses(&["169.254.169.254"])
    ));
    config["resources"][0]["endpoints"][0]["destination"] =
        json!({"kind": "pinned", "addresses": ["169.254.169.254"]});
    let pinned = workspace(config);
    let policy = Policy::new(&pinned);
    assert!(policy.allows_resolved(
        &host("llm.int.exe.xyz"),
        443,
        &addresses(&["169.254.169.254"])
    ));
    for (name, port, ips) in [
        ("llm.int.exe.xyz", 80, vec!["169.254.169.254"]),
        ("169.254.169.254", 443, vec!["169.254.169.254"]),
        ("another.int.exe.xyz", 443, vec!["169.254.169.254"]),
        ("llm.int.exe.xyz", 443, vec!["169.254.169.253"]),
        ("llm.int.exe.xyz", 443, vec!["169.254.169.254", "1.1.1.1"]),
    ] {
        assert!(!policy.allows_resolved(&host(name), port, &addresses(&ips)));
    }
}

#[test]
fn pins_cannot_include_loopback_scoped_ipv6_or_multicast_addresses() {
    for value in [
        "127.0.0.1",
        "::ffff:127.0.0.1",
        "::",
        "fe80::1",
        "224.0.0.1",
    ] {
        let mut config = example();
        config["resources"][1]["endpoints"][0]["destination"]["addresses"] = json!([value]);
        rejected(config);
    }
    let mut value = example();
    value["resources"][1]["endpoints"][0]["destination"]["addresses"] = json!([]);
    rejected(value);
}

#[test]
fn ports_are_nonzero_and_private_relays_cannot_shadow_management_ports() {
    for port in [0, 65536] {
        let mut value = example();
        value["resources"][0]["endpoints"][0]["port"] = json!(port);
        rejected(value);
    }
    let mut value = example();
    value["private_web"][0]["host_port"] = json!(22);
    rejected(value);
    let mut value = example();
    value["private_web"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "another", "guest_port": 4000, "host_port": 8000}));
    rejected(value);
}

#[test]
fn rendering_is_explicitly_a_draft_with_private_ingress() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    let environment = Environment::describe(&workspace);
    assert_eq!(
        environment.status,
        kuggis::environment::DescriptionStatus::ConfigurationOnly
    );
    assert_ne!(environment.guest_root_host_uid, 0);
    assert_eq!(environment.host_data, "/var/lib/kuggis/rootfs");
    assert_eq!(environment.private_web[0].host_bind, "127.0.0.1");
    assert!(environment.private_web[0].requires_authenticated_ingress);
}

#[test]
fn ssh_is_noninteractive_for_auth_and_never_passes_launch_to_host_shell() {
    let mut value = example();
    value["launch"] = json!(["bash", "-c", "printf guest-only"]);
    let workspace = workspace(value);
    let invocation = Invocation::for_request(
        &workspace,
        &Request::Chat("default".to_owned().try_into().unwrap()),
    );
    assert_eq!(invocation.args[0], "-tt");
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
    ] {
        assert!(invocation.args.contains(&option.to_owned()));
    }
    let remote = invocation.args.last().unwrap();
    assert!(remote.contains("'sudo' '-n' '--'"));
    assert!(remote.contains(HOST_HELPER));
    assert!(!remote.contains("guest-only"));
}

#[test]
fn file_transfer_is_binary_and_remote_shell_cannot_expand_paths() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    let path = "reports/a'b $(printf EXPANDED); final.txt";
    for request in [
        Request::Put {
            path: path.to_owned().try_into().unwrap(),
            replace: false,
        },
        Request::Get {
            path: path.to_owned().try_into().unwrap(),
        },
    ] {
        let invocation = Invocation::for_request(&workspace, &request);
        assert_eq!(invocation.args[0], "-T");
        assert!(
            invocation.stdin == kuggis::ssh::Stream::FileFrame
                || invocation.stdout == kuggis::ssh::Stream::FileFrame
        );
        // Parse the remote words with a real shell without executing the helper.
        let script = format!(
            "set -- {}; printf '%s\\0' \"$@\"",
            invocation.args.last().unwrap()
        );
        let output = Command::new("/bin/sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success());
        let words: Vec<&[u8]> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .collect();
        assert_eq!(words.last().unwrap(), &path.as_bytes());
        assert_eq!(words[words.len() - 2], b"--");
    }
}

#[test]
fn overwrite_is_an_explicit_noninteractive_operation_choice() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    for replace in [false, true] {
        let invocation = Invocation::for_request(
            &workspace,
            &Request::Put {
                path: "output/file".to_owned().try_into().unwrap(),
                replace,
            },
        );
        assert_eq!(
            invocation.args.last().unwrap().contains("'--replace'"),
            replace
        );
    }
}
