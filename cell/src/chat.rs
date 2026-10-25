//! Cell or explicit local workspace: assistant-ui HTTP/SSE ↔ durable Pi RPC
//! sessions. Cell runs publish their status through the signed-log outbox.

use crate::pi::protocol::{Command as PiCommand, ModelCatalog, ModelSelection, RuntimeState};
use crate::pi::{self, Rpc};
use crate::request::{Claim, Receipt, RequestJournal, SubmissionOutcome};
use crate::session::StreamingBehavior;
use crate::{RequestId, Session, SessionAction, SessionCommand, SessionId};
mod config;
pub use config::{Config, ExecutionMode};
pub(crate) mod oauth;
mod provider_check;
pub(crate) mod settings;
pub(crate) mod types;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap, convert::Infallible, fs, os::unix::fs::PermissionsExt, path::PathBuf,
    sync::Arc, time::Duration,
};
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::services::ServeDir;
use types::*;

struct Store {
    config: Config,
    sessions: Mutex<BTreeMap<SessionId, Arc<LiveSession>>>,
    authority: String,
    stopping: watch::Sender<bool>,
    requests: RequestJournal,
    settings_lock: Arc<Mutex<()>>,
    logins: Mutex<BTreeMap<RequestId, oauth::Login>>,
    provider_observations:
        Arc<Mutex<BTreeMap<crate::Provider, crate::provider::ProviderObservations>>>,
}

struct LiveSession {
    view: Mutex<View>,
    rpc: Mutex<Option<Arc<Rpc>>>,
    events: broadcast::Sender<Value>,
}

struct View {
    metadata: ThreadMetadata,
    messages: Vec<Value>,
    pending: BTreeMap<String, HostUiRequest>,
    streaming: Option<usize>,
    seq: u64,
    turn: u64,
    error: Option<String>,
    readiness: Option<Readiness>,
    generation: u64,
}

impl View {
    fn new(metadata: ThreadMetadata) -> Self {
        Self {
            metadata,
            messages: vec![],
            pending: BTreeMap::new(),
            streaming: None,
            seq: 0,
            turn: 0,
            error: None,
            readiness: None,
            generation: 0,
        }
    }
    fn snapshot(&self) -> ThreadSnapshot {
        ThreadSnapshot {
            metadata: self.metadata.clone(),
            messages: self.messages.clone(),
            host_ui_requests: self.pending.values().cloned().collect(),
            seq: self.seq,
            last_error: self.error.clone(),
            readiness: self.readiness.clone(),
        }
    }
    fn frame(&self) -> Value {
        json!({"threadId":self.metadata.session.id(), "seq":self.seq,
            "type":"snapshot", "snapshot":self.snapshot()})
    }
    fn stamp(&mut self, mut event: Value) -> Value {
        self.seq += 1;
        event["threadId"] = json!(self.metadata.session.id());
        event["seq"] = json!(self.seq);
        event
    }
    fn apply(&mut self, mut event: Value) -> Option<Value> {
        // Pi 1.3 records internal system-prompt updates alongside messages.
        // react-pi renders unknown roles as assistant messages; keep these
        // runtime records out of the conversation presented to the browser.
        if event["message"]["role"] == "system" {
            return None;
        }
        let wire_event = match serde_json::from_value::<PiEvent>(event.clone()) {
            Ok(parsed) => parsed,
            Err(error) => {
                let message = format!("Invalid known Pi event: {error}");
                self.error = Some(message.clone());
                self.metadata.status = RunStatus::Failed;
                self.pending.clear();
                event = json!({"type":"error", "error":message});
                PiEvent::Error { error: message }
            }
        };
        let kind = wire_event.kind().to_owned();
        match kind.as_str() {
            "agent_start" => {
                self.metadata.status = RunStatus::Running;
                self.error = None;
            }
            // Pi can continue with queued work after agent_end. Only settled is final.
            "agent_end" => {
                event["willRetry"] = json!(true);
            }
            "agent_settled" => {
                self.metadata.status = if self.error.is_some() {
                    RunStatus::Failed
                } else {
                    RunStatus::Idle
                };
                self.metadata.compaction_active = false;
                self.metadata.retry_active = false;
                self.streaming = None;
            }
            "turn_start" => {
                self.turn += 1;
                event["turnIndex"] = json!(self.turn);
            }
            "turn_end" => {
                event["turnIndex"] = json!(self.turn);
            }
            "message_start" => {
                self.messages.push(event["message"].clone());
                if matches!(&wire_event, PiEvent::MessageStart { message } if message.role == "assistant")
                {
                    self.streaming = Some(self.messages.len() - 1);
                }
            }
            "message_update" => {
                if let Some(index) = self.streaming {
                    if let PiEvent::MessageUpdate {
                        assistant_message_event,
                    } = &wire_event
                    {
                        pi::apply_delta_typed(
                            &mut self.messages[index],
                            &event,
                            assistant_message_event,
                        );
                    }
                    event["message"] = self.messages[index].clone();
                } else {
                    return None;
                }
            }
            "message_end" => {
                if matches!(&wire_event, PiEvent::MessageEnd { message } if message.role == "assistant")
                {
                    if let Some(index) = self.streaming.take() {
                        self.messages[index] = event["message"].clone();
                    }
                    if matches!(&wire_event, PiEvent::MessageEnd { message } if message.stop_reason.as_deref() == Some("error"))
                    {
                        self.error = Some(if let PiEvent::MessageEnd { message } = &wire_event {
                            message
                                .error_message
                                .clone()
                                .unwrap_or_else(|| "Model request failed".into())
                        } else {
                            "Model request failed".into()
                        });
                    }
                } else if let Some(last) = self.messages.last_mut() {
                    *last = event["message"].clone();
                }
                self.metadata.message_count = self.messages.len();
            }
            "queue_update" => {
                let mut queue = vec![];
                if let PiEvent::QueueUpdate {
                    steering,
                    follow_up,
                } = &wire_event
                {
                    for (field, mode, items) in [
                        ("steering", StreamingBehavior::Steer, steering),
                        ("followUp", StreamingBehavior::FollowUp, follow_up),
                    ] {
                        for (index, content) in items.iter().enumerate() {
                            queue.push(QueuedMessage {
                                id: format!("{field}:{index}"),
                                mode,
                                content: content.clone(),
                            });
                        }
                    }
                }
                self.metadata.queued_messages = queue;
            }
            "session_info_changed" => {
                if let PiEvent::SessionInfoChanged { name } = &wire_event {
                    self.metadata.session.title = name.clone();
                }
            }
            "thinking_level_changed" => {
                if let PiEvent::ThinkingLevelChanged { level } = &wire_event {
                    self.metadata.config.thinking_level = Some(*level);
                }
            }
            "compaction_start" | "auto_compaction_start" => {
                self.metadata.compaction_active = true;
                event["type"] = json!("compaction_start");
            }
            "compaction_end" | "auto_compaction_end" => {
                self.metadata.compaction_active = false;
                event["type"] = json!("compaction_end");
            }
            "auto_retry_start" => {
                self.metadata.retry_active = true;
                self.metadata.retry_attempt =
                    if let PiEvent::AutoRetryStart { attempt } = &wire_event {
                        Some(*attempt)
                    } else {
                        None
                    };
            }
            "auto_retry_end" => self.metadata.retry_active = false,
            "extension_ui_request" => {
                let kind = if let PiEvent::ExtensionUiRequest { method, .. } = &wire_event {
                    method.as_str()
                } else {
                    ""
                };
                if !["confirm", "select", "input", "editor"].contains(&kind) {
                    return None;
                }
                let mut request = event.clone();
                request["kind"] = json!(kind);
                if let PiEvent::ExtensionUiRequest { id, .. } = &wire_event {
                    request["id"] = json!(id);
                }
                if let Some(timeout) = event.get("timeout") {
                    request["timeoutMs"] = timeout.clone();
                }
                let request: HostUiRequest =
                    match serde_json::from_value(request) {
                        Ok(request) => request,
                        Err(e) => return self.apply(
                            json!({"type":"error", "error":format!("Invalid Pi UI request: {e}")}),
                        ),
                    };
                self.pending.insert(request.id.clone(), request.clone());
                event = json!({"type":"extension_ui_request", "request":request});
            }
            "extension_ui_resolved" => {
                if let PiEvent::ExtensionUiResolved { request_id } = &wire_event {
                    self.pending.remove(request_id);
                }
            }
            "error" => {
                self.error = Some(if let PiEvent::Error { error } = &wire_event {
                    error.clone()
                } else {
                    "Pi failed".into()
                });
                self.metadata.status = RunStatus::Failed;
                self.pending.clear();
            }
            _ => {}
        }
        let settled = event["type"] == "agent_settled";
        let event = self.stamp(event);
        // Reconcile run status and any failed final message in one authoritative update.
        Some(if settled { self.frame() } else { event })
    }
}

type ApiResult<T> = Result<T, ApiError>;
struct ApiError {
    status: StatusCode,
    message: String,
    submission: Option<pi::CallError>,
}
fn error(status: StatusCode, message: String) -> ApiError {
    ApiError {
        status,
        message,
        submission: None,
    }
}
impl From<pi::CallError> for ApiError {
    fn from(failure: pi::CallError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: failure.to_string(),
            submission: Some(failure),
        }
    }
}
impl From<String> for ApiError {
    fn from(e: String) -> Self {
        error(StatusCode::BAD_GATEWAY, e)
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":self.message, "submission":self.submission})),
        )
            .into_response()
    }
}
fn bad(message: &str) -> ApiError {
    error(StatusCode::BAD_REQUEST, message.into())
}

fn save_session(config: &Config, session: &Session) -> ApiResult<()> {
    session
        .reference()
        .check_owner(&config.cell())
        .map_err(|e| bad(&e))?;
    let id = session.id();
    let path = config.state.join("threads").join(format!("{id}.json"));
    let temp = path.with_extension("tmp");
    fs::write(&temp, serde_json::to_vec_pretty(session).unwrap())?;
    fs::rename(temp, path)?;
    Ok(())
}

impl Store {
    async fn find(&self, id: &SessionId) -> ApiResult<Arc<LiveSession>> {
        self.sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "Chat not found".into()))
    }
    async fn open(&self, id: &SessionId) -> ApiResult<(Arc<LiveSession>, Arc<Rpc>)> {
        let session = self.find(id).await?;
        let mut slot = session.rpc.lock().await;
        if let Some(rpc) = slot.as_ref().filter(|p| p.alive()) {
            return Ok((session.clone(), rpc.clone()));
        }
        let generation = {
            let mut view = session.view.lock().await;
            view.generation += 1;
            view.generation
        };
        let (rpc, mut events) = Rpc::spawn(
            &self.config.pi,
            &self.config.workspace,
            &self.config.state.join("pi"),
            &id.to_string(),
            self.config.agent_dir(),
        )?;
        let target = Arc::downgrade(&session);
        let config = self.config.clone();
        let provider_observations = self.provider_observations.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(session) = target.upgrade() else {
                    break;
                };
                let timeout = if event["type"] == "extension_ui_request" {
                    event["timeout"]
                        .as_u64()
                        .zip(event["id"].as_str().map(str::to_owned))
                } else {
                    None
                };
                let mut view = session.view.lock().await;
                if view.generation != generation {
                    break;
                }
                if let Err(e) =
                    settings::record_request(&config, &provider_observations, &event).await
                {
                    eprintln!("provider observation: {}", e.message);
                }
                let state = match event["type"].as_str() {
                    Some("agent_start") => Some(crate::State::Working),
                    Some("agent_settled") => Some(if view.error.is_some() {
                        crate::State::Blocked
                    } else {
                        crate::State::Done
                    }),
                    Some("error") if event["error"] != "Pi stopped" => Some(crate::State::Blocked),
                    _ => None,
                };
                if let Some(state) = state {
                    if let Err(e) = speak(
                        &config,
                        view.metadata.session.reference(),
                        state,
                        if state == crate::State::Blocked {
                            event["error"]
                                .as_str()
                                .or(view.error.as_deref())
                                .map(str::to_owned)
                        } else {
                            None
                        },
                    ) {
                        eprintln!("session statement: {e}");
                    }
                }
                let renamed = event["type"] == "session_info_changed";
                if let Some(event) = view.apply(event) {
                    let _ = session.events.send(event);
                }
                if renamed {
                    if let Err(e) = save_session(&config, &view.metadata.session) {
                        eprintln!("save session: {}", e.message);
                    }
                }
                drop(view);
                if let Some((ms, id)) = timeout {
                    let target = Arc::downgrade(&session);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        if let Some(session) = target.upgrade() {
                            let mut view = session.view.lock().await;
                            if view.generation == generation && view.pending.contains_key(&id) {
                                if let Some(event) = view
                                    .apply(json!({"type":"extension_ui_resolved","requestId":id}))
                                {
                                    let _ = session.events.send(event);
                                }
                            }
                        }
                    });
                }
            }
        });
        let initial = async {
            let state = rpc.call::<RuntimeState>(PiCommand::GetState).await?;
            let entries = rpc.call::<Value>(PiCommand::GetEntries).await?;
            let messages = pi::transcript(&entries)?;
            Ok::<_, ApiError>((state, messages))
        }
        .await;
        let (state, messages) = match initial {
            Ok(v) => v,
            Err(e) => {
                rpc.stop();
                return Err(e.into());
            }
        };
        {
            let mut view = session.view.lock().await;
            view.messages = messages;
            view.streaming = None;
            view.pending.clear();
            view.error = view
                .messages
                .last()
                .filter(|message| {
                    message["role"] == "assistant" && message["stopReason"] == "error"
                })
                .map(|message| {
                    message["errorMessage"]
                        .as_str()
                        .unwrap_or("Model request failed")
                        .to_owned()
                });
            view.metadata.status = if view.error.is_some() {
                RunStatus::Failed
            } else {
                RunStatus::Idle
            };
            view.metadata.message_count = view.messages.len();
            view.metadata.queued_messages.clear();
            view.metadata.compaction_active = false;
            view.metadata.retry_active = false;
            update_config(&mut view, &state);
            view.seq += 1;
            let _ = session.events.send(view.frame());
        }
        *slot = Some(rpc.clone());
        drop(slot);
        Ok((session, rpc))
    }
}

fn update_config(view: &mut View, state: &RuntimeState) {
    view.metadata.config = ModelConfig {
        selection: state.model.clone(),
        thinking_level: state.thinking_level,
    };
    view.metadata.session_file = state.session_file.clone();
    view.readiness = Some(match &state.model {
        Some(selection) => Readiness::Ready {
            selection: selection.clone(),
            source: SelectionSource::Session,
        },
        None => Readiness::MissingModel {
            message: "Choose a model above. If no models are available, enable a provider and sign in in Cell settings.",
        },
    });
}

async fn list(
    State(store): State<Arc<Store>>,
    Query(query): Query<ListThreads>,
) -> Json<Vec<ThreadMetadata>> {
    let sessions: Vec<_> = store.sessions.lock().await.values().cloned().collect();
    let mut result = vec![];
    for session in sessions {
        let view = session.view.lock().await;
        if query.include_archived || !view.metadata.session.archived {
            result.push(view.metadata.clone());
        }
    }
    Json(result)
}

async fn create(
    State(store): State<Arc<Store>>,
    headers: HeaderMap,
    Json(body): Json<CreateThread>,
) -> ApiResult<Json<ThreadSnapshot>> {
    if body
        .workspace_path
        .as_ref()
        .is_some_and(|path| path != &store.config.workspace)
    {
        return Err(bad("This server is bound to one workspace"));
    }
    let record = Session::new(
        store.config.cell(),
        body.title.unwrap_or_else(|| "New chat".into()),
    );
    let id = record.id();
    save_session(&store.config, &record)?;
    let (events, _) = broadcast::channel(256);
    let session = Arc::new(LiveSession {
        view: Mutex::new(View::new(ThreadMetadata::new(
            record,
            store.config.workspace.clone(),
        ))),
        rpc: Mutex::new(None),
        events,
    });
    store.sessions.lock().await.insert(id, session.clone());
    store.open(&id).await?;
    if let Some(input) = body.initial_message {
        let target = session.view.lock().await.metadata.session.reference();
        execute(
            &store,
            &id,
            SessionCommand {
                request: request_id(&headers)?,
                session: target,
                action: SessionAction::Prompt(input),
            },
        )
        .await?;
    }
    let snapshot = session.view.lock().await.snapshot();
    Ok(Json(snapshot))
}

async fn snapshot(
    State(store): State<Arc<Store>>,
    Path(id): Path<SessionId>,
) -> ApiResult<Json<ThreadSnapshot>> {
    let (session, _) = store.open(&id).await?;
    let mut view = session.view.lock().await;
    view.seq += 1;
    let _ = session.events.send(view.frame());
    let snapshot = view.snapshot();
    Ok(Json(snapshot))
}

fn request_id(headers: &HeaderMap) -> ApiResult<RequestId> {
    headers
        .get("x-request-id")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| bad("invalid request ID"))?
                .parse()
                .map_err(|_| bad("invalid request ID"))
        })
        .transpose()
        .map(|id| id.unwrap_or_else(RequestId::generate))
}

async fn execute(store: &Store, id: &SessionId, command: SessionCommand) -> ApiResult<()> {
    let _settings = settings::lock(store)?;
    let session = store.find(id).await?;
    {
        let view = session.view.lock().await;
        command
            .session
            .check_owner(&store.config.cell())
            .map_err(|e| error(StatusCode::FORBIDDEN, e))?;
        command
            .validate(&view.metadata.session)
            .map_err(|e| bad(&e))?;
    }
    let permit = match store.requests.claim(&command).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            error(StatusCode::CONFLICT, e.to_string())
        } else {
            e.into()
        }
    })? {
        Claim::Recorded(Receipt::Accepted) => return Ok(()),
        Claim::Recorded(Receipt::Pending) => {
            let mut failure: ApiError = pi::CallError::OutcomeUnknown { message:
                "This request is pending or its outcome is unknown; it will not be submitted again".into() }.into();
            failure.status = StatusCode::CONFLICT;
            return Err(failure);
        }
        Claim::Recorded(Receipt::NotDispatched { status, message }) => {
            let mut failure: ApiError = pi::CallError::NotDispatched { message }.into();
            failure.status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            return Err(failure);
        }
        Claim::Recorded(Receipt::Rejected { message }) => {
            return Err(pi::CallError::Rejected { message }.into())
        }
        Claim::Fresh(permit) => permit,
    };
    // Opening the runtime and checking permissions cannot dispatch this prompt.
    // Record a known failure instead of leaving a misleading uncertain receipt.
    let preflight = async {
        let (_, rpc) = store.open(id).await?;
        if matches!(&command.action, SessionAction::Prompt(_)) {
            if let Some(cell) = store.config.declaration() {
                let state = rpc.call::<RuntimeState>(PiCommand::GetState).await?;
                if !settings::provider_allowed(
                    cell,
                    state
                        .model
                        .as_ref()
                        .map(|m| m.provider.as_str())
                        .unwrap_or(""),
                ) {
                    return Err(error(
                        StatusCode::FORBIDDEN,
                        "Select a provider granted to this cell".into(),
                    ));
                }
            }
        }
        Ok::<_, ApiError>(rpc)
    }
    .await;
    let rpc = match preflight {
        Ok(rpc) => rpc,
        Err(mut failure) => {
            permit.finish(SubmissionOutcome::NotDispatched {
                status: failure.status.as_u16(),
                message: failure.message.clone(),
            })?;
            failure.submission = Some(pi::CallError::NotDispatched {
                message: failure.message.clone(),
            });
            return Err(failure);
        }
    };
    let result = match &command.action {
        SessionAction::Prompt(input) => {
            let wire = PiCommand::Prompt {
                message: &input.content,
                streaming_behavior: input.streaming_behavior,
                images: &input.attachments,
            };
            {
                let mut view = session.view.lock().await;
                view.metadata.status = RunStatus::Running;
                view.seq += 1;
                let _ = session.events.send(view.frame());
            }
            rpc.call::<Value>(wire).await.map(|_| ())
        }
        SessionAction::Cancel => match rpc.call::<Value>(PiCommand::ClearQueue).await {
            Err(e) => Err(e),
            // After clearing the queue, cancellation has already partly taken
            // effect. Never replay the composite command if abort loses its ack.
            Ok(_) => rpc
                .call::<Value>(PiCommand::Abort)
                .await
                .map(|_| ())
                .map_err(|e| pi::CallError::OutcomeUnknown {
                    message: format!("Queue cleared, but cancellation is incomplete: {e}"),
                }),
        },
    };
    if let Err(failure) = result {
        match &failure {
            pi::CallError::NotDispatched { message } => {
                permit.finish(SubmissionOutcome::NotDispatched {
                    status: 502,
                    message: message.clone(),
                })?
            }
            pi::CallError::Rejected { message } => permit.finish(SubmissionOutcome::Rejected {
                message: message.clone(),
            })?,
            pi::CallError::OutcomeUnknown { .. } => drop(permit),
        }
        let running = if rpc.alive() {
            tokio::time::timeout(
                Duration::from_secs(3),
                rpc.call::<RuntimeState>(PiCommand::GetState),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(|state| state.is_streaming)
            .unwrap_or(true)
        } else {
            false
        };
        let mut view = session.view.lock().await;
        view.metadata.status = if running {
            RunStatus::Running
        } else {
            RunStatus::Failed
        };
        view.error = Some(failure.to_string());
        view.seq += 1;
        let _ = session.events.send(view.frame());
        return Err(failure.into());
    }
    // Acceptance is durable before any cosmetic metadata update can fail.
    permit.finish(SubmissionOutcome::Accepted)?;
    if let SessionAction::Prompt(input) = &command.action {
        let mut view = session.view.lock().await;
        if view.metadata.session.title == "New chat" {
            let title: String = input.content.chars().take(64).collect();
            view.metadata.session.title = title.clone();
            save_session(&store.config, &view.metadata.session)?;
            let event = view.stamp(json!({"type":"session_info_changed","name":title}));
            let _ = session.events.send(event);
        }
    }
    Ok(())
}

async fn message(
    State(store): State<Arc<Store>>,
    Path(id): Path<SessionId>,
    headers: HeaderMap,
    Json(body): Json<SendMessage>,
) -> ApiResult<StatusCode> {
    let session = store.find(&id).await?;
    let target = session.view.lock().await.metadata.session.reference();
    execute(
        &store,
        &id,
        SessionCommand {
            request: request_id(&headers)?,
            session: target,
            action: SessionAction::Prompt(body.input),
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn command(
    State(store): State<Arc<Store>>,
    Path(id): Path<SessionId>,
    Json(command): Json<SessionCommand>,
) -> ApiResult<StatusCode> {
    execute(&store, &id, command).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rename(
    State(store): State<Arc<Store>>,
    Path(id): Path<SessionId>,
    Json(body): Json<RenameThread>,
) -> ApiResult<StatusCode> {
    let title = body.title;
    if title.trim().is_empty() {
        return Err(bad("title must be a nonempty string"));
    }
    let session = store.find(&id).await?;
    let mut view = session.view.lock().await;
    view.metadata.session.title = title.to_owned();
    save_session(&store.config, &view.metadata.session)?;
    let event = view.stamp(json!({"type":"session_info_changed", "name":title}));
    let _ = session.events.send(event);
    Ok(StatusCode::NO_CONTENT)
}

async fn action(
    State(store): State<Arc<Store>>,
    Path((id, action)): Path<(SessionId, ThreadAction)>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult<Response> {
    let body = body.map(|b| b.0).unwrap_or(json!({}));
    if matches!(action, ThreadAction::Archive | ThreadAction::Unarchive) {
        let session = store.find(&id).await?;
        let mut view = session.view.lock().await;
        view.metadata.session.archived = action == ThreadAction::Archive;
        save_session(&store.config, &view.metadata.session)?;
        view.seq += 1;
        let _ = session.events.send(view.frame());
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if action == ThreadAction::Cancel {
        let session = store.find(&id).await?;
        let target = session.view.lock().await.metadata.session.reference();
        execute(
            &store,
            &id,
            SessionCommand {
                request: request_id(&headers)?,
                session: target,
                action: SessionAction::Cancel,
            },
        )
        .await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    match action {
        ThreadAction::Model | ThreadAction::Thinking => {
            let selection;
            let command = if action == ThreadAction::Model {
                selection = serde_json::from_value::<ModelSelection>(body)
                    .map_err(|e| bad(&e.to_string()))?;
                if store
                    .config
                    .declaration()
                    .is_some_and(|cell| !settings::provider_allowed(cell, &selection.provider))
                {
                    return Err(error(
                        StatusCode::FORBIDDEN,
                        "Provider is not granted to this cell".into(),
                    ));
                }
                PiCommand::SetModel {
                    selection: &selection,
                }
            } else {
                let change: SetThinking =
                    serde_json::from_value(body).map_err(|e| bad(&e.to_string()))?;
                PiCommand::SetThinkingLevel {
                    level: change.level,
                }
            };
            let (session, rpc) = store.open(&id).await?;
            rpc.call::<Value>(command).await?;
            let state = rpc.call::<RuntimeState>(PiCommand::GetState).await?;
            let mut view = session.view.lock().await;
            update_config(&mut view, &state);
            view.seq += 1;
            let _ = session.events.send(view.frame());
        }
        ThreadAction::HostUi => {
            let (session, rpc) = store.open(&id).await?;
            let submission: HostUiSubmission =
                serde_json::from_value(body).map_err(|e| bad(&e.to_string()))?;
            let request_id = submission.response.request_id().to_owned();
            let mut view = session.view.lock().await;
            let request = view
                .pending
                .get(&request_id)
                .ok_or_else(|| bad("This request is no longer pending"))?;
            let reply = submission.response.resolve(request).map_err(bad)?;
            rpc.write(pi::protocol::Notification::ExtensionUiResponse {
                id: &request_id,
                reply,
            })
            .await?;
            if let Some(event) =
                view.apply(json!({"type":"extension_ui_resolved","requestId":request_id}))
            {
                let _ = session.events.send(event);
            }
        }
        _ => unreachable!(),
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn clear_queue(
    State(store): State<Arc<Store>>,
    Path(id): Path<SessionId>,
) -> ApiResult<Json<Value>> {
    let (_, rpc) = store.open(&id).await?;
    Ok(Json(rpc.call::<Value>(PiCommand::ClearQueue).await?))
}

async fn models(State(store): State<Arc<Store>>) -> ApiResult<Json<Vec<ModelInfo>>> {
    let (rpc, mut events) = Rpc::spawn(
        &store.config.pi,
        &store.config.workspace,
        &store.config.state.join("pi"),
        "catalog",
        store.config.agent_dir(),
    )?;
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let result = rpc
        .call::<ModelCatalog>(PiCommand::GetAvailableModels)
        .await;
    rpc.stop();
    let models = result?
        .models
        .into_iter()
        .filter(|model| {
            store
                .config
                .declaration()
                .is_none_or(|cell| settings::provider_allowed(cell, &model.selection.provider))
        })
        .map(|model| ModelInfo {
            selection: model.selection,
            name: model.name,
            supports_thinking: model.reasoning,
        })
        .collect();
    Ok(Json(models))
}

async fn events(State(store): State<Arc<Store>>, Path(id): Path<SessionId>) -> ApiResult<Response> {
    let (session, _) = store.open(&id).await?;
    // Subscribe and take the snapshot under the same lock used by event publication.
    // Always send a snapshot, even if the client fetched one before subscribing.
    let view = session.view.lock().await;
    let mut receiver = session.events.subscribe();
    let initial = view.frame();
    let mut recovering = view.streaming.is_some();
    let mut watermark = view.seq;
    drop(view);
    let mut stopping = store.stopping.subscribe();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().json_data(initial).unwrap());
        loop {
            let incoming = tokio::select! {
                _ = stopping.changed() => break,
                event = receiver.recv() => event,
            };
            let event = match incoming {
                Ok(event) if event["seq"].as_u64().unwrap_or(0) <= watermark => continue,
                Ok(event) => {
                    let kind = event["type"].as_str().unwrap_or("");
                    if kind == "message_start" { recovering = false; }
                    // react-pi clears its streaming pointer on snapshots. Until the
                    // next message starts, reconcile the existing partial by snapshot.
                    if (recovering && ["message_update","message_end"].contains(&kind)) || kind == "snapshot" {
                        let view = session.view.lock().await;
                        recovering = view.streaming.is_some();
                        view.frame()
                    } else { event }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let view = session.view.lock().await;
                    recovering = view.streaming.is_some(); view.frame()
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            watermark = event["seq"].as_u64().unwrap_or(watermark);
            yield Ok(Event::default().json_data(event).unwrap());
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

async fn guard(State(store): State<Arc<Store>>, request: Request, next: Next) -> Response {
    if *store.stopping.borrow() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let expected = format!("http://{}", store.authority);
    let cross_site = request
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        == Some("cross-site");
    if host != Some(store.authority.as_str()) || origin.is_some_and(|o| o != expected) || cross_site
    {
        return (StatusCode::FORBIDDEN, "Use this server's loopback URL").into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'self'".parse().unwrap());
    response
}

pub async fn serve(mut config: Config) -> crate::apply::Fallible {
    if matches!(&config.mode, ExecutionMode::Local { listen } if !listen.ip().is_loopback()) {
        return Err("chat must listen on a loopback address".into());
    }
    config.workspace = config.workspace.canonicalize()?;
    if !config.workspace.is_dir() {
        return Err("workspace must be a directory".into());
    }
    fs::create_dir_all(&config.state)?;
    config.state = config.state.canonicalize()?;
    fs::set_permissions(&config.state, fs::Permissions::from_mode(0o700))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.state.join("lock"))?;
    lock.try_lock()
        .map_err(|_| "another chat server owns this state directory")?;
    if let (Some(directory), Some(cell)) = (config.agent_dir(), config.declaration()) {
        pi::models::configure(directory, cell)?;
    }
    let workspace_file = config.state.join("workspace.json");
    if workspace_file.exists() {
        let workspace: PathBuf = serde_json::from_slice(&fs::read(&workspace_file)?)?;
        if workspace != config.workspace {
            return Err(
                "state belongs to a different workspace; use another --state directory".into(),
            );
        }
    } else {
        fs::write(workspace_file, serde_json::to_vec(&config.workspace)?)?;
    }
    fs::create_dir_all(config.state.join("threads"))?;
    fs::create_dir_all(config.state.join("pi"))?;
    let mut sessions = BTreeMap::new();
    for entry in fs::read_dir(config.state.join("threads"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let (record, migrated) = if value.get("cell").is_some() {
            (serde_json::from_slice::<Session>(&bytes)?, false)
        } else {
            let legacy: LegacyThread = serde_json::from_slice(&bytes)?;
            if legacy.workspace_path != config.workspace {
                return Err("legacy session belongs to another workspace".into());
            }
            (
                Session::restore(legacy.id, config.cell(), legacy.title, legacy.archived),
                true,
            )
        };
        record.reference().check_owner(&config.cell())?;
        let id = record.id();
        if path.file_stem().and_then(|name| name.to_str()) != Some(&id.to_string()) {
            return Err("session filename does not match its ID".into());
        }
        if migrated {
            let backup = path.with_extension("json.bak");
            if !backup.exists() {
                fs::write(&backup, &bytes)?;
            }
            save_session(&config, &record).map_err(|e| e.message)?;
        }
        let metadata = ThreadMetadata::new(record, config.workspace.clone());
        let (events, _) = broadcast::channel(256);
        sessions.insert(
            id,
            Arc::new(LiveSession {
                view: Mutex::new(View::new(metadata)),
                rpc: Mutex::new(None),
                events,
            }),
        );
    }
    let requests = RequestJournal::new(&config.state.join("requests"))?;
    let listener = match &config.mode {
        ExecutionMode::Local { listen } => Some(tokio::net::TcpListener::bind(listen).await?),
        ExecutionMode::Cell { .. } => None,
    };
    let authority = listener
        .as_ref()
        .map(|l| l.local_addr().unwrap().to_string())
        .unwrap_or_else(|| "cell.internal".into());
    let assets = config.assets.clone();
    let (stopping, _) = watch::channel(false);
    let observed_path = config.state.join("provider-observations.json");
    let observed = if observed_path.exists() {
        serde_json::from_slice(&fs::read(observed_path)?)?
    } else {
        BTreeMap::new()
    };
    let store = Arc::new(Store {
        config,
        sessions: Mutex::new(sessions),
        authority,
        stopping,
        requests,
        settings_lock: Arc::new(Mutex::new(())),
        logins: Mutex::new(BTreeMap::new()),
        provider_observations: Arc::new(Mutex::new(observed)),
    });
    let app = Router::new()
        .route(
            "/api/info",
            get(|State(store): State<Arc<Store>>| async move {
                Json(json!({"workspace":store.config.workspace,"cell":store.config.cell(),"mode":if store.config.declaration().is_some() { "cell" } else { "local" },"runtime":"Pi"}))
            }),
        )
        .route("/api/pi/threads", get(list).post(create))
        .route("/api/pi/threads/:id", get(snapshot).patch(rename))
        .route("/api/pi/threads/:id/events", get(events))
        .route("/api/pi/threads/:id/messages", post(message))
        .route("/api/pi/threads/:id/queue/clear", post(clear_queue))
        .route("/api/pi/threads/:id/:action", post(action))
        .route("/api/pi/models", get(models))
        .route("/api/sessions/:id/commands", post(command))
        .route("/api/settings", get(settings::get))
        .route("/api/settings/providers/:provider/login", post(oauth::start))
        .route("/api/settings/providers/:provider/login/:id", get(oauth::get).post(oauth::respond).delete(oauth::cancel))
        .route("/api/settings/providers/:provider", axum::routing::delete(oauth::logout))
        .route("/api/settings/providers/:provider/check", post(provider_check::check))
        .route("/api/settings/connections", axum::routing::put(settings::connections))
        .fallback_service(ServeDir::new(assets))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(store.clone(), guard))
        .with_state(store.clone());
    if let Some(inbox) = store.config.inbox().map(std::path::Path::to_path_buf) {
        let target = store.clone();
        tokio::spawn(async move { inbox_loop(target, inbox).await });
    }
    let shutdown = store.clone();
    let signal = async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        shutdown.stopping.send_replace(true);
        let sessions: Vec<_> = shutdown.sessions.lock().await.values().cloned().collect();
        for session in sessions {
            if let Some(rpc) = session.rpc.lock().await.as_ref() {
                let _ = tokio::time::timeout(Duration::from_secs(3), async {
                    let _ = rpc.call::<Value>(PiCommand::ClearQueue).await;
                    let _ = rpc.call::<Value>(PiCommand::Abort).await;
                })
                .await;
                rpc.stop();
            }
        }
    };
    if let Some(listener) = listener {
        eprintln!("Kuggis chat: http://{}", store.authority);
        axum::serve(listener, app)
            .with_graceful_shutdown(signal)
            .await?;
    } else {
        let socket = store.config.socket().unwrap();
        if socket.exists() {
            fs::remove_file(socket)?;
        }
        let listener = tokio::net::UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
        eprintln!("Kuggis cell {}: {}", store.config.cell(), socket.display());
        tokio::pin!(signal);
        loop {
            tokio::select! {
                _ = &mut signal => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let service = hyper_util::service::TowerToHyperService::new(app.clone());
                    tokio::spawn(async move {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
                    });
                }
            }
        }
        fs::remove_file(socket)?;
    }
    drop(lock);
    Ok(())
}

fn speak(
    config: &Config,
    session: crate::SessionRef,
    state: crate::State,
    text: Option<String>,
) -> std::io::Result<()> {
    let Some(outbox) = config.outbox() else {
        return Ok(());
    };
    // The shipper sorts filenames; preserve order even when a run settles
    // before its next polling interval.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let name = format!("{stamp:032}-{}", RequestId::generate());
    let temp = outbox.join(format!("{name}.tmp"));
    fs::write(
        &temp,
        serde_json::to_vec(&crate::Said::Session {
            session,
            state,
            text,
        })?,
    )?;
    // The daemon uses umask 0077 for credentials and transcripts, but its
    // public statements must be readable by the separate cell-say account.
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))?;
    fs::rename(temp, outbox.join(format!("{name}.json")))
}

async fn inbox_loop(store: Arc<Store>, inbox: PathBuf) {
    let cursor = store.config.state.join("inbox-seen");
    let mut seen = fs::read_to_string(&cursor)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    loop {
        if *store.stopping.borrow() {
            break;
        }
        let mut entries: Vec<_> = fs::read_dir(&inbox)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".json")?
                    .parse::<u64>()
                    .ok()
            })
            .filter(|n| *n > seen)
            .collect();
        entries.sort_unstable();
        for n in entries {
            let result = async {
                let knock: crate::door::Knock =
                    serde_json::from_slice(&fs::read(inbox.join(format!("{n}.json")))?)
                        .map_err(|e| bad(&e.to_string()))?;
                if let crate::Said::Brief {
                    request,
                    session,
                    text,
                } = knock.said
                {
                    session
                        .check_owner(&store.config.cell())
                        .map_err(|e| bad(&e))?;
                    if store.find(&session.id).await.is_err() {
                        let mut sessions = store.sessions.lock().await;
                        if !sessions.contains_key(&session.id) {
                            let record = Session::restore(
                                session.id,
                                session.cell.clone(),
                                "New chat".into(),
                                false,
                            );
                            save_session(&store.config, &record)?;
                            let (events, _) = broadcast::channel(256);
                            sessions.insert(
                                session.id,
                                Arc::new(LiveSession {
                                    view: Mutex::new(View::new(ThreadMetadata::new(
                                        record,
                                        store.config.workspace.clone(),
                                    ))),
                                    rpc: Mutex::new(None),
                                    events,
                                }),
                            );
                        }
                    }
                    let id = session.id;
                    let cmd = SessionCommand {
                        request,
                        session: session.clone(),
                        action: SessionAction::Prompt(crate::Prompt {
                            content: text,
                            attachments: vec![],
                            streaming_behavior: Some(StreamingBehavior::FollowUp),
                        }),
                    };
                    if let Err(e) = execute(&store, &id, cmd).await {
                        speak(
                            &store.config,
                            session,
                            crate::State::Blocked,
                            Some(e.message.clone()),
                        )?;
                        return Err(e);
                    }
                }
                Ok::<_, ApiError>(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("inbox {n}: {}", e.message);
            }
            if let Err(e) = fs::write(&cursor, n.to_string()) {
                eprintln!("save inbox cursor: {e}");
                break;
            }
            seen = n;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;

    fn new_view() -> View {
        let name: crate::Name = "dev".parse().unwrap();
        let session = Session::new(name, "fixture".into());
        View::new(ThreadMetadata::new(session, PathBuf::from("/fixture")))
    }

    #[test]
    fn malformed_known_pi_events_fail_visibly_and_unknown_events_survive() {
        let mut view = new_view();
        let error = view
            .apply(json!({"type":"message_start","message":{"role":42}}))
            .unwrap();
        assert_eq!(error["type"], "error");
        assert!(error["error"]
            .as_str()
            .unwrap()
            .contains("Invalid known Pi event"));
        assert_eq!(view.metadata.status, RunStatus::Failed);

        let mut view = new_view();
        let error = view
            .apply(json!({"type":"queue_update","steering":[7],"followUp":[]}))
            .unwrap();
        assert_eq!(error["type"], "error");

        let mut view = new_view();
        let unknown = view
            .apply(json!({"type":"future_extension_event","extension":{"raw":true}}))
            .unwrap();
        assert_eq!(unknown["extension"]["raw"], true);
        assert_eq!(unknown["type"], "future_extension_event");
    }
}
