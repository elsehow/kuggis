//! Loopback-only, one-attempt callback listeners. No OAuth or credentials here.
use super::*;
use crate::chat::oauth::{AuthFlow, AuthNotice, AuthPromptKind, AuthState, Callback, RelayState};
use axum::extract::Query;
use std::{
    future::IntoFuture,
    time::{Duration, Instant},
};
use tokio::net::TcpListener;

pub(super) struct Relay {
    started: Instant,
    state: Arc<Mutex<RelayState>>,
}

#[derive(Deserialize)]
struct ResponseQuery {
    state: String,
    code: String,
}

fn coordinates(callback: &Callback) -> Option<(SocketAddr, String, String)> {
    let uri: axum::http::Uri = callback.address.parse().ok()?;
    if uri.scheme_str() != Some("http") || uri.query().is_some() || callback.state.is_empty() {
        return None;
    }
    let ip = match uri.host()? {
        "localhost" | "127.0.0.1" => "127.0.0.1",
        "[::1]" | "::1" => "::1",
        _ => return None,
    };
    let port = uri.port_u16()?;
    if port == 0 {
        return None;
    }
    Some((
        SocketAddr::new(ip.parse().ok()?, port),
        uri.path().into(),
        uri.authority()?.to_string(),
    ))
}

async fn flow(op: &Operator, cell: &Name, path: &str) -> Result<AuthFlow> {
    let response = op
        .forward(
            cell,
            Request::builder().uri(path).body(Body::empty()).unwrap(),
        )
        .await?;
    if !response.status().is_success() {
        return Err(fail("Sign-in unavailable"));
    }
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .map_err(|_| fail("Sign-in unavailable"))?;
    serde_json::from_slice(&bytes).map_err(|_| fail("Invalid sign-in status"))
}

struct Attempt {
    op: Arc<Operator>,
    cell: Name,
    path: String,
    callback: Callback,
    callback_path: String,
    authority: String,
    state: Arc<Mutex<RelayState>>,
    started: Instant,
}

async fn callback(State(attempt): State<Arc<Attempt>>, request: Request) -> Response {
    tokio::time::timeout(Duration::from_secs(15), receive_callback(attempt, request))
        .await
        .unwrap_or_else(|_| {
            (
                StatusCode::GATEWAY_TIMEOUT,
                "Sign-in relay timed out; check Kuggis",
            )
                .into_response()
        })
}

async fn receive_callback(attempt: Arc<Attempt>, request: Request) -> Response {
    let reject = || {
        (
            StatusCode::BAD_REQUEST,
            "Invalid or expired sign-in callback",
        )
            .into_response()
    };
    if request.method() != axum::http::Method::GET
        || request.uri().path() != attempt.callback_path
        || request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            != Some(attempt.authority.as_str())
        || request.uri().to_string().len() > 8192
        || attempt.started.elapsed() >= Duration::from_secs(600)
    {
        return reject();
    }
    let Ok(Query(query)) = Query::<ResponseQuery>::try_from_uri(request.uri()) else {
        return reject();
    };
    if query.state != attempt.callback.state
        || query.code.is_empty()
        || query.code.chars().any(char::is_control)
    {
        return reject();
    }
    // Hold the claim through forwarding: concurrent callbacks cannot both reply.
    let mut state = attempt.state.lock().await;
    if !matches!(*state, RelayState::Ready) {
        return reject();
    }
    let Ok(current) = flow(&attempt.op, &attempt.cell, &attempt.path).await else {
        return reject();
    };
    if attempt.path
        != format!(
            "/api/settings/providers/{}/login/{}",
            current.provider.as_str(),
            current.id
        )
    {
        return reject();
    }
    let AuthState::Waiting {
        prompt: Some(prompt),
        notice:
            Some(AuthNotice::Link {
                callback: Some(ref expected),
                ..
            }),
    } = current.status
    else {
        return reject();
    };
    if expected.address != attempt.callback.address
        || expected.state != query.state
        || !matches!(prompt.kind, AuthPromptKind::Input { secret: false, .. })
    {
        return reject();
    }
    let value = format!(
        "{}?{}",
        attempt.callback.address,
        request.uri().query().unwrap_or("")
    );
    // Claim before I/O so cancellation or a timeout cannot make the code reusable.
    *state = RelayState::Forwarded;
    let response = attempt
        .op
        .forward(
            &attempt.cell,
            Request::builder()
                .method("POST")
                .uri(&attempt.path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"id":prompt.id,"value":value})).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    // An ambiguous transport failure must not replay a possibly accepted code.
    *state = RelayState::Forwarded;
    if !matches!(response, Ok(ref response) if response.status().is_success()) {
        return (
            StatusCode::BAD_GATEWAY,
            "Could not forward sign-in. Return to Kuggis to check or retry.",
        )
            .into_response();
    }
    // This is receipt, NOT completion. Only Pi can report completion. The
    // original tab keeps polling; the callback tab returns without secrets.
    let location = format!(
        "http://{}/?cell={}&setup=1&signin={}&provider={}",
        attempt.op.authority,
        attempt.cell,
        current.id,
        current.provider.as_str()
    );
    let mut response = axum::response::Redirect::to(&location).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}

/// Called before a flow (and therefore its browser link) reaches React.
pub(super) async fn prepare(op: &Arc<Operator>, cell: &Name, path: &str, view: &mut AuthFlow) {
    prepare_with_lifetime(op, cell, path, view, Duration::from_secs(600)).await;
}

async fn prepare_with_lifetime(
    op: &Arc<Operator>,
    cell: &Name,
    path: &str,
    view: &mut AuthFlow,
    lifetime: Duration,
) {
    // A browser on another machine cannot reach this VM's loopback listener.
    // Keep Pi's manual redirect prompt for authenticated hosted supervisors.
    if !matches!(op.config.access, Access::Local) {
        return;
    }
    let AuthState::Waiting {
        notice:
            Some(AuthNotice::Link {
                callback: Some(callback),
                ..
            }),
        ..
    } = &view.status
    else {
        return;
    };
    if path
        != format!(
            "/api/settings/providers/{}/login/{}",
            view.provider.as_str(),
            view.id
        )
    {
        return;
    }
    let key = format!("{cell}/{}/{}", view.provider.as_str(), view.id);
    let mut relays = op.relays.lock().await;
    relays.retain(|_, relay| relay.started.elapsed() < Duration::from_secs(660));
    if let Some(relay) = relays.get(&key) {
        view.relay = relay.state.lock().await.clone();
        return;
    }
    if relays.len() >= 128 {
        return;
    }
    let state = Arc::new(Mutex::new(RelayState::Manual));
    let started = Instant::now();
    relays.insert(
        key,
        Relay {
            started,
            state: state.clone(),
        },
    );
    let Some((address, callback_path, authority)) = coordinates(callback) else {
        return;
    };
    // Never probe, terminate or reuse another application's listener.
    let Ok(listener) = TcpListener::bind(address).await else {
        return;
    };
    *state.lock().await = RelayState::Ready;
    view.relay = RelayState::Ready;
    let attempt = Arc::new(Attempt {
        op: op.clone(),
        cell: cell.clone(),
        path: path.into(),
        callback: callback.clone(),
        callback_path,
        authority,
        state: state.clone(),
        started,
    });
    let app = Router::new()
        .fallback(any(self::callback))
        .with_state(attempt.clone());
    let mut stopping = op.stopping.subscribe();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).into_future();
        tokio::pin!(server);
        let deadline = tokio::time::sleep(lifetime);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut server => break,
                _ = &mut deadline => break,
                _ = stopping.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let status = tokio::time::timeout(Duration::from_secs(5), flow(&attempt.op, &attempt.cell, &attempt.path)).await;
                    if !matches!(status, Ok(Ok(ref flow)) if !flow.status.terminal()) { break; }
                }
            }
        }
        *state.lock().await = RelayState::Closed;
        // Dropping the server releases only our own listening socket.
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_loopback_callback_addresses() {
        for address in [
            "http://localhost:1455/auth/callback",
            "http://127.0.0.1:8765/callback",
            "http://[::1]:8765/callback",
        ] {
            assert!(coordinates(&Callback {
                address: address.into(),
                state: "fixture".into()
            })
            .is_some());
        }
        for address in [
            "https://localhost:1455/callback",
            "http://example.com:1455/callback",
            "http://0.0.0.0:1455/",
            "http://localhost/",
            "http://localhost:0/",
            "http://localhost:1234/?code=x",
        ] {
            assert!(coordinates(&Callback {
                address: address.into(),
                state: "fixture".into()
            })
            .is_none());
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The remote cell/Pi bridge is simulated over its real Unix HTTP transport.
    // No provider network, user credentials or model request is used.
    struct Fixture {
        op: Arc<Operator>,
        cell: Name,
        path: String,
        view: Arc<Mutex<Value>>,
        replies: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
        directory: PathBuf,
    }
    impl Fixture {
        async fn new(provider: &str, port: u16) -> Self {
            let directory =
                std::env::temp_dir().join(format!("callback-test-{}", RequestId::generate()));
            fs::create_dir_all(directory.join("tree")).unwrap();
            fs::create_dir_all(directory.join("state")).unwrap();
            let cell: Name = "fixture".parse().unwrap();
            let declaration: Cell =
                serde_json::from_value(json!({"name":"fixture","grants":[{"provider":provider}]}))
                    .unwrap();
            fs::write(directory.join("tree/fixture.json"), declaration.canonical()).unwrap();
            let id = RequestId::generate();
            let path = format!("/api/settings/providers/{provider}/login/{id}");
            let view = Arc::new(Mutex::new(
                json!({"id":id,"provider":provider,"state":"waiting",
                "notice":{"kind":"link","url":"https://provider.invalid/authorize","instructions":null,
                    "callback":{"address":format!("http://127.0.0.1:{port}/callback"),"state":"isolated-state"}},
                "prompt":{"id":"isolated-prompt","kind":"input","message":"Redirect URL","placeholder":null,"secret":false}}),
            ));
            let replies = Arc::new(AtomicUsize::new(0));
            let listener = tokio::net::UnixListener::bind(directory.join("fixture.sock")).unwrap();
            let remote_view = view.clone();
            let remote_replies = replies.clone();
            let remote_path = path.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let view = remote_view.clone();
                    let replies = remote_replies.clone();
                    let path = remote_path.clone();
                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(
                            move |request: hyper::Request<hyper::body::Incoming>| {
                                let view = view.clone();
                                let replies = replies.clone();
                                let path = path.clone();
                                async move {
                                    assert_eq!(request.uri().path(), path);
                                    let mut view = view.lock().await;
                                    let response = if request.method() == axum::http::Method::POST {
                                        let bytes = axum::body::to_bytes(
                                            Body::new(request.into_body()),
                                            16384,
                                        )
                                        .await
                                        .unwrap();
                                        let reply: Value = serde_json::from_slice(&bytes).unwrap();
                                        assert_eq!(reply["id"], "isolated-prompt");
                                        assert!(reply["value"]
                                            .as_str()
                                            .unwrap()
                                            .contains("code=fixture-code"));
                                        if path.contains("/openai/") {
                                            assert!(reply["value"]
                                                .as_str()
                                                .unwrap()
                                                .contains("client_id=issued-client"));
                                        }
                                        replies.fetch_add(1, Ordering::SeqCst);
                                        view["prompt"] = Value::Null;
                                        StatusCode::NO_CONTENT.into_response()
                                    } else {
                                        Json(view.clone()).into_response()
                                    };
                                    Ok::<_, std::convert::Infallible>(response)
                                }
                            },
                        );
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    });
                }
            });
            let op = Arc::new(Operator {
                config: Config {
                    tree: directory.join("tree"),
                    state: directory.join("state"),
                    assets: directory.clone(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    binary: None,
                    sockets: Some(directory.clone()),
                    access: Access::Local,
                },
                authority: "127.0.0.1:9999".into(),
                relays: Mutex::new(BTreeMap::new()),
                vms: tokio::sync::RwLock::new(BTreeMap::new()),
                edge: Arc::new(Ssh { bin: None }),
                changes: Mutex::new(BTreeMap::new()),
                applying: Arc::new(Mutex::new(())),
                stopping: tokio::sync::watch::channel(false).0,
            });
            Self {
                op,
                cell,
                path,
                view,
                replies,
                server,
                directory,
            }
        }
        async fn prepare(&self) -> AuthFlow {
            let mut view = flow(&self.op, &self.cell, &self.path).await.unwrap();
            prepare(&self.op, &self.cell, &self.path, &mut view).await;
            view
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.op.stopping.send_replace(true);
            self.server.abort();
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    async fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
    async fn get(port: u16, path: &str) -> Response {
        let stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender
            .send_request(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, format!("127.0.0.1:{port}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        Response::from_parts(parts, Body::new(body))
    }
    async fn assert_released(port: u16) {
        for _ in 0..40 {
            if TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("callback listener leaked");
    }
    #[tokio::test]
    async fn hosted_supervisor_keeps_manual_login_without_binding_a_callback() {
        let port = free_port().await;
        let mut f = Fixture::new("openai", port).await;
        let config = f.directory.join("access.json");
        fs::write(&config, br#"{"origin":"https://supervisor.example","users":["usr_fixture"]}"#).unwrap();
        Arc::get_mut(&mut f.op).unwrap().config.access = Access::exe_proxy(&config).unwrap();
        assert!(matches!(f.prepare().await.relay, RelayState::Manual));
        assert!(f.op.relays.lock().await.is_empty());
        let _listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await.unwrap();
        assert_eq!(f.replies.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn both_providers_validate_forward_once_and_wait_for_pi() {
        for provider in ["openai", "anthropic"] {
            let port = free_port().await;
            let f = Fixture::new(provider, port).await;
            assert!(matches!(f.prepare().await.relay, RelayState::Ready));
            for path in [
                "/other?state=isolated-state&code=fixture-code",
                "/callback?state=wrong&code=fixture-code",
                "/callback?state=isolated-state&state=wrong&code=fixture-code",
            ] {
                assert_eq!(get(port, path).await.status(), StatusCode::BAD_REQUEST);
            }
            assert_eq!(f.replies.load(Ordering::SeqCst), 0);
            let accepted = get(
                port,
                "/callback?state=isolated-state&code=fixture-code&client_id=issued-client",
            )
            .await;
            assert_eq!(accepted.status(), StatusCode::SEE_OTHER);
            let location = accepted.headers()[header::LOCATION].to_str().unwrap();
            assert!(location.contains("cell=fixture"));
            assert!(!location.contains("code="));
            assert!(!location.contains("state="));
            assert!(matches!(
                f.prepare().await.status,
                AuthState::Waiting { .. }
            ));
            assert_eq!(
                get(
                    port,
                    "/callback?state=isolated-state&code=fixture-code&client_id=issued-client"
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
            assert_eq!(f.replies.load(Ordering::SeqCst), 1);
            f.view.lock().await["state"] = json!("complete");
            assert_released(port).await;
            assert!(matches!(f.prepare().await.status, AuthState::Complete));
        }
    }
    #[tokio::test]
    async fn occupied_ports_and_concurrent_attempts_preserve_manual_fallback() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let f = Fixture::new("openai", port).await;
        assert!(matches!(f.prepare().await.relay, RelayState::Manual));
        drop(occupied);
        // No surprise takeover if the other application later releases its port.
        assert!(matches!(f.prepare().await.relay, RelayState::Manual));
        let first = Fixture::new("anthropic", port).await;
        assert!(matches!(first.prepare().await.relay, RelayState::Ready));
        let second = Fixture::new("openai", port).await;
        assert!(matches!(second.prepare().await.relay, RelayState::Manual));
        first.view.lock().await["state"] = json!("cancelled");
        assert_released(port).await;
        assert_eq!(first.replies.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn expiry_closes_listener_and_does_not_rearm_attempt() {
        for provider in ["openai", "anthropic"] {
            let port = free_port().await;
            let f = Fixture::new(provider, port).await;
            let mut view = flow(&f.op, &f.cell, &f.path).await.unwrap();
            prepare_with_lifetime(
                &f.op,
                &f.cell,
                &f.path,
                &mut view,
                Duration::from_millis(50),
            )
            .await;
            assert!(matches!(view.relay, RelayState::Ready));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_released(port).await;
            assert!(matches!(f.prepare().await.relay, RelayState::Closed));
            assert_eq!(f.replies.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn simultaneous_callbacks_only_forward_once() {
        for provider in ["openai", "anthropic"] {
            let port = free_port().await;
            let f = Fixture::new(provider, port).await;
            f.prepare().await;
            let path = "/callback?state=isolated-state&code=fixture-code&client_id=issued-client";
            let (a, b) = tokio::join!(get(port, path), get(port, path));
            assert!([a.status(), b.status()].contains(&StatusCode::SEE_OTHER));
            assert!([a.status(), b.status()].contains(&StatusCode::BAD_REQUEST));
            assert_eq!(f.replies.load(Ordering::SeqCst), 1);
            f.view.lock().await["state"] = json!("cancelled");
            assert_released(port).await;
        }
    }
    #[tokio::test]
    async fn expired_and_wrong_flow_callbacks_are_rejected() {
        let port = free_port().await;
        let f = Fixture::new("anthropic", port).await;
        let attempt = |started| {
            Arc::new(Attempt {
                op: f.op.clone(),
                cell: f.cell.clone(),
                path: f.path.clone(),
                callback: Callback {
                    address: format!("http://127.0.0.1:{port}/callback"),
                    state: "isolated-state".into(),
                },
                callback_path: "/callback".into(),
                authority: format!("127.0.0.1:{port}"),
                state: Arc::new(Mutex::new(RelayState::Ready)),
                started,
            })
        };
        let request = || {
            Request::builder()
                .uri("/callback?state=isolated-state&code=fixture-code&client_id=issued-client")
                .header(header::HOST, format!("127.0.0.1:{port}"))
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            receive_callback(
                attempt(Instant::now() - Duration::from_secs(601)),
                request()
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        f.view.lock().await["state"] = json!("cancelled");
        assert_eq!(
            receive_callback(attempt(Instant::now()), request())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        f.view.lock().await["state"] = json!("waiting");
        let original_id = f.view.lock().await["id"].clone();
        f.view.lock().await["id"] = json!(RequestId::generate());
        assert_eq!(
            receive_callback(attempt(Instant::now()), request())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        f.view.lock().await["id"] = original_id;
        f.view.lock().await["notice"]["callback"]["state"] = json!("different-flow");
        assert_eq!(
            receive_callback(attempt(Instant::now()), request())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(f.replies.load(Ordering::SeqCst), 0);
    }
}
