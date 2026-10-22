//! Signed, audience-bound publisher requests. The HTTP server has NO operator
//! routes. Signatures authenticate possession of a key; the registry's current
//! approved policy decides what that key may do. VM/source headers are ignored.
//! Serve on loopback behind a trusted HTTPS frontend (or an SSH tunnel).

use super::registry::{Registry, RegistryError, SiteAction};
use super::{artifact::Artifact, PublicationReceipt, SiteId};
use crate::prim::{hex_bytes, hex_string};
use crate::{canon, Key, RequestId, Secret};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_LIFETIME_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Publish {
        site: SiteId,
        artifact: crate::Hash,
    },
    Status {
        site: SiteId,
    },
    Rollback {
        site: SiteId,
        version: RequestId,
    },
    Unpublish {
        site: SiteId,
    },
    ReserveSite {
        site: SiteId,
    },
    /// A check only, not a deployment operation or reusable permission token.
    Check {
        site: SiteId,
        action: SiteAction,
    },
}
impl Command {
    pub fn site(&self) -> &SiteId {
        match self {
            Self::ReserveSite { site }
            | Self::Check { site, .. }
            | Self::Publish { site, .. }
            | Self::Status { site }
            | Self::Rollback { site, .. }
            | Self::Unpublish { site } => site,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    audience: RequestId,
    caller: Key,
    request: RequestId,
    expires: u64,
    command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRequest {
    body: Body,
    signature: String,
}
/// Can only be obtained by verifying a signature. Not a long-lived authority:
/// the registry still checks audience, expiry, replay and current policy.
pub(super) struct VerifiedRequest {
    pub audience: RequestId,
    pub caller: Key,
    pub request: RequestId,
    pub expires: u64,
    pub command: Command,
}
impl SignedRequest {
    /// For the trusted host-side signer, not a reason to copy its key to agents.
    pub fn sign(
        secret: &Secret,
        audience: RequestId,
        request: RequestId,
        expires: u64,
        command: Command,
    ) -> Self {
        let body = Body {
            audience,
            caller: secret.public(),
            request,
            expires,
            command,
        };
        let signature = hex_string(&secret.sign_publisher(&canon::bytes(&body)));
        Self { body, signature }
    }
    pub(super) fn verify(&self) -> Result<VerifiedRequest, RegistryError> {
        let signature =
            hex_bytes::<64>(&self.signature).map_err(|_| RegistryError::Unauthenticated)?;
        if !self
            .body
            .caller
            .verify_publisher(&canon::bytes(&self.body), &signature)
        {
            return Err(RegistryError::Unauthenticated);
        }
        Ok(VerifiedRequest {
            audience: self.body.audience,
            caller: self.body.caller,
            request: self.body.request,
            expires: self.body.expires,
            command: self.body.command.clone(),
        })
    }
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Reserved,
    Authorized,
    Published(PublicationReceipt),
    RolledBack(PublicationReceipt),
    Unpublished,
    Status {
        site: SiteId,
        active: Option<PublicationReceipt>,
        versions: Vec<PublicationReceipt>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub request: SignedRequest,
    pub artifact: Artifact,
}

pub fn unix_time() -> std::io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(std::io::Error::other)
}

pub fn router(registry: Registry) -> Router {
    Router::new()
        .route("/v1/requests", post(request))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .route(
            "/v1/uploads",
            post(upload).layer(DefaultBodyLimit::max(super::artifact::MAX_WIRE_BYTES)),
        )
        .with_state(Arc::new(registry))
        .layer(axum::middleware::from_fn({
            let capacity = Arc::new(tokio::sync::Semaphore::new(8));
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let capacity = capacity.clone();
                async move {
                    use axum::response::IntoResponse;
                    let Ok(_permit) = capacity.try_acquire_owned() else {
                        return StatusCode::TOO_MANY_REQUESTS.into_response();
                    };
                    // Applied before JSON extraction: bound simultaneous uploads,
                    // not merely the already-parsed handler invocations.
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        next.run(request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
                    }
                }
            }
        }))
}
async fn request(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<SignedRequest>,
) -> Result<Json<Reply>, (StatusCode, &'static str)> {
    tokio::task::spawn_blocking(move || registry.execute(&request))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "publisher unavailable"))?
        .map(Json)
        .map_err(public_error)
}
async fn upload(
    State(registry): State<Arc<Registry>>,
    Json(upload): Json<Upload>,
) -> Result<Json<Reply>, (StatusCode, &'static str)> {
    tokio::task::spawn_blocking(move || registry.execute_upload(&upload.request, &upload.artifact))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "publisher unavailable"))?
        .map(Json)
        .map_err(public_error)
}
fn public_error(error: RegistryError) -> (StatusCode, &'static str) {
    match error {
        RegistryError::InvalidArtifact(_) => (
            StatusCode::BAD_REQUEST,
            "invalid publication artifact or configuration",
        ),
        RegistryError::Unauthenticated => (StatusCode::UNAUTHORIZED, "invalid publisher request"),
        RegistryError::Denied => (StatusCode::FORBIDDEN, "publishing authority denied"),
        RegistryError::Replay | RegistryError::SiteExists => {
            (StatusCode::CONFLICT, "request or site already exists")
        }
        RegistryError::UnknownSite | RegistryError::UnknownNamespace => {
            (StatusCode::NOT_FOUND, "site not found")
        }
        RegistryError::Busy => (
            StatusCode::TOO_MANY_REQUESTS,
            "publisher request capacity reached",
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "publisher unavailable"),
    }
}

/// Deliberately refuses a public plaintext bind. TLS termination and ingress
/// provisioning remain operator work; do not route the cell's private door here.
pub async fn serve(registry: Registry, address: std::net::SocketAddr) -> crate::apply::Fallible {
    if !address.ip().is_loopback() {
        return Err("publisher must listen on loopback behind HTTPS or SSH".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router(registry)).await?;
    Ok(())
}
