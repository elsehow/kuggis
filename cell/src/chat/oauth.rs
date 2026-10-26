//! An isolated Pi process owns authentication. Rust relays only typed UI
//! prompts/status; tokens never cross this HTTP boundary or enter a chat.
use super::*;
use crate::provider::LoginMethod;
use crate::Provider;
use serde::{Deserialize, Serialize};
use std::{process::Stdio, time::Instant, sync::atomic::{AtomicBool, Ordering}};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::mpsc,
};

#[derive(Default, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    #[serde(default)]
    method: LoginMethod,
}

/// Callback coordinates extracted from Pi's authorization URL, never invented
/// by the gateway. Pi still validates the response and exchanges the code.
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
pub(crate) struct Callback {
    pub address: String,
    pub state: String,
}
#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelayState {
    #[default]
    Manual,
    Ready,
    Forwarded,
    Closed,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AuthNotice {
    Link {
        url: String,
        instructions: Option<String>,
        callback: Option<Callback>,
    },
    DeviceCode {
        url: String,
        code: String,
    },
    Info {
        message: String,
    },
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
pub(crate) struct AuthOption {
    value: String,
    label: String,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AuthPromptKind {
    Input {
        message: String,
        placeholder: Option<String>,
        secret: bool,
    },
    Select {
        message: String,
        options: Vec<AuthOption>,
    },
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
pub(crate) struct AuthPrompt {
    pub id: String,
    #[serde(flatten)]
    pub kind: AuthPromptKind,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum AuthState {
    Starting,
    Waiting {
        notice: Option<AuthNotice>,
        prompt: Option<AuthPrompt>,
    },
    Complete,
    Failed {
        message: String,
    },
    Cancelled,
}
impl AuthState {
    pub(crate) fn terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Failed { .. } | Self::Cancelled)
    }
    fn fail(&mut self, message: &str) {
        *self = Self::Failed {
            message: message.into(),
        };
    }
    fn waiting(&mut self) -> (&mut Option<AuthNotice>, &mut Option<AuthPrompt>) {
        if !matches!(self, Self::Waiting { .. }) {
            *self = Self::Waiting {
                notice: None,
                prompt: None,
            };
        }
        let Self::Waiting { notice, prompt } = self else {
            unreachable!()
        };
        (notice, prompt)
    }
}
#[derive(Deserialize, Serialize, ts_rs::TS)]
pub(crate) struct AuthFlow {
    pub id: RequestId,
    pub provider: Provider,
    #[serde(default)]
    pub relay: RelayState,
    #[serde(flatten)]
    pub status: AuthState,
}
#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthReply {
    id: String,
    value: String,
}

pub(super) struct Login {
    provider: Provider,
    started: Instant,
    state: Arc<Mutex<AuthState>>,
    settled: Arc<AtomicBool>,
    replies: mpsc::Sender<AuthReply>,
    cancel: watch::Sender<bool>,
}
impl Login {
    async fn view(&self, id: RequestId) -> AuthFlow {
        let mut status = self.state.lock().await.clone();
        // Terminal state is observable only after cleanup releases settings_lock.
        // This applies to every caller, including new logins and MCP updates.
        if status.terminal() && !self.settled.load(Ordering::Acquire) {
            status = AuthState::Starting;
        }
        AuthFlow {
            id,
            provider: self.provider,
            relay: RelayState::Manual,
            status,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Record {
    ExtensionUiRequest {
        id: String,
        #[serde(flatten)]
        request: UiRequest,
    },
    ExtensionError,
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum UiRequest {
    Input {
        title: String,
    },
    Notify {
        message: String,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Notice {
    Notice { notice: AuthNotice },
    Complete,
    Failed { message: String },
}

fn receive(state: &mut AuthState, record: Record) -> Result<(), &'static str> {
    match record {
        Record::ExtensionUiRequest {
            id,
            request: UiRequest::Input { title },
        } => {
            let kind = serde_json::from_str(&title)
                .map_err(|_| "Pi returned an invalid sign-in prompt")?;
            state.waiting().1.replace(AuthPrompt { id, kind });
        }
        Record::ExtensionUiRequest {
            request: UiRequest::Notify { message },
            ..
        } => {
            match serde_json::from_str(&message)
                .map_err(|_| "Pi returned invalid sign-in progress")?
            {
                Notice::Notice { notice } => {
                    state.waiting().0.replace(notice);
                }
                Notice::Complete => *state = AuthState::Complete,
                Notice::Failed { message } => state.fail(&message),
            }
        }
        Record::ExtensionError => return Err("Pi could not load its sign-in bridge"),
        _ => {}
    }
    Ok(())
}

fn validate_reply(state: &AuthState, reply: &AuthReply) -> Result<(), &'static str> {
    let AuthState::Waiting {
        prompt: Some(prompt),
        ..
    } = state
    else {
        return Err("No sign-in prompt is waiting");
    };
    if reply.id != prompt.id {
        return Err("This sign-in prompt has changed");
    }
    if reply.value.trim().is_empty()
        || reply.value.len() > 8192
        || reply.value.chars().any(char::is_control)
    {
        return Err("Enter a valid sign-in response");
    }
    match &prompt.kind {
        AuthPromptKind::Select { options, .. }
            if !options.iter().any(|o| o.value == reply.value) =>
        {
            Err("Choose one of the offered options")
        }
        AuthPromptKind::Input { secret: true, .. } if reply.value.trim().starts_with('!') => {
            Err("Enter a literal API key")
        }
        _ => Ok(()),
    }
}

fn launch(config: &Config, provider: Provider, method: &str) -> ApiResult<tokio::process::Child> {
    let directory = config.state.join("auth-bridge");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let extension = directory.join("auth.ts");
    fs::write(&extension, include_str!("../pi/auth.ts"))?;
    let pi_provider = if method == "oauth" {
        provider
            .oauth_id()
            .ok_or_else(|| bad("This provider uses an API key"))?
    } else {
        provider.as_str()
    };
    tokio::process::Command::new(&config.pi)
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--no-mcp",
            "--no-tools",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--offline",
            "--extension",
        ])
        .arg(&extension)
        .current_dir(&directory)
        .env(
            "PI_CODING_AGENT_DIR",
            config.agent_dir().ok_or_else(|| bad("Cell required"))?,
        )
        .env("PI_TELEMETRY", "0")
        .env("KUGGIS_AUTH_PROVIDER", pi_provider)
        .env("KUGGIS_AUTH_METHOD", method)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(Into::into)
}

async fn run(
    mut child: tokio::process::Child,
    state: Arc<Mutex<AuthState>>,
    mut replies: mpsc::Receiver<AuthReply>,
    mut cancelled: watch::Receiver<bool>,
    mut stopping: watch::Receiver<bool>,
) {
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    // Keep partial JSONL reads alive while a browser reply arrives. Cancelling
    // record() inside select would otherwise discard an unfinished record.
    let (records, mut incoming) = mpsc::channel(8);
    let reader = tokio::spawn(async move {
        loop {
            let line = super::super::pi::record(&mut output).await;
            let ended = !matches!(&line, Ok(Some(_)));
            if records.send(line).await.is_err() || ended {
                break;
            }
        }
    });
    let deadline = tokio::time::sleep(Duration::from_secs(600));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => { state.lock().await.fail("Sign-in expired; try again"); break; }
            _ = cancelled.changed() => { *state.lock().await = AuthState::Cancelled; break; }
            _ = stopping.changed() => { *state.lock().await = AuthState::Cancelled; break; }
            reply = replies.recv() => {
                let Some(reply) = reply else { *state.lock().await = AuthState::Cancelled; break; };
                let mut bytes = serde_json::to_vec(&json!({"type":"extension_ui_response", "id":reply.id, "value":reply.value})).unwrap();
                bytes.push(b'\n');
                if input.write_all(&bytes).await.is_err() { state.lock().await.fail("Pi sign-in closed"); break; }
            }
            line = incoming.recv() => {
                let mut state = state.lock().await;
                let result = match line {
                    Some(Ok(Some(line))) if line.len() <= 65536 => serde_json::from_str(&line).map_err(|_| "Invalid Pi sign-in response").and_then(|record| receive(&mut state, record)),
                    _ => Err("Pi sign-in closed before completing"),
                };
                if let Err(message) = result { state.fail(message); }
                if state.terminal() { break; }
            }
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    reader.abort();
}

fn granted(store: &Store, provider: Provider) -> ApiResult<()> {
    settings::directory(store)?;
    if !store
        .config
        .declaration()
        .is_some_and(|c| c.providers().contains(&provider))
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Provider is not granted to this cell".into(),
        ));
    }
    Ok(())
}

pub(super) async fn start(
    State(store): State<Arc<Store>>,
    Path(provider): Path<Provider>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<AuthFlow>> {
    granted(&store, provider)?;
    if !provider.login_methods().contains(&body.method) {
        return Err(bad("This provider does not support that sign-in method"));
    }
    let guard = store
        .settings_lock
        .clone()
        .try_lock_owned()
        .map_err(|_| bad("Finish the current settings change or sign-in first"))?;
    settings::stop_idle(&store).await?;
    let mut logins = store.logins.lock().await;
    logins.retain(|_, login| login.started.elapsed() < Duration::from_secs(600));
    if logins.len() >= 32 {
        return Err(bad("Too many sign-in attempts; wait before trying again"));
    }
    let child = launch(&store.config, provider, body.method.as_str())?;
    let (replies, rx) = mpsc::channel(4);
    let (cancel, cancelled) = watch::channel(false);
    let state = Arc::new(Mutex::new(AuthState::Starting));
    let id = RequestId::generate();
    let settled = Arc::new(AtomicBool::new(false));
    let login = Login {
        provider,
        started: Instant::now(),
        state: state.clone(),
        settled: settled.clone(),
        replies,
        cancel,
    };
    let view = login.view(id).await;
    logins.insert(id, login);
    let stopping = store.stopping.subscribe();
    drop(logins);
    tokio::spawn(async move {
        run(child, state, rx, cancelled, stopping).await;
        // Browsing a chat during sign-in may have opened an idle Pi process.
        // Its next use must see the new credential/catalog snapshot too.
        let _ = settings::stop_idle(&store).await;
        drop(guard);
        settled.store(true, Ordering::Release);
    });
    Ok(Json(view))
}

pub(super) async fn get(
    State(store): State<Arc<Store>>,
    Path((provider, id)): Path<(Provider, RequestId)>,
) -> ApiResult<Json<AuthFlow>> {
    let logins = store.logins.lock().await;
    let login = logins
        .get(&id)
        .filter(|l| l.provider == provider)
        .ok_or_else(|| bad("Sign-in expired; start again"))?;
    Ok(Json(login.view(id).await))
}

pub(super) async fn respond(
    State(store): State<Arc<Store>>,
    Path((provider, id)): Path<(Provider, RequestId)>,
    Json(reply): Json<AuthReply>,
) -> ApiResult<StatusCode> {
    let logins = store.logins.lock().await;
    let login = logins
        .get(&id)
        .filter(|l| l.provider == provider)
        .ok_or_else(|| bad("Sign-in expired; start again"))?;
    let mut state = login.state.lock().await;
    validate_reply(&state, &reply).map_err(bad)?;
    login
        .replies
        .try_send(reply)
        .map_err(|_| bad("Pi sign-in is no longer waiting"))?;
    state.waiting().1.take();
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn cancel(
    State(store): State<Arc<Store>>,
    Path((provider, id)): Path<(Provider, RequestId)>,
) -> ApiResult<StatusCode> {
    let logins = store.logins.lock().await;
    let login = logins
        .get(&id)
        .filter(|l| l.provider == provider)
        .ok_or_else(|| bad("Sign-in expired"))?;
    if !login.state.lock().await.terminal() {
        let _ = login.cancel.send(true);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn logout(
    State(store): State<Arc<Store>>,
    Path(provider): Path<Provider>,
) -> ApiResult<StatusCode> {
    let _guard = settings::lock(&store)?;
    settings::directory(&store)?;
    settings::stop_idle(&store).await?;
    let child = launch(&store.config, provider, "logout")?;
    let state = Arc::new(Mutex::new(AuthState::Starting));
    let (_replies, rx) = mpsc::channel(1);
    let (_cancel, cancelled) = watch::channel(false);
    tokio::time::timeout(
        Duration::from_secs(30),
        run(
            child,
            state.clone(),
            rx,
            cancelled,
            store.stopping.subscribe(),
        ),
    )
    .await
    .map_err(|_| bad("Pi sign-out timed out; check settings"))?;
    if matches!(*state.lock().await, AuthState::Complete) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(bad("Pi could not sign out; try again"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn terminal_login_is_hidden_until_settings_guard_is_released() {
        for terminal in [AuthState::Complete, AuthState::Cancelled,
                         AuthState::Failed { message: "fixture".into() }] {
            let lock = Arc::new(Mutex::new(()));
            let guard = lock.clone().lock_owned().await;
            let (replies, _rx) = mpsc::channel(1);
            let (cancel, _cancelled) = watch::channel(false);
            let settled = Arc::new(AtomicBool::new(false));
            let login = Login {
                provider: Provider::Anthropic,
                started: Instant::now(),
                state: Arc::new(Mutex::new(terminal)),
                settled: settled.clone(), replies, cancel,
            };
            let id = RequestId::generate();
            assert!(!login.view(id).await.status.terminal());
            assert!(lock.try_lock().is_err());
            drop(guard);
            settled.store(true, Ordering::Release);
            assert!(login.view(id).await.status.terminal());
            assert!(lock.try_lock().is_ok());
        }
    }

    #[tokio::test]
    async fn queued_prompt_and_cancel_finish_without_reopening_terminal_state() {
        let mut child = tokio::process::Command::new("python3");
        child.args(["-c", "import json,time; print(json.dumps({'type':'extension_ui_request','id':'queued','method':'input','title':json.dumps({'kind':'input','message':'fixture','secret':False})}),flush=True); time.sleep(30)"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).kill_on_drop(true);
        let child = child.spawn().unwrap();
        let state = Arc::new(Mutex::new(AuthState::Starting));
        let (_replies, rx) = mpsc::channel(1);
        let (cancel, cancelled) = watch::channel(false);
        let (_stop, stopping) = watch::channel(false);
        cancel.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), run(child, state.clone(), rx, cancelled, stopping))
            .await.unwrap();
        assert!(matches!(*state.lock().await, AuthState::Cancelled));
    }

    #[test]
    fn auth_ui_rejects_stale_prompts_and_nonliteral_keys() {
        let mut state = AuthState::Starting;
        receive(&mut state, serde_json::from_value(json!({"type":"extension_ui_request","id":"prompt-1","method":"input","title":r#"{"kind":"input","message":"Key","secret":true}"#})).unwrap()).unwrap();
        assert!(validate_reply(
            &state,
            &AuthReply {
                id: "old".into(),
                value: "key".into()
            }
        )
        .is_err());
        assert!(validate_reply(
            &state,
            &AuthReply {
                id: "prompt-1".into(),
                value: "!command".into()
            }
        )
        .is_err());
        assert!(validate_reply(
            &state,
            &AuthReply {
                id: "prompt-1".into(),
                value: "fixture".into()
            }
        )
        .is_ok());
        receive(&mut state, serde_json::from_value(json!({"type":"extension_ui_request","id":"notice","method":"notify","message":r#"{"state":"complete"}"#})).unwrap()).unwrap();
        assert!(state.terminal());
        assert!(validate_reply(
            &state,
            &AuthReply {
                id: "prompt-1".into(),
                value: "key".into()
            }
        )
        .is_err());
    }
}
