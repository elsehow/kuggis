//! Run the real CLI against a fake SSH executable: no provider or network.
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("cell-host-cli-{}", cell::RequestId::generate()));
        fs::create_dir_all(&root).unwrap();
        let config = json!({"hostname":"worker.example","user":"operator","port":2222,
            "identity_file":root.join("identity"),"known_hosts":root.join("known_hosts"),
            "runtime":{"backend":"ssh","dns":["127.0.0.53"],"memory_max":8589934592_u64}});
        fs::write(root.join("host.json"), serde_json::to_vec(&config).unwrap()).unwrap();
        fs::write(
            root.join("cell.json"),
            r#"{"name":"worker","grants":[{"provider":"openai"}]}"#,
        )
        .unwrap();
        fs::write(root.join("known_hosts"), "fixture host key\n").unwrap();
        let mut elf = vec![0; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[18] = 62;
        fs::write(root.join("cell-linux"), elf).unwrap();
        fs::write(
            root.join("ssh"),
            r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['SSH_HOST_FIXTURE'])
with (root / 'calls.jsonl').open('a') as f: f.write(json.dumps(sys.argv[1:]) + '\n')
assert sys.argv[-1].endswith(' inspect'), 'no mutations expected'
print(json.dumps(dict(machine_id='0123456789abcdef0123456789abcdef', os='ubuntu', version='24.04',
    init='systemd', arch='x86_64', dns=['127.0.0.53'], memory_total=17179869184,
    cell=None, key=None, runtime=None, binary=None, checked=None)))
"#,
        )
        .unwrap();
        fs::set_permissions(root.join("ssh"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }
    fn run(&self, action: &str, extra: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cell"));
        cmd.env(
            "PATH",
            format!("{}:{}", self.root.display(), std::env::var("PATH").unwrap()),
        )
        .env("SSH_HOST_FIXTURE", &self.root)
        .arg("host")
        .arg(action);
        if action != "inspect" {
            cmd.arg(self.root.join("cell.json"));
        }
        cmd.arg(self.root.join("host.json"));
        if action != "inspect" {
            cmd.arg("--bin")
                .arg(self.root.join("cell-linux"))
                .arg("--state")
                .arg(self.root.join("state"));
        }
        cmd.args(extra).output().unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cli_plan_uses_pinned_ssh_and_stale_approval_never_bootstraps() {
    let f = Fixture::new();
    let out = f.run("plan", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["actions"], json!(["setup"]));
    assert_eq!(plan["host"]["runtime"]["memory_max"], 8589934592_u64);
    assert!(String::from_utf8_lossy(&out.stderr).contains("approve with "));
    let call = &f.calls()[0];
    assert!(call.contains(&"StrictHostKeyChecking=yes".into()));
    assert!(call.contains(&"worker.example".into()));
    let trust = call
        .iter()
        .find(|s| s.starts_with("UserKnownHostsFile="))
        .unwrap();
    assert!(
        !trust.contains(f.root.join("known_hosts").to_str().unwrap()),
        "SSH receives a frozen trust snapshot"
    );
    let out = f.run("apply", &["--approve", &"0".repeat(64)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("changed; review"));
    assert!(f
        .calls()
        .iter()
        .all(|args| args.last().unwrap().ends_with(" inspect")));
    assert!(!f.root.join("state/host-pin.json").exists());
}

#[test]
fn cli_rejects_unsupported_grants_and_invalid_options_before_ssh() {
    let f = Fixture::new();
    for options in [
        vec!["--state"],
        vec!["--mystery", "value"],
        vec!["--approve", "abc"],
        vec!["--bin", "elsewhere"],
    ] {
        assert!(!f.run("plan", &options).status.success());
    }
    fs::write(
        f.root.join("cell.json"),
        r#"{"name":"worker","grants":[{"edge":"search"}]}"#,
    )
    .unwrap();
    let out = f.run("plan", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("do not yet support grant"));
    assert!(f.calls().is_empty());
}

#[test]
fn cli_rejects_non_linux_builds_before_ssh() {
    let f = Fixture::new();
    fs::write(f.root.join("cell-linux"), b"not a linux executable").unwrap();
    assert!(!f.run("plan", &[]).status.success());
    assert!(f.calls().is_empty());
}

#[test]
fn portable_proxy_refuses_implicit_exe_doors() {
    let cell = cell::Cell::new("worker".parse().unwrap(), []);
    let mut policy = cell::proxy::Policy::for_cell(&cell);
    let door = "door-child.int.exe.xyz".parse().unwrap();
    assert!(policy.allows(&door));
    policy.allow_doors = false;
    assert!(!policy.allows(&door));
    assert!(!policy.allows(&"example.com".parse().unwrap()));
}
