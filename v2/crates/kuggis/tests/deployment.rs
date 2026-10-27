use kuggis::{
    deploy::{normalize_nft, Bundle},
    Workspace,
};
use serde_json::json;

const EXAMPLE: &str = include_str!("../../../examples/workspace.json");

#[test]
fn memory_limit_is_a_positive_declarative_byte_count() {
    let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    let default = Workspace::parse(EXAMPLE).unwrap();
    assert_eq!(default.memory_bytes().get(), 4 * 1024 * 1024 * 1024);
    config["memory_bytes"] = json!(56_u64 * 1024 * 1024 * 1024);
    let workspace = Workspace::parse(&config.to_string()).unwrap();
    let bundle = Bundle::compile(&workspace);
    assert!(bundle.files["/etc/systemd/system/kuggis-v2.service"]
        .content
        .contains("MemoryMax=60129542144\n"));
    for invalid in [
        json!(0),
        json!(-1),
        json!("56G"),
        json!(true),
        json!(u64::MAX),
    ] {
        config["memory_bytes"] = invalid;
        assert!(Workspace::parse(&config.to_string()).is_err());
    }
}

#[test]
fn compile_is_deterministic_and_carries_no_credential_values() {
    let workspace = Workspace::parse(EXAMPLE).unwrap();
    let first = serde_json::to_vec(&Bundle::compile(&workspace)).unwrap();
    let second = serde_json::to_vec(&Bundle::compile(&workspace)).unwrap();
    assert_eq!(first, second);
    let bundle = Bundle::compile(&workspace);
    let service = &bundle.files["/etc/systemd/system/kuggis-v2.service"].content;
    assert!(service.contains("--private-users=524288:65536"));
    assert!(service.contains("--settings=no"));
    assert!(!service.contains("/etc/kuggis/credentials"));
    assert!(!service.contains("/home/agent"));
    assert!(bundle.files["/etc/kuggis/wall.nft"]
        .content
        .contains("oifname \"kv2-host\" counter reject"));
}

#[test]
fn no_user_launch_string_is_embedded_in_host_units() {
    let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    config["launch"] = json!(["bash", "-c", "guest-only; ${ESCAPE} %i\nExecStart=/bad"]);
    let workspace = Workspace::parse(&config.to_string()).unwrap();
    for (path, file) in Bundle::compile(&workspace).files {
        if path.ends_with(".service") {
            assert!(!file.content.contains("guest-only"));
        }
    }
}

#[test]
fn nft_normalization_ignores_counters_but_preserves_security_semantics() {
    let mut original = json!({"nftables":[
        {"metainfo":{"version":"one"}},
        {"chain":{"name":"input","handle":1}},
        {"rule":{"chain":"input","handle":2,"expr":[{"counter":{"packets":0,"bytes":0}},{"accept":null}]}},
        {"rule":{"chain":"input","handle":3,"expr":[{"drop":null}]}}
    ]});
    let expected = normalize_nft(&original).unwrap();
    original["nftables"][0]["metainfo"]["version"] = json!("two");
    original["nftables"][2]["rule"]["handle"] = json!(99);
    original["nftables"][2]["rule"]["expr"][0]["counter"]["packets"] = json!(1000);
    assert_eq!(normalize_nft(&original).unwrap(), expected);
    original["nftables"].as_array_mut().unwrap().swap(2, 3);
    assert_ne!(normalize_nft(&original).unwrap(), expected);
}

#[test]
fn guest_network_cannot_be_granted_as_an_external_resource() {
    for address in ["10.205.0.1", "10.205.0.2", "::ffff:10.205.0.1"] {
        let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        config["resources"][1]["endpoints"][0]["destination"]["addresses"] = json!([address]);
        assert!(Workspace::parse(&config.to_string()).is_err());
    }
}

#[test]
fn package_names_and_proxy_port_collisions_fail_validation() {
    for package in [
        "--allow-unauthenticated",
        "tool;id",
        "tool\nExecStart=bad",
        "../tool",
    ] {
        let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        config["packages"] = json!([package]);
        assert!(Workspace::parse(&config.to_string()).is_err());
    }
    let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    config["private_web"][0]["host_port"] = json!(3128);
    assert!(Workspace::parse(&config.to_string()).is_err());
}

#[test]
fn exe_ingress_cannot_point_at_a_different_vm_or_undeclared_port() {
    let mut config: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    config["ingress"] = json!({"kind":"exe", "vm":"test-box", "web":"preview"});
    assert!(Workspace::parse(&config.to_string()).is_err());
    config["ssh"] = json!("test-box.exe.xyz");
    assert!(Workspace::parse(&config.to_string()).is_ok());
    config["ingress"]["web"] = json!("not-declared");
    assert!(Workspace::parse(&config.to_string()).is_err());
    config["ingress"] = json!({"kind":"public"});
    assert!(Workspace::parse(&config.to_string()).is_err());
}
