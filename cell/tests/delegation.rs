//! Real door -> inbox -> chat daemon -> deterministic Pi -> outbox -> signed
//! log -> parent verification. Only the platform-authenticated HTTP hop and
//! model are simulated; no cloud credentials or real model calls are used.
use cell::context::{verified_status, Context};
use cell::door::Door;
use cell::{Cell, Grant, Provider, RequestId, Secret, SessionId, Tree};
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
    fn start(cell: Cell, context: &Context) -> Self {
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
            secret: Secret::generate(),
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
            .brief(&self.cell.id, session, request, text.into())
            .unwrap();
        let body = serde_json::to_string(&said).unwrap();
        let response = self.exchange(&format!("POST /say HTTP/1.1\r\nHost: door\r\nX-Exedev-Source-Vm: parent-vm\r\nContent-Length: {}\r\n\r\n{}", body.len(), body));
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    }
    fn done(&self, parent: &Context, session: SessionId, since: u64) -> (String, u64) {
        let child = parent.child(&self.cell.id).unwrap();
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
fn parent_drives_two_children_retries_and_continues_after_rename() {
    let parent = Cell::new(
        "fri".parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let airo = Cell::new(
        "airo".parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let prod = Cell::new(
        "airo-prod".parse().unwrap(),
        [Grant::Provider(Provider::Anthropic)],
    );
    let tree = Tree::new([
        (parent.clone(), None),
        (airo.clone(), Some(parent.id.clone())),
        (prod.clone(), Some(parent.id.clone())),
    ])
    .unwrap();
    let pins = std::env::temp_dir().join(format!("cell-delegation-pins-{}", RequestId::generate()));
    let mut context = Context::for_cell(&tree, &parent.id, &pins).unwrap();
    let workers: Vec<_> = [airo, prod]
        .into_iter()
        .map(|cell| {
            Worker::start(
                cell.clone(),
                &Context::for_cell(&tree, &cell.id, &pins).unwrap(),
            )
        })
        .collect();
    for worker in &workers {
        context
            .children
            .iter_mut()
            .find(|c| c.cell.id == worker.cell.id)
            .unwrap()
            .key = Some(worker.secret.public().to_string());
    }
    let jobs: Vec<_> = workers
        .iter()
        .map(|worker| {
            let session = SessionId::generate();
            let request = RequestId::generate();
            worker.brief(&context, session, request, "report task result");
            worker.brief(&context, session, request, "report task result"); // lost HTTP acknowledgement
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
            .contains(worker.cell.id.as_str()));
        assert!(users[0]["content"]
            .as_str()
            .unwrap()
            .contains("not the checkout directory"));
        // Refresh the published context without replacing the daemon or session.
        let mut own = Context::for_cell(&tree, &worker.cell.id, &pins).unwrap();
        own.cell.name = "renamed-child".parse().unwrap();
        own.parent.as_mut().unwrap().name = "renamed-parent".parse().unwrap();
        fs::write(
            worker.dir.join("context.json"),
            serde_json::to_vec(&own).unwrap(),
        )
        .unwrap();
        worker.brief(&context, session, RequestId::generate(), "continue task");
        worker.done(&context, session, cursor);
        let users = worker.user_messages(session);
        assert_eq!(users.len(), 2);
        assert!(users[1]["content"]
            .as_str()
            .unwrap()
            .contains("renamed-parent"));
        assert!(users[1]["content"]
            .as_str()
            .unwrap()
            .contains("renamed-child"));
    }
}
