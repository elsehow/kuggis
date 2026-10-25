//! Health is independent of the chat socket so startup failures remain visible.
use super::*;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatState {
    Ready,
    Stopped,
    Starting,
    Failed,
    Unreachable,
}

#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatRecovery {
    Start,
    Repair,
    Restart,
}

#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub(crate) struct ChatHealth {
    state: ChatState,
    message: String,
    recovery: Option<ChatRecovery>,
}

#[derive(Deserialize)]
pub(crate) struct Recover {
    action: ChatRecovery,
}

fn unavailable(message: &str) -> ChatHealth {
    ChatHealth {
        state: ChatState::Unreachable,
        message: message.into(),
        recovery: None,
    }
}

// This trusted host snippet only enters the guest via the installed namespace
// helper. It never reads or executes files from the mutable rootfs on the host.
const HOST: &str = r#"import fcntl, sys
sys.path.insert(0, '/usr/local/lib/cell')
import environment
with open('/run/cell-chat-recovery.lock', 'a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    result = environment.guest('/usr/bin/python3', '-c', sys.stdin.read(), sys.argv[1], timeout=25)
    print(result.stdout)
"#;

async fn inspect(op: &Operator, name: &Name, action: &str) -> Result<ChatHealth> {
    deletion::ensure_available(&op.config.state, name)?;
    if op.tree()?.0.get(name).is_none() {
        return Err(Error(StatusCode::NOT_FOUND, "Cell not found".into()));
    }
    if op.config.sockets.is_some() {
        if action != "health" {
            return Err(Error(
                StatusCode::CONFLICT,
                "Start this local chat service from its terminal.".into(),
            ));
        }
        let request = Request::builder()
            .uri("/api/info")
            .body(Body::empty())
            .unwrap();
        return Ok(
            match tokio::time::timeout(Duration::from_secs(3), op.forward(name, request)).await {
                Ok(Ok(response)) if response.status().is_success() => ChatHealth {
                    state: ChatState::Ready,
                    message: "Chat is ready.".into(),
                    recovery: None,
                },
                _ => unavailable(
                    "Chat service unavailable. Start this local chat service from its terminal.",
                ),
            },
        );
    }
    let vm = op.vms.read().await.get(name).cloned().ok_or_else(|| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Cell has no VM; apply its declaration first".into(),
        )
    })?;
    let command = format!(
        "sudo -n /usr/bin/python3 -c '{}' {action}",
        HOST.replace('\'', "'\\''")
    );
    let mut child = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
        ])
        .arg(format!("{vm}.exe.xyz"))
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(fail)?;
    let work = async {
        let mut input = child.stdin.take().unwrap();
        input.write_all(include_bytes!("chat_health.py")).await?;
        drop(input);
        child.wait_with_output().await
    };
    Ok(match tokio::time::timeout(Duration::from_secs(40), work).await {
        Ok(Ok(output)) if output.status.success() => serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| unavailable("The cell returned an invalid chat health response.")),
        _ => unavailable("Cannot reach the cell's chat service manager. Check the VM connection and environment service, then retry."),
    })
}

pub(super) async fn get(
    State(op): State<Arc<Operator>>,
    Path(name): Path<Name>,
) -> Result<Json<ChatHealth>> {
    inspect(&op, &name, "health").await.map(Json)
}

pub(super) async fn recover(
    State(op): State<Arc<Operator>>,
    Path(name): Path<Name>,
    Json(body): Json<Recover>,
) -> Result<Json<ChatHealth>> {
    // Serialize with declaration changes and deletion; the host also serializes
    // simultaneous opens/recovery requests and rechecks health before starting.
    let _guard = op.applying.lock().await;
    let action = match body.action {
        ChatRecovery::Start => "start",
        ChatRecovery::Repair => "repair",
        ChatRecovery::Restart => "restart",
    };
    inspect(&op, &name, action).await.map(Json)
}
