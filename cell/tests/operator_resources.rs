use cell::publish::{
    artifact::{Artifact, Entry},
    registry::Registry,
    transport::{self, Command, SignedRequest},
    PublishScope, SiteId,
};
use cell::{Cell, Grant, RequestId, Secret, Tree};
use std::{
    fs,
    io::{BufRead, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
};
struct Fixture {
    root: PathBuf,
    registry: Registry,
    site: SiteId,
}
struct Ui {
    child: Child,
    authority: String,
}
impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("operator-resources-{}", RequestId::generate()));
        fs::create_dir_all(root.join("tree/.pins")).unwrap();
        fs::create_dir(root.join("sockets")).unwrap();
        let registry = Registry::create(&root.join("registry")).unwrap();
        let secret = Secret::generate();
        let ns = registry.allocate_namespace(secret.public()).unwrap();
        let cell = Cell::new(
            "author".parse().unwrap(),
            [Grant::Publish(PublishScope::Namespace(ns.clone()))],
        );
        fs::write(root.join("tree/author.json"), cell.canonical()).unwrap();
        fs::write(
            root.join("tree/.pins/author.pub"),
            secret.public().to_string(),
        )
        .unwrap();
        registry
            .approve_tree(
                &Tree::new([(cell.clone(), None)]).unwrap(),
                &[(cell.name, secret.public())].into(),
            )
            .unwrap();
        registry
            .configure_public_base("https://sites.example.com".parse().unwrap())
            .unwrap();
        let site = SiteId {
            namespace: ns,
            name: "demo".parse().unwrap(),
        };
        let artifact = Artifact {
            files: vec![Entry {
                path: "index.html".into(),
                bytes: b"hello".to_vec(),
            }],
        };
        let request = SignedRequest::sign(
            &secret,
            registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            Command::Publish {
                site: site.clone(),
                artifact: artifact.hash().unwrap(),
            },
        );
        registry.execute_upload(&request, &artifact).unwrap();
        let config = serde_json::json!({"backends":[{"name":"sites","provider":registry.audience().unwrap(),"location":{"kind":"local","registry":root.join("registry")}}]});
        fs::write(
            root.join("tree/.resources.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        Self {
            root,
            registry,
            site,
        }
    }
    fn ui(&self) -> Ui {
        let mut child = Process::new(env!("CARGO_BIN_EXE_cell"))
            .arg("ui")
            .arg(self.root.join("tree"))
            .arg("--state")
            .arg(self.root.join("operator"))
            .arg("--socket-dir")
            .arg(self.root.join("sockets"))
            .args(["--listen", "127.0.0.1:0"])
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut reader = std::io::BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0, "UI exited");
            if let Some(at) = line.trim().strip_prefix("Kuggis UI: http://") {
                return Ui {
                    child,
                    authority: at.into(),
                };
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn call(ui: &Ui, path: &str, body: Option<serde_json::Value>) -> (u16, serde_json::Value) {
    let body = body.map(|b| b.to_string());
    let bytes = body.as_deref().unwrap_or("");
    let method = if body.is_some() { "POST" } else { "GET" };
    let mut stream = TcpStream::connect(&ui.authority).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}",ui.authority,bytes.len()).unwrap();
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    let (header, body) = output.split_once("\r\n\r\n").unwrap();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    (
        status,
        serde_json::from_str(body).unwrap_or(serde_json::json!({"raw":body})),
    )
}
#[test]
fn operator_http_inventory_review_apply_retry_and_retained_custody() {
    let f = Fixture::new();
    let mut ui = f.ui();
    let (status, view) = call(&ui, "/api/resources", None);
    assert_eq!(status, 200);
    let resource = view["backends"][0]["inventory"]["resources"][0]["id"].clone();
    let (status, plan) = call(
        &ui,
        "/api/resources/plan",
        Some(serde_json::json!({"backend":"sites","resource":resource,"intent":"unpublish"})),
    );
    assert_eq!(status, 200, "{plan}");
    let apply = format!(
        "/api/resources/actions/{}/apply",
        plan["id"].as_str().unwrap()
    );
    assert_ne!(
        call(
            &ui,
            &apply,
            Some(serde_json::json!({"approval":"00".repeat(32)}))
        )
        .0,
        200
    );
    let (status, applied) = call(
        &ui,
        &apply,
        Some(serde_json::json!({"approval":plan["approval"]})),
    );
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["receipt"]["phase"]["state"], "succeeded");
    assert!(f.registry.public_file(&f.site, "index.html").is_err());
    drop(ui);
    ui = f.ui();
    assert_eq!(
        call(
            &ui,
            &apply,
            Some(serde_json::json!({"approval":plan["approval"]}))
        )
        .1,
        applied
    );
    // Inventory and operator actions survive the original declaration/pin disappearing.
    fs::remove_file(f.root.join("tree/author.json")).unwrap();
    fs::remove_file(f.root.join("tree/.pins/author.pub")).unwrap();
    let (_, view) = call(&ui, "/api/resources", None);
    assert_eq!(view["cells"], serde_json::json!([]));
    let (status, plan) = call(
        &ui,
        "/api/resources/plan",
        Some(
            serde_json::json!({"backend":"sites","resource":resource,"intent":{"transfer":{"to":"operator","visibility":"keep_current"}}}),
        ),
    );
    assert_eq!(status, 200, "{plan}");
    let (status, result) = call(
        &ui,
        &format!(
            "/api/resources/actions/{}/apply",
            plan["id"].as_str().unwrap()
        ),
        Some(serde_json::json!({"approval":plan["approval"]})),
    );
    assert_eq!(status, 200, "{result}");
    assert_eq!(
        f.registry.resources().unwrap().resources[0].owner,
        cell::resource::ResourceOwner::Operator
    );
}
#[test]
fn unavailable_or_redirected_backends_never_become_empty_success() {
    let f = Fixture::new();
    let ui = f.ui();
    let (_, view) = call(&ui, "/api/resources", None);
    let resource = view["backends"][0]["inventory"]["resources"][0]["id"].clone();
    let (_, plan) = call(
        &ui,
        "/api/resources/plan",
        Some(serde_json::json!({"backend":"sites","resource":resource,"intent":"unpublish"})),
    );
    let path = f.root.join("tree/.resources.json");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["backends"][0]["location"]["registry"] = serde_json::json!(f.root.join("missing"));
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let (status, view) = call(&ui, "/api/resources", None);
    assert_eq!(status, 200);
    assert!(view["backends"][0]["inventory"].is_null());
    assert!(view["backends"][0]["error"].is_string());
    assert_ne!(
        call(
            &ui,
            &format!(
                "/api/resources/actions/{}/apply",
                plan["id"].as_str().unwrap()
            ),
            Some(serde_json::json!({"approval":plan["approval"]}))
        )
        .0,
        200
    );
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"hello"
    );
    assert_ne!(call(&ui,"/api/resources/plan",Some(serde_json::json!({"backend":"sites","resource":resource,"intent":"unpublish","location":{"kind":"local","registry":"/tmp/attacker"}}))).0,200);
}

#[test]
fn external_registration_attachment_and_manual_ack_are_enforced_over_http() {
    let f = Fixture::new();
    let ui = f.ui();
    let (_, view) = call(&ui, "/api/resources", None);
    let cell = view["cells"][0]["identity"].clone();
    let body = serde_json::json!({"backend":"sites","registration":{"account":{"provider":"runpod","account":"account-a"},"kind":"compute","external_id":"pod-existing"}});
    let mut injected = body.clone();
    injected["registration"]["credential_value"] =
        serde_json::json!("not-an-accepted-secret-field");
    assert_ne!(
        call(&ui, "/api/resources/register-plan", Some(injected)).0,
        200
    );
    let (status, plan) = call(&ui, "/api/resources/register-plan", Some(body.clone()));
    assert_eq!(status, 200, "{plan}");
    let resource = plan["plan"]["resource"]["id"].clone();
    let path = format!(
        "/api/resources/actions/{}/apply",
        plan["id"].as_str().unwrap()
    );
    assert_ne!(
        call(
            &ui,
            &path,
            Some(serde_json::json!({"approval":plan["approval"]}))
        )
        .0,
        200
    );
    let approved = serde_json::json!({"approval":plan["approval"],"acknowledge_manual":true});
    assert_eq!(call(&ui, &path, Some(approved.clone())).0, 200);
    assert_eq!(call(&ui, &path, Some(approved)).0, 200);
    assert_ne!(call(&ui, "/api/resources/register-plan", Some(body)).0, 200);
    let attach = serde_json::json!({"backend":"sites","resource":resource,"intent":{"attach":{"cell":cell,"scope":"use","credential":null}}});
    let (status, plan) = call(&ui, "/api/resources/plan", Some(attach.clone()));
    assert_eq!(status, 200, "{plan}");
    let path = format!(
        "/api/resources/actions/{}/apply",
        plan["id"].as_str().unwrap()
    );
    let pin = f.root.join("tree/.pins/author.pub");
    let original = fs::read(&pin).unwrap();
    fs::write(&pin, Secret::generate().public().to_string()).unwrap();
    let approved = serde_json::json!({"approval":plan["approval"],"acknowledge_manual":true});
    assert_ne!(call(&ui, &path, Some(approved.clone())).0, 200);
    fs::write(pin, original).unwrap();
    assert_eq!(call(&ui, &path, Some(approved)).0, 200);
    let (_, view) = call(&ui, "/api/resources", None);
    let record = view["backends"][0]["inventory"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == resource)
        .unwrap();
    assert_eq!(record["owner"], "operator");
    assert_eq!(record["state"], "unknown");
    assert_eq!(record["attachments"].as_array().unwrap().len(), 1);
    let (status, plan) = call(
        &ui,
        "/api/resources/plan",
        Some(
            serde_json::json!({"backend":"sites","resource":resource,"intent":{"detach":{"cell":cell}}}),
        ),
    );
    assert_eq!(status, 200);
    let path = format!(
        "/api/resources/actions/{}/apply",
        plan["id"].as_str().unwrap()
    );
    assert_ne!(
        call(
            &ui,
            &path,
            Some(serde_json::json!({"approval":plan["approval"]}))
        )
        .0,
        200
    );
    assert_eq!(
        call(
            &ui,
            &path,
            Some(serde_json::json!({"approval":plan["approval"],"acknowledge_manual":true}))
        )
        .0,
        200
    );
}
