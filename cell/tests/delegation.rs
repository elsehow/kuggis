//! Real door -> inbox -> chat daemon -> deterministic Pi -> outbox -> signed
//! log -> parent verification. Only the platform-authenticated HTTP hop and
//! model are simulated; no cloud credentials or real model calls are used.
use cell::context::{verified_status, Context};
use cell::door::Door;
use cell::{Cell, CellId, Grant, Provider, RequestId, Secret, SessionId};
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct Worker {
    dir: PathBuf,
    process: Child,
    cell: Cell,
    id: CellId,
    secret: Secret,
    addr: SocketAddr,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for child");
        thread::sleep(Duration::from_millis(30));
    }
}
impl Worker {
    fn start(cell: Cell, context: &Context, secret: Secret) -> Self {
        let dir = std::env::temp_dir().join(format!("cell-delegation-{}", RequestId::generate()));
        for path in ["inbox", "outbox", "said", "auth", "airo-checkout"] {
            fs::create_dir_all(dir.join(path)).unwrap();
        }
        fs::write(dir.join("cell.json"), cell.canonical()).unwrap();
        fs::write(
            dir.join("context.json"),
            serde_json::to_vec(context).unwrap(),
        )
        .unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/tests/fake-pi.mjs");
        let stderr = fs::File::create(dir.join("stderr")).unwrap();
        let process = Command::new(env!("CARGO_BIN_EXE_cell"))
            .args(["chat", "--cell"])
            .arg(dir.join("cell.json"))
            .arg("--workspace")
            .arg(dir.join("airo-checkout"))
            .arg("--state")
            .arg(dir.join("state"))
            .arg("--socket")
            .arg(dir.join("chat.sock"))
            .arg("--context")
            .arg(dir.join("context.json"))
            .arg("--agent-dir")
            .arg(dir.join("auth"))
            .arg("--inbox")
            .arg(dir.join("inbox"))
            .arg("--outbox")
            .arg(dir.join("outbox"))
            .arg("--shared")
            .arg(dir.join("shared"))
            .arg("--pi")
            .arg(fixture)
            .env("KUGGIS_FIXTURE_PROVIDER", "anthropic")
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let door = Arc::new(Door::new(&cell, dir.join("said"), dir.join("inbox")));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || door.serve(listener));
        let worker = Self {
            dir,
            process,
            cell,
            id: context.cell.id.clone(),
            secret,
            addr,
        };
        until(|| worker.dir.join("chat.sock").exists());
        worker
    }
    fn exchange(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    fn brief(&self, parent: &Context, session: SessionId, request: RequestId, text: &str) {
        let said = parent
            .brief(&self.id, session, request, text.into())
            .unwrap();
        let body = serde_json::to_string(&said).unwrap();
        let response = self.exchange(&format!("POST /say HTTP/1.1\r\nHost: door\r\nX-Exedev-Source-Vm: parent-vm\r\nContent-Length: {}\r\n\r\n{}", body.len(), body));
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    }
    fn done(&self, parent: &Context, session: SessionId, since: u64) -> (String, u64) {
        let child = parent.child(&self.id).unwrap();
        let mut answer = None;
        until(|| {
            cell::ship::ship(
                &self.cell,
                &self.secret,
                &self.dir.join("outbox"),
                &self.dir.join("said"),
            )
            .unwrap();
            let response = self.exchange(&format!(
                "GET /said?since={since} HTTP/1.1\r\nHost: door\r\n\r\n"
            ));
            assert!(response.starts_with("HTTP/1.1 200"));
            let (_, body) = response.split_once("\r\n\r\n").unwrap();
            let verified = verified_status(child, &session, since, body).unwrap();
            if let Some(done) = verified
                .iter()
                .find(|v| v["said"]["session"]["state"] == "done")
            {
                answer = Some((
                    done["said"]["session"]["text"].as_str().unwrap().to_owned(),
                    verified.last().unwrap()["next_since"].as_u64().unwrap(),
                ));
            }
            answer.is_some()
        });
        answer.unwrap()
    }
    fn user_messages(&self, session: SessionId) -> Vec<serde_json::Value> {
        let messages: Vec<serde_json::Value> = serde_json::from_slice(
            &fs::read(self.dir.join(format!("state/pi/{session}.fixture.json"))).unwrap(),
        )
        .unwrap();
        messages
            .into_iter()
            .filter(|m| m["role"] == "user")
            .collect()
    }
}

#[test]
fn migrated_parent_drives_legacy_and_native_children_and_continues_after_rename() {
    let parent = Cell::new(
        "fri".parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let airo = Cell::new(
        "airo".parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let native_id = CellId::generate();
    let prod = Cell::new(
        native_id.to_string().parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let root = std::env::temp_dir().join(format!("cell-delegation-tree-{}", RequestId::generate()));
    let pins = root.join(".pins");
    fs::create_dir_all(&pins).unwrap();
    fs::create_dir_all(root.join("fri")).unwrap();
    let secrets: std::collections::BTreeMap<_, _> = [&parent, &airo, &prod]
        .into_iter()
        .map(|c| (c.name.clone(), Secret::generate()))
        .collect();
    for c in [&parent, &airo, &prod] {
        let path = if c == &parent {
            root.join("fri.json")
        } else {
            root.join("fri").join(format!("{}.json", c.name))
        };
        fs::write(path, c.canonical()).unwrap();
        fs::write(
            pins.join(format!("{}.pub", c.name)),
            secrets[&c.name].public().to_string(),
        )
        .unwrap();
    }
    // Native cells are enrolled under their original UUID; legacy cells receive
    // separate UUIDs through the real migration planner, preserving wire names.
    cell::identity::Registry {
        version: 1,
        cells: vec![cell::identity::Identity {
            id: native_id.clone(),
            binding: prod.name.clone(),
            name: "airo-prod".parse().unwrap(),
            key: secrets[&prod.name].public(),
            vm: None,
        }],
    }
    .save(&root)
    .unwrap();
    let observed = cell::identity::observe(&root, None).unwrap();
    let migration = cell::identity::Migration::plan(&root, &observed).unwrap();
    migration
        .apply(&root, &observed, migration.approval)
        .unwrap();
    let tree = cell::tree::load(&root).unwrap().0;
    let context = Context::for_cell(&tree, &parent.name, &pins).unwrap();
    assert_ne!(context.cell.id.as_str(), parent.name.as_str());
    assert_eq!(context.child(&native_id).unwrap().cell.binding, prod.name);
    let mut secrets = secrets;
    let workers: Vec<_> = [airo, prod]
        .into_iter()
        .map(|c| {
            Worker::start(
                c.clone(),
                &Context::for_cell(&tree, &c.name, &pins).unwrap(),
                secrets.remove(&c.name).unwrap(),
            )
        })
        .collect();
    let jobs: Vec<_> = workers
        .iter()
        .map(|worker| {
            let session = SessionId::generate();
            let request = RequestId::generate();
            let brief = context
                .brief(&worker.id, session, request, "report task result".into())
                .unwrap();
            assert!(
                matches!(brief, cell::Said::Brief { session: s, .. } if s.cell == worker.cell.name)
            );
            worker.brief(&context, session, request, "report task result");
            worker.brief(&context, session, request, "report task result");
            (session, request)
        })
        .collect();
    for (worker, (session, _)) in workers.iter().zip(jobs) {
        let (answer, cursor) = worker.done(&context, session, 0);
        assert!(answer.contains("can be resumed"));
        until(|| fs::read_to_string(worker.dir.join("state/inbox-seen")).is_ok_and(|s| s == "2"));
        let users = worker.user_messages(session);
        assert_eq!(users.len(), 1, "same request must dispatch once");
        assert!(users[0]["content"]
            .as_str()
            .unwrap()
            .contains(worker.id.as_str()));
        assert!(users[0]["content"]
            .as_str()
            .unwrap()
            .contains("not the checkout directory"));
        let signed_before: Vec<_> = fs::read_dir(worker.dir.join("said"))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (path.clone(), fs::read(path).unwrap())
            })
            .collect();
        let mut registry = cell::identity::Registry::load(&root).unwrap();
        registry
            .rename(&root, &context.cell.id, "renamed-parent".parse().unwrap())
            .unwrap();
        let label = format!("renamed-{}", worker.id);
        registry
            .rename(&root, &worker.id, label.parse().unwrap())
            .unwrap();
        let own = Context::for_cell(&tree, &worker.cell.name, &pins).unwrap();
        fs::write(
            worker.dir.join("context.json"),
            serde_json::to_vec(&own).unwrap(),
        )
        .unwrap();
        let refreshed = Context::for_cell(&tree, &parent.name, &pins).unwrap();
        assert_eq!(
            refreshed.child(&worker.id).unwrap().cell.binding,
            worker.cell.name
        );
        assert_eq!(
            refreshed.child(&worker.id).unwrap().key,
            context.child(&worker.id).unwrap().key
        );
        worker.brief(&refreshed, session, RequestId::generate(), "continue task");
        worker.done(&refreshed, session, cursor);
        let users = worker.user_messages(session);
        assert_eq!(users.len(), 2);
        assert!(users[1]["content"]
            .as_str()
            .unwrap()
            .contains("renamed-parent"));
        assert!(users[1]["content"].as_str().unwrap().contains(&label));
        for (path, bytes) in signed_before {
            assert_eq!(
                fs::read(path).unwrap(),
                bytes,
                "rename never rewrites signed history"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}
