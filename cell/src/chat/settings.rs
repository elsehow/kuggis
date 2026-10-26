//! Cell-local credentials and MCP connections. Secrets never appear in responses.
use super::*;
use crate::connection::{
    ConnectionAuth, ConnectionChanges, ConnectionConfig, ConnectionName, McpEndpoint, SecretUpdate,
};
use crate::provider::*;
use crate::Provider;
use serde::Serialize;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

pub(super) fn provider_allowed(cell: &crate::Cell, name: &str) -> bool {
    Provider::from_pi(name).is_some_and(|provider| cell.providers().contains(&provider))
}

pub(super) fn directory(store: &Store) -> ApiResult<&std::path::Path> {
    store
        .config
        .agent_dir()
        .ok_or_else(|| bad("Provider settings are available for cells"))
}

pub(super) fn read(path: &std::path::Path) -> ApiResult<Value> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn write(path: &std::path::Path, value: &Value) -> ApiResult<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let tmp = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value).unwrap())?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub(super) fn lock(store: &Store) -> ApiResult<tokio::sync::MutexGuard<'_, ()>> {
    store.settings_lock.try_lock().map_err(|_| {
        error(
            StatusCode::CONFLICT,
            "Finish the current settings change or sign-in first".into(),
        )
    })
}

pub(super) async fn stop_idle(store: &Store) -> ApiResult<()> {
    let sessions: Vec<_> = store.sessions.lock().await.values().cloned().collect();
    for session in &sessions {
        if session.view.lock().await.metadata.status == RunStatus::Running {
            return Err(error(
                StatusCode::CONFLICT,
                "Stop active runs before changing cell settings".into(),
            ));
        }
    }
    for session in sessions {
        if let Some(rpc) = session.rpc.lock().await.take() {
            rpc.stop();
        }
        session.view.lock().await.generation += 1;
    }
    Ok(())
}

#[derive(Serialize, ts_rs::TS)]
pub(crate) struct SettingsView {
    cell: crate::Name,
    providers: Vec<ProviderConnection>,
    connections: Vec<ConnectionConfig>,
}

pub(super) async fn get(State(store): State<Arc<Store>>) -> ApiResult<Json<SettingsView>> {
    let dir = directory(&store)?;
    let auth = read(&dir.join("auth.json"))?;
    let mcp = read(&dir.join("mcp.json"))?;
    let observations = store.provider_observations.lock().await;
    let providers = Provider::ALL
        .into_iter()
        .map(|id| {
            let source = credential_source(id, &auth);
            let fingerprint = crate::Hash::of(&crate::canon::bytes(&source));
            let observed = observations
                .get(&id)
                .filter(|o| o.credential == fingerprint)
                .cloned()
                .unwrap_or_else(|| ProviderObservations::new(fingerprint));
            ProviderConnection {
                id,
                login_methods: id.login_methods(),
                granted: store
                    .config
                    .declaration()
                    .is_some_and(|c| c.providers().contains(&id)),
                credential: CredentialState::from_pi(&source, now()),
                authentication: observed.authentication,
                account: observed.account,
                last_request: observed.last_request,
            }
        })
        .collect();
    let mut connections = Vec::new();
    for name in ConnectionName::ALL {
        if let Some(entry) = mcp["mcpServers"].get(name.as_str()) {
            let url: McpEndpoint = serde_json::from_value(entry["url"].clone()).map_err(|_| {
                bad("A saved connection has an invalid endpoint; update its Pi configuration")
            })?;
            let auth = if entry["headers"]["Authorization"].is_string() {
                ConnectionAuth::Bearer { credential: name }
            } else {
                ConnectionAuth::None
            };
            connections.push(ConnectionConfig { name, url, auth });
        }
    }
    Ok(Json(SettingsView {
        cell: store.config.cell(),
        providers,
        connections,
    }))
}

pub(super) fn pi_provider(provider: Provider, auth: &Value) -> &'static str {
    if let Some(id) = provider.oauth_id() {
        if auth.get(id).is_some() {
            return id;
        }
    }
    provider.as_str()
}

fn credential_source(provider: Provider, auth: &Value) -> Value {
    auth[pi_provider(provider, auth)].clone()
}

pub(super) fn fingerprint(config: &Config, provider: Provider) -> ApiResult<crate::Hash> {
    let dir = config.agent_dir().ok_or_else(|| bad("Cell required"))?;
    Ok(crate::Hash::of(&crate::canon::bytes(&credential_source(
        provider,
        &read(&dir.join("auth.json"))?,
    ))))
}

pub(super) fn observation_for(
    observations: &mut BTreeMap<Provider, ProviderObservations>,
    provider: Provider,
    fingerprint: crate::Hash,
) -> &mut ProviderObservations {
    let observation = observations
        .entry(provider)
        .or_insert_with(|| ProviderObservations::new(fingerprint));
    if observation.credential != fingerprint {
        *observation = ProviderObservations::new(fingerprint);
    }
    observation
}

pub(super) async fn record_request(
    config: &Config,
    observations: &Mutex<BTreeMap<Provider, ProviderObservations>>,
    event: &Value,
) -> ApiResult<()> {
    let message = &event["message"];
    if config.agent_dir().is_none()
        || event["type"] != "message_end"
        || message["role"] != "assistant"
        || message["stopReason"] == "aborted"
    {
        return Ok(());
    }
    let Some(provider) = message["provider"].as_str().and_then(Provider::from_pi) else {
        return Ok(());
    };
    let observation = RequestObservation {
        observed_at: now(),
        model: message["model"].as_str().unwrap_or("unknown").into(),
        outcome: if message["stopReason"] == "error" {
            RequestOutcome::Failed {
                failure: ProviderFailure::from_message(
                    message["errorMessage"]
                        .as_str()
                        .unwrap_or("Model request failed")
                        .into(),
                ),
            }
        } else {
            RequestOutcome::Succeeded
        },
    };
    let fingerprint = fingerprint(config, provider)?;
    let mut observations = observations.lock().await;
    observation_for(&mut observations, provider, fingerprint).last_request = Some(observation);
    write(
        &config.state.join("provider-observations.json"),
        &json!(*observations),
    )
}

pub(super) async fn connections(
    State(store): State<Arc<Store>>,
    Json(body): Json<ConnectionChanges>,
) -> ApiResult<StatusCode> {
    let _lock = lock(&store)?;
    let path = directory(&store)?.join("mcp.json");
    let cell = store
        .config
        .declaration()
        .ok_or_else(|| bad("Cell required"))?;
    let mut mcp = read(&path)?;
    let old = mcp["mcpServers"].clone();
    let mut entries = serde_json::Map::new();
    for entry in body.into_entries() {
        let host = entry.url.host();
        if !cell.allowlist().contains(&host) {
            return Err(error(
                StatusCode::FORBIDDEN,
                format!("Grant {host} to this cell before connecting it"),
            ));
        }
        entries.insert(
            entry.name.as_str().into(),
            connection_value(&entry, &old[entry.name.as_str()]),
        );
    }
    if !mcp["mcpServers"].is_object() {
        mcp["mcpServers"] = json!({});
    }
    let servers = mcp["mcpServers"].as_object_mut().unwrap();
    for name in ConnectionName::ALL {
        servers.remove(name.as_str());
    }
    servers.extend(entries);
    stop_idle(&store).await?;
    write(&path, &mcp)?;
    Ok(StatusCode::NO_CONTENT)
}

fn connection_value(entry: &crate::connection::ConnectionUpdate, old: &Value) -> Value {
    let mut value = json!({"url":entry.url});
    match &entry.credential {
        SecretUpdate::Replace { value: token } => {
            value["headers"] = json!({"Authorization":token.authorization()})
        }
        SecretUpdate::Keep {} if old["url"] == value["url"] && old["headers"].is_object() => {
            value["headers"] = old["headers"].clone()
        }
        SecretUpdate::Keep {} | SecretUpdate::Clear {} => {}
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_kept_only_for_the_same_endpoint() {
        let old = json!({"url":"https://tools.example/mcp","headers":{"Authorization":"Bearer secret-fixture"}});
        let update = |url, credential| {
            serde_json::from_value(json!({"name":"portal","url":url,"credential":credential}))
                .unwrap()
        };
        let keep = json!({"operation":"keep"});
        assert_eq!(
            connection_value(&update("https://tools.example/mcp", keep.clone()), &old),
            old
        );
        assert!(
            connection_value(&update("https://other.example/mcp", keep), &old)
                .get("headers")
                .is_none()
        );
        assert!(connection_value(
            &update("https://tools.example/mcp", json!({"operation":"clear"})),
            &old
        )
        .get("headers")
        .is_none());
        assert_eq!(
            connection_value(
                &update(
                    "https://tools.example/mcp",
                    json!({"operation":"replace","value":"replacement"})
                ),
                &old
            )["headers"]["Authorization"],
            "Bearer replacement"
        );
    }
    #[test]
    fn account_and_request_observations_cannot_follow_a_changed_credential() {
        let mut observations = BTreeMap::new();
        let previous = observation_for(&mut observations, Provider::Anthropic, crate::Hash::ZERO);
        previous.last_request = Some(RequestObservation {
            observed_at: 1,
            model: "fixture".into(),
            outcome: RequestOutcome::Succeeded,
        });
        let changed = observation_for(
            &mut observations,
            Provider::Anthropic,
            crate::Hash::of(b"new-credential"),
        );
        assert!(changed.last_request.is_none());
        assert!(matches!(changed.account, AccountObservation::Unchecked));
    }
}
