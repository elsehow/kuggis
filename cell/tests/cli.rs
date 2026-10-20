//! The binary, driven as a separate process: keys minted in one place,
//! statements signed in another, opened in a third, a tree read from a
//! directory, and a plan drawn from a cell file against an edge listing.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cell")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cell-cli-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).trim().to_string()
}

fn write(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn keys_statements_and_replies_across_processes() {
    let d = tmp("keys");
    let secret = d.join("airo.key");
    let minted = run(&["key", "new", secret.to_str().unwrap()], "");
    assert!(minted.status.success(), "{}", err(&minted));
    let public = out(&minted);
    assert_eq!(public.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let again = run(&["key", "new", secret.to_str().unwrap()], "");
    assert!(!again.status.success(), "a secret is never overwritten");
    assert_eq!(
        out(&run(&["key", "pub", secret.to_str().unwrap()], "")),
        public
    );

    let said = r#"{"verdict":{"repo":"elsehow/airo","head":"0123456789abcdef0123456789abcdef01234567","pass":true}}"#;
    let signed = run(&["say", "airo", "1", secret.to_str().unwrap()], said);
    assert!(signed.status.success(), "{}", err(&signed));
    let statement = out(&signed);
    assert!(statement.contains("\"cell\":\"airo\""));

    let opened = run(&["open", &public], &statement);
    assert!(opened.status.success(), "{}", err(&opened));
    assert_eq!(out(&opened), said);

    let other = d.join("other.key");
    let other_pub = out(&run(&["key", "new", other.to_str().unwrap()], ""));
    let opened = run(&["open", &other_pub], &statement);
    assert!(!opened.status.success());
    assert!(err(&opened).contains("does not verify"));

    let tampered = statement.replace("\"pass\":true", "\"pass\":false");
    assert_ne!(tampered, statement);
    assert!(!run(&["open", &public], &tampered).status.success());

    let forged_cell = statement.replace("\"cell\":\"airo\"", "\"cell\":\"forecasting\"");
    assert!(!run(&["open", &public], &forged_cell).status.success());

    // a cell name that is a command word, a sequence that is not a number, a bad said
    assert!(!run(&["say", "plan", "1", secret.to_str().unwrap()], said)
        .status
        .success());
    assert!(
        !run(&["say", "airo", "one", secret.to_str().unwrap()], said)
            .status
            .success()
    );
    assert!(!run(
        &["say", "airo", "1", secret.to_str().unwrap()],
        r#"{"verdict":{"repo":"elsehow/airo","head":"short","pass":true}}"#
    )
    .status
    .success());
    assert!(!run(
        &["say", "airo", "1", secret.to_str().unwrap()],
        r#"{"fetch":{"url":"http://x.com"}}"#
    )
    .status
    .success());
    assert!(!run(&["open", "not-a-key"], &statement).status.success());
}

#[test]
fn tree_allow_plan_and_observe() {
    let d = tmp("tree");
    let cells = d.join("cells");
    fs::create_dir_all(cells.join("forecasting")).unwrap();
    let forecasting = write(
        &cells,
        "forecasting.json",
        r#"{"name":"forecasting","grants":[{"agent":"claude"},{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}},{"code":{"repo":"forecastingresearch/airo","write":false}},{"host":"pypi.org"},{"host":"files.pythonhosted.org"}]}"#,
    );
    let airo = write(
        &cells.join("forecasting"),
        "airo.json",
        r#"{"name":"airo","grants":[{"agent":"claude"},{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}},{"code":{"repo":"forecastingresearch/airo","write":false}},{"host":"pypi.org"},{"host":"files.pythonhosted.org"}]}"#,
    );
    fs::create_dir_all(cells.join("forecasting/airo")).unwrap();
    let scratch = write(
        &cells.join("forecasting/airo"),
        "airo-scratch.json",
        r#"{"name":"airo-scratch","grants":[{"agent":"claude"},{"code":{"repo":"elsehow/airo","write":false}}]}"#,
    );
    write(&cells, "README.md", "not a cell");

    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(t.status.success(), "{}", err(&t));
    assert_eq!(
        out(&t).lines().count(),
        3,
        "the path gives the edge; other files are skipped:\n{}",
        out(&t)
    );
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("airo-scratch\tairo\t2 grants")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("airo\tforecasting\t6 grants")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("forecasting\t-\t6 grants")));

    // re-parenting is mv: a subtree moved to the top keeps its hashes
    let hash_of = |listing: &str, name: &str| -> String {
        listing
            .lines()
            .find(|l| l.starts_with(&format!("{name}\t")))
            .unwrap()
            .split('\t')
            .nth(3)
            .unwrap()
            .to_string()
    };
    let before = hash_of(&out(&t), "airo");
    fs::rename(&airo, cells.join("airo.json")).unwrap();
    fs::rename(cells.join("forecasting/airo"), cells.join("airo")).unwrap();
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(t.status.success(), "{}", err(&t));
    assert!(out(&t).lines().any(|l| l.starts_with("airo\t-\t")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("airo-scratch\tairo\t")));
    assert_eq!(hash_of(&out(&t), "airo"), before);
    fs::rename(cells.join("airo.json"), &airo).unwrap();
    fs::rename(cells.join("airo"), cells.join("forecasting/airo")).unwrap();

    // a child wider than its parent
    let wide = write(
        &cells.join("forecasting"),
        "wide.json",
        r#"{"name":"wide","grants":[{"host":"evil.example"}]}"#,
    );
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(err(&t).contains("not covered by its parent"), "{}", err(&t));
    fs::remove_file(&wide).unwrap();

    // a file that lies about its name
    let liar = write(&cells, "liar.json", r#"{"name":"forecasting","grants":[]}"#);
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(
        err(&t).contains("liar.json: holds cell forecasting"),
        "{}",
        err(&t)
    );
    fs::remove_file(&liar).unwrap();

    // a cell file carrying its own edge
    let smuggler = write(
        &cells,
        "smuggler.json",
        r#"{"name":"smuggler","parent":"forecasting","grants":[]}"#,
    );
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(err(&t).contains("unknown field `parent`"), "{}", err(&t));
    fs::remove_file(&smuggler).unwrap();

    // a directory with no cell of its own
    fs::create_dir_all(cells.join("nobody")).unwrap();
    write(&cells.join("nobody"), "orphan.json", r#"{"name":"orphan"}"#);
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(
        err(&t).contains("orphan: parent nobody does not exist"),
        "{}",
        err(&t)
    );
    fs::remove_dir_all(cells.join("nobody")).unwrap();

    let bad = d.join("bad");
    fs::create_dir_all(&bad).unwrap();
    write(
        &bad,
        "bad.json",
        r#"{"name":"bad","grants":[{"host":"169.254.169.254"}]}"#,
    );
    let t = run(&["tree", bad.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(err(&t).contains("an address, not a name"), "{}", err(&t));
    let t = run(&["tree", &forecasting], "");
    assert!(!t.status.success(), "a file is not a tree");

    let a = run(&["allow", &airo], "");
    assert_eq!(
        out(&a).lines().collect::<Vec<_>>(),
        [
            "api.anthropic.com",
            "codex.int.exe.xyz",
            "files.pythonhosted.org",
            "github.int.exe.xyz",
            "pypi.org"
        ]
    );

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let o = run(&["observe", "--from", fixtures.to_str().unwrap()], "");
    assert!(o.status.success(), "{}", err(&o));
    let observed: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(
        observed["cells"]["airo"]["grants"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "a door is not a grant: {}",
        observed["cells"]["airo"]
    );
    assert_eq!(
        observed["doors"],
        serde_json::json!({"airo": ["airo-scratch"]})
    );
    assert!(
        observed["cells"].get("svc-1").is_some(),
        "every nameable VM is observed, with whatever it has"
    );
    assert_eq!(
        observed["cells"]["svc-1"]["grants"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let have = write(&d, "have.json", &observed.to_string());

    let dir = cells.to_str().unwrap();
    let p = run(&["plan", dir, "airo", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    assert_eq!(
        plan["actions"],
        serde_json::json!(["setup"]),
        "edge and door match; only the box needs the file: {plan}"
    );
    let approval = err(&p).rsplit(' ').next().unwrap().to_string();
    assert_eq!(approval.len(), 64);

    // the tree decides the door: drop the child and the plan closes it
    fs::remove_file(&scratch).unwrap();
    let p = run(&["plan", dir, "airo", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    assert_eq!(
        plan["actions"],
        serde_json::json!([{"close": "airo-scratch"}, "setup"]),
        "{plan}"
    );
    assert_ne!(err(&p).rsplit(' ').next().unwrap(), approval);

    // a cell the edge has never seen: create, attach, open, setup
    let p = run(&["plan", dir, "forecasting", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    let actions = plan["actions"].as_array().unwrap();
    assert_eq!(actions[0], "create");
    assert!(
        actions.contains(&serde_json::json!({"open": "airo"})),
        "{plan}"
    );
    assert_eq!(actions.last().unwrap(), "setup");

    let p = run(&["plan", dir, "nobody", "--have", &have], "");
    assert!(!p.status.success());
    assert!(err(&p).contains("nobody: not in the tree"), "{}", err(&p));
}

#[test]
fn no_command_prints_usage_and_fails() {
    let o = run(&[], "");
    assert!(!o.status.success());
    assert!(err(&o).contains("usage:"));
}
