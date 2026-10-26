//! Read-only account verification after Pi resolves/refreshes its credential.
//! A successful profile check never substitutes for an inference result.
use super::*;
use crate::{provider::*, Provider};
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AuthCheck {
    Ready {
        #[serde(rename = "authType")]
        method: AuthMethod,
    },
    #[serde(other)]
    Unavailable,
}

async fn resolve(config: &Config, provider: Provider) -> Authentication {
    let auth = config
        .agent_dir()
        .and_then(|dir| settings::read(&dir.join("auth.json")).ok())
        .unwrap_or_default();
    let pi_provider = settings::pi_provider(provider, &auth);
    let mut command = tokio::process::Command::new(&config.pi);
    command
        .args(["auth", "check", "--provider", pi_provider, "--json"])
        .current_dir(&config.workspace)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = config.agent_dir() {
        command.env("PI_CODING_AGENT_DIR", dir);
    }
    // Never pass --credentials: Pi owns refresh and reports only auth metadata.
    let output = tokio::time::timeout(Duration::from_secs(15), command.output()).await;
    if let Ok(Ok(output)) = output {
        if output.status.success() {
            if let Ok(AuthCheck::Ready { method }) = serde_json::from_slice(&output.stdout) {
                return Authentication::Resolved {
                    checked_at: now(),
                    method,
                };
            }
        }
    }
    Authentication::Unavailable {
        checked_at: now(),
        message: "Pi could not resolve credentials. Sign in again.".into(),
    }
}

async fn anthropic_profile(access: &str) -> Result<AccountIdentity, String> {
    if !access.starts_with("sk-ant-oat") || access.chars().any(char::is_control) {
        return Err("No Claude subscription credential available".into());
    }
    let mut child = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--max-time",
            "8",
            "--max-filesize",
            "65536",
            "-H",
            "@-",
            "https://api.anthropic.com/api/oauth/profile",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Could not start account verification")?;
    // Secret headers travel over stdin, never process arguments or logs.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!("Authorization: Bearer {access}\nanthropic-beta: oauth-2025-04-20\n")
                .as_bytes(),
        )
        .await
        .map_err(|_| "Could not send account verification")?;
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| "Account verification timed out")?
        .map_err(|_| "Account verification failed")?;
    if !output.status.success() || output.stdout.len() > 65536 {
        return Err("Anthropic account verification failed".into());
    }
    let profile: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Invalid account response")?;
    AccountIdentity::from_anthropic(&profile)
        .ok_or_else(|| "Account response omitted its identity".into())
}

pub(super) async fn check(
    State(store): State<Arc<Store>>,
    Path(provider): Path<Provider>,
) -> ApiResult<StatusCode> {
    let _settings = settings::acquire(&store, Duration::from_secs(10)).await?;
    let dir = settings::directory(&store)?;
    if !store
        .config
        .declaration()
        .is_some_and(|cell| cell.providers().contains(&provider))
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Provider is not granted to this cell".into(),
        ));
    }
    let authentication = resolve(&store.config, provider).await;
    let credential = settings::fingerprint(&store.config, provider)?;
    let account = if provider == Provider::Anthropic
        && matches!(
            authentication,
            Authentication::Resolved {
                method: AuthMethod::Oauth,
                ..
            }
        ) {
        let auth = settings::read(&dir.join("auth.json"))?;
        match anthropic_profile(auth["anthropic"]["access"].as_str().unwrap_or("")).await {
            Ok(identity) => AccountObservation::Verified {
                checked_at: now(),
                identity,
            },
            Err(message) => AccountObservation::Unavailable {
                checked_at: now(),
                message,
            },
        }
    } else {
        AccountObservation::Unchecked
    };
    // Refuse to attach an observation if an external login changed the store.
    if settings::fingerprint(&store.config, provider)? != credential {
        return Err(error(
            StatusCode::CONFLICT,
            "Credentials changed during verification; check again".into(),
        ));
    }
    let mut observations = store.provider_observations.lock().await;
    let observation = settings::observation_for(&mut observations, provider, credential);
    observation.authentication = authentication;
    observation.account = account;
    settings::write(
        &store.config.state.join("provider-observations.json"),
        &json!(*observations),
    )?;
    Ok(StatusCode::NO_CONTENT)
}
