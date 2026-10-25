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
    if let Err(error) = child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        // Invalid arguments can be rejected before stdin is read. Still inspect
        // exit status/output below instead of racing that legitimate close.
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
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
    let secret = d.join("widget.key");
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

    let said = r#"{"verdict":{"repo":"example/widget","head":"0123456789abcdef0123456789abcdef01234567","pass":true}}"#;
    let signed = run(&["say", "widget", "1", secret.to_str().unwrap()], said);
    assert!(signed.status.success(), "{}", err(&signed));
    let statement = out(&signed);
    assert!(statement.contains("\"cell\":\"widget\""));

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

    let forged_cell = statement.replace("\"cell\":\"widget\"", "\"cell\":\"coordinator\"");
    assert!(!run(&["open", &public], &forged_cell).status.success());

    // a cell name that is a command word, a sequence that is not a number, a bad said
    assert!(!run(&["say", "plan", "1", secret.to_str().unwrap()], said)
        .status
        .success());
    assert!(
        !run(&["say", "widget", "one", secret.to_str().unwrap()], said)
            .status
            .success()
    );
    assert!(!run(
        &["say", "widget", "1", secret.to_str().unwrap()],
        r#"{"verdict":{"repo":"example/widget","head":"short","pass":true}}"#
    )
    .status
    .success());
    assert!(!run(
        &["say", "widget", "1", secret.to_str().unwrap()],
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
    fs::create_dir_all(cells.join("coordinator")).unwrap();
    let coordinator = write(
        &cells,
        "coordinator.json",
        r#"{"name":"coordinator","grants":[{"provider":"anthropic"},{"provider":"openai"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"},{"host":"files.pythonhosted.org"}]}"#,
    );
    let widget = write(
        &cells.join("coordinator"),
        "widget.json",
        r#"{"name":"widget","grants":[{"provider":"anthropic"},{"provider":"openai"},{"code":{"repo":"example/widget","write":true}},{"code":{"repo":"upstream/widget","write":false}},{"host":"pypi.org"},{"host":"files.pythonhosted.org"}]}"#,
    );
    fs::create_dir_all(cells.join("coordinator/widget")).unwrap();
    let scratch = write(
        &cells.join("coordinator/widget"),
        "widget-scratch.json",
        r#"{"name":"widget-scratch","grants":[{"provider":"anthropic"},{"code":{"repo":"example/widget","write":false}}]}"#,
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
        .any(|l| l.starts_with("widget-scratch\twidget\t2 grants")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("widget\tcoordinator\t6 grants")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("coordinator\t-\t6 grants")));

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
    let before = hash_of(&out(&t), "widget");
    fs::rename(&widget, cells.join("widget.json")).unwrap();
    fs::rename(cells.join("coordinator/widget"), cells.join("widget")).unwrap();
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(t.status.success(), "{}", err(&t));
    assert!(out(&t).lines().any(|l| l.starts_with("widget\t-\t")));
    assert!(out(&t)
        .lines()
        .any(|l| l.starts_with("widget-scratch\twidget\t")));
    assert_eq!(hash_of(&out(&t), "widget"), before);
    fs::rename(cells.join("widget.json"), &widget).unwrap();
    fs::rename(cells.join("widget"), cells.join("coordinator/widget")).unwrap();

    // a child wider than its parent
    let wide = write(
        &cells.join("coordinator"),
        "wide.json",
        r#"{"name":"wide","grants":[{"host":"evil.example"}]}"#,
    );
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(
        err(&t).contains("You must add these hosts to parent cell coordinator. (Children cannot have more access than their parents)."),
        "{}", err(&t)
    );
    fs::remove_file(&wide).unwrap();

    // a file that lies about its name
    let liar = write(&cells, "liar.json", r#"{"name":"coordinator","grants":[]}"#);
    let t = run(&["tree", cells.to_str().unwrap()], "");
    assert!(!t.status.success());
    assert!(
        err(&t).contains("liar.json: holds cell coordinator"),
        "{}",
        err(&t)
    );
    fs::remove_file(&liar).unwrap();

    // a cell file carrying its own edge
    let smuggler = write(
        &cells,
        "smuggler.json",
        r#"{"name":"smuggler","parent":"coordinator","grants":[]}"#,
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
    let t = run(&["tree", &coordinator], "");
    assert!(!t.status.success(), "a file is not a tree");

    let a = run(&["allow", &widget], "");
    assert_eq!(
        out(&a).lines().collect::<Vec<_>>(),
        [
            "api.anthropic.com",
            "api.openai.com",
            "auth.openai.com",
            "chatgpt.com",
            "claude.ai",
            "files.pythonhosted.org",
            "platform.claude.com",
            "pypi.org"
        ]
    );

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let o = run(&["observe", "--from", fixtures.to_str().unwrap()], "");
    assert!(o.status.success(), "{}", err(&o));
    let observed: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(
        observed["cells"]["widget"]["grants"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "a door is not a grant: {}",
        observed["cells"]["widget"]
    );
    assert_eq!(
        observed["doors"],
        serde_json::json!({"widget": ["widget-scratch"]})
    );
    assert!(
        observed["cells"].get("svc-1").is_none() && observed["cells"].get("toolbox-dev").is_none(),
        "a VM without a cell tag is not a cell: {}",
        observed["cells"]
    );
    assert_eq!(observed["cells"].as_object().unwrap().len(), 2);
    let have = write(&d, "have.json", &observed.to_string());

    let dir = cells.to_str().unwrap();
    let p = run(&["plan", dir, "widget", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    assert_eq!(
        plan["widget"]["actions"],
        serde_json::json!(["setup", {"detach":{"code":{"repo":"example/widget","write":true}}}, {"detach":{"code":{"repo":"upstream/widget","write":false}}}, {"detach":{"edge":"codex"}}]),
        "replace the legacy Codex edge with cell-local Pi sign-in: {plan}"
    );
    let approval = err(&p).rsplit(' ').next().unwrap().to_string();
    assert_eq!(approval.len(), 64);

    // the tree decides the door: drop the child and the plan closes it
    fs::remove_file(&scratch).unwrap();
    let p = run(&["plan", dir, "widget", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    assert_eq!(
        plan["widget"]["actions"],
        serde_json::json!(["setup", {"detach":{"code":{"repo":"example/widget","write":true}}}, {"detach":{"code":{"repo":"upstream/widget","write":false}}}, {"detach":{"edge":"codex"}}, {"close": "widget-scratch"}]),
        "{plan}"
    );
    assert_ne!(err(&p).rsplit(' ').next().unwrap(), approval);

    // a cell the edge has never seen: create, attach, open, setup
    let p = run(&["plan", dir, "coordinator", "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    let actions = plan["coordinator"]["actions"].as_array().unwrap();
    assert_eq!(actions[0], "create");
    assert!(
        actions.contains(&serde_json::json!({"open": "widget"})),
        "{plan}"
    );
    assert_eq!(actions.last().unwrap(), "setup");

    // the whole tree at once, under one hash
    let p = run(&["plan", dir, "--have", &have], "");
    assert!(p.status.success(), "{}", err(&p));
    let plan: serde_json::Value = serde_json::from_str(&out(&p)).unwrap();
    assert_eq!(plan.as_object().unwrap().len(), 2, "{plan}");
    assert_eq!(plan["coordinator"]["actions"][0], "create");
    assert_eq!(
        plan["widget"]["actions"],
        serde_json::json!(["setup", {"detach":{"code":{"repo":"example/widget","write":true}}}, {"detach":{"code":{"repo":"upstream/widget","write":false}}}, {"detach":{"edge":"codex"}}, {"close": "widget-scratch"}])
    );

    let p = run(&["apply", dir, "--approve", "00"], "");
    assert!(!p.status.success());

    let p = run(&["plan", dir, "nobody", "--have", &have], "");
    assert!(!p.status.success());
    assert!(err(&p).contains("nobody: not in the tree"), "{}", err(&p));
}

#[test]
fn the_agent_speaks_through_the_outbox() {
    let d = tmp("ship");
    let outbox = d.join("outbox");
    let said = d.join("said");
    fs::create_dir_all(&outbox).unwrap();
    fs::create_dir_all(&said).unwrap();
    let cell = write(
        &d,
        "cell.json",
        r#"{"name":"a","grants":[{"provider":"anthropic"}]}"#,
    );
    let secret = d.join("secret");
    let k = run(&["key", "new", secret.to_str().unwrap()], "");
    let key = out(&k).trim().to_string();
    write(
        &outbox,
        "q.json",
        r#"{"session":{"session":{"cell":"a","id":"00000000000000000000000000000001"},"state":"question","text":"merge?"}}"#,
    );
    write(
        &outbox,
        "x.json",
        r#"{"checked":{"cell":"0000000000000000000000000000000000000000000000000000000000000000","ok":true,"report":""}}"#,
    );
    let args = [
        "ship",
        "--cell",
        &cell,
        "--secret",
        secret.to_str().unwrap(),
        "--outbox",
        outbox.to_str().unwrap(),
        "--said",
        said.to_str().unwrap(),
        "--once",
    ];
    let s = run(&args, "");
    assert!(s.status.success(), "{}", err(&s));
    assert_eq!(
        out(&s).lines().collect::<Vec<_>>(),
        [
            "q.json -> 1",
            "x.json refused: only the wall check says checked"
        ]
    );
    assert!(outbox.join("q.1").exists());
    assert!(outbox.join("x.refused").exists());
    let statement = fs::read_to_string(said.join("1.json")).unwrap();
    let o = run(&["open", &key], &statement);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains(r#""text":"merge?""#), "{}", out(&o));
    let s = run(&args, "");
    assert!(
        s.status.success() && out(&s).is_empty(),
        "nothing left to ship"
    );
}

#[test]
fn a_knock_from_root_has_no_name() {
    let d = tmp("knock");
    let inbox = d.join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let k = run(
        &["knock", "--inbox", inbox.to_str().unwrap()],
        r#"{"brief":{"request":"00000000000000000000000000000002","session":{"cell":"a","id":"00000000000000000000000000000001"},"text":"go"}}"#,
    );
    assert!(k.status.success(), "{}", err(&k));
    assert_eq!(out(&k).trim(), "1");
    let filed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(inbox.join("1.json")).unwrap()).unwrap();
    assert!(filed.get("from").is_none(), "{filed}");
    assert_eq!(filed["said"]["brief"]["text"], "go");
    let k = run(&["knock", "--inbox", inbox.to_str().unwrap()], "junk");
    assert!(!k.status.success());
}

#[test]
fn a_session_id_is_generated_before_submitting_a_brief() {
    let first = run(&["session", "new"], "");
    let second = run(&["session", "new"], "");
    assert!(first.status.success(), "{}", err(&first));
    assert!(out(&first).parse::<cell::SessionId>().is_ok());
    assert_ne!(out(&first), out(&second));
    let directory = tmp("session-id");
    write(&directory, "dev.json", r#"{"name":"dev","grants":[]}"#);
    let invalid = run(
        &["brief", directory.to_str().unwrap(), "dev", "cleanup", "Go"],
        "",
    );
    assert!(!invalid.status.success(), "session titles are not IDs");
    assert!(err(&invalid).contains("session id"), "{}", err(&invalid));
}

#[test]
fn no_command_prints_usage_and_fails() {
    let o = run(&[], "");
    assert!(!o.status.success());
    assert!(err(&o).contains("usage:"));
}

#[test]
fn publisher_policy_approval_and_trusted_signer_across_processes() {
    use cell::publish::{
        registry::Registry,
        transport::{Reply, SignedRequest},
    };
    let d = tmp("publisher");
    let registry = d.join("registry");
    let registry_arg = registry.to_str().unwrap();
    let initialized = run(&["publisher", "init", registry_arg], "");
    assert!(initialized.status.success(), "{}", err(&initialized));
    let audience = out(&initialized);
    let keyfile = d.join("parent.key");
    let minted = run(&["key", "new", keyfile.to_str().unwrap()], "");
    assert!(minted.status.success(), "{}", err(&minted));
    let public = out(&minted);
    let allocated = run(&["publisher", "namespace", registry_arg, &public], "");
    assert!(allocated.status.success(), "{}", err(&allocated));
    let namespace = out(&allocated);
    let tree = d.join("tree");
    fs::create_dir_all(tree.join(".pins")).unwrap();
    let declaration =
        serde_json::json!({"name":"parent", "grants":[{"publish":{"namespace":namespace}}]});
    fs::write(tree.join("parent.json"), declaration.to_string()).unwrap();
    fs::write(tree.join(".pins/parent.pub"), &public).unwrap();
    let tree_arg = tree.to_str().unwrap();
    let plan = run(&["publisher", "plan", registry_arg, tree_arg], "");
    assert!(plan.status.success(), "{}", err(&plan));
    let value: serde_json::Value = serde_json::from_str(&out(&plan)).unwrap();
    assert_eq!(value["audience"], audience);
    assert_eq!(value["cells"][0]["key"], public);
    let approval = err(&plan)
        .strip_prefix("approve with ")
        .unwrap()
        .to_string();
    assert!(!run(&["publisher", "apply", registry_arg, tree_arg], "")
        .status
        .success());
    assert!(!run(
        &[
            "publisher",
            "plan",
            registry_arg,
            tree_arg,
            "--pin",
            "ignored"
        ],
        ""
    )
    .status
    .success());
    assert!(!run(
        &[
            "publisher",
            "apply",
            registry_arg,
            tree_arg,
            "--approve",
            &"00".repeat(32)
        ],
        ""
    )
    .status
    .success());
    let applied = run(
        &[
            "publisher",
            "apply",
            registry_arg,
            tree_arg,
            "--approve",
            &approval,
        ],
        "",
    );
    assert!(applied.status.success(), "{}", err(&applied));
    let command =
        serde_json::json!({"reserve_site":{"site":{"namespace":namespace,"name":"news"}}});
    let signed = run(
        &[
            "publisher",
            "sign",
            "--secret",
            keyfile.to_str().unwrap(),
            "--audience",
            &audience,
        ],
        &command.to_string(),
    );
    assert!(signed.status.success(), "{}", err(&signed));
    let request: SignedRequest = serde_json::from_str(&out(&signed)).unwrap();
    let opened = Registry::open(&registry).unwrap();
    assert_eq!(opened.execute(&request).unwrap(), Reply::Reserved);
    assert!(opened.execute(&request).is_err());
    // A pin changed after preview cannot use that earlier approval.
    let preview = run(&["publisher", "plan", registry_arg, tree_arg], "");
    let approval = err(&preview)
        .strip_prefix("approve with ")
        .unwrap()
        .to_string();
    fs::write(
        tree.join(".pins/parent.pub"),
        cell::Secret::generate().public().to_string(),
    )
    .unwrap();
    assert!(!run(
        &[
            "publisher",
            "apply",
            registry_arg,
            tree_arg,
            "--approve",
            &approval
        ],
        ""
    )
    .status
    .success());
    // Listen policy is enforced before any public plaintext socket is opened.
    let serve = run(
        &["publisher", "serve", registry_arg, "--listen", "0.0.0.0:0"],
        "",
    );
    assert!(!serve.status.success());
    assert!(err(&serve).contains("loopback"));
    fs::remove_dir_all(d).unwrap();
}
