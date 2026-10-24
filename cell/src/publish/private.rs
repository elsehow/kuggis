//! Private static sites use one HTTPS origin per resource, behind a trusted
//! authentication gateway. No browser identity or cookie is passed to a cell.
use super::{registry::Registry, transport::SignedRequest};
use crate::{Hash, Host, Key, RequestId};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Gateway hostname; without ports, wildcard DNS/TLS must cover *.domain.
    pub domain: Host,
    /// Native exe.xyz hosting: one stable port origin per Site. Omit for wildcard DNS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortRange>,
    /// Stable identities asserted by the trusted ingress, never by the cell.
    pub users: BTreeSet<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRange {
    pub first: u16,
    pub last: u16,
}
impl Config {
    pub(crate) fn overlaps(&self, base: &crate::Url) -> bool {
        if let Some(range) = self.ports {
            let authority = base
                .as_str()
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap();
            let port = authority
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .unwrap_or(443);
            base.host() == self.domain && (range.first..=range.last).contains(&port)
        } else {
            base.host() == self.domain
                || base.host().as_str().ends_with(&format!(".{}", self.domain))
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.ports.is_some_and(|p| {
            p.first < 3000 || p.last > 9998 || p.first > p.last || p.last - p.first > 999
        }) {
            return Err(
                "Private ports must be a range of at most 1000 ports within 3000–9998".into(),
            );
        }
        if self.domain.as_str().len() > 180
            || self.users.is_empty()
            || self.users.iter().any(|id| {
                id.is_empty()
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            })
        {
            return Err("Private hosting requires a DNS domain and stable reader IDs".into());
        }
        Ok(())
    }
}
/// Deterministic within this provider and pinned key, never a mutable cell label.
pub fn namespace(audience: RequestId, key: Key) -> super::NamespaceId {
    super::NamespaceId(Hash::of(&crate::canon::bytes(&(
        "private-sites-v1",
        audience,
        key,
    ))))
}
/// A complete 256-bit resource identity encoded as one DNS label.
pub fn label(id: crate::resource::ResourceId) -> String {
    let hex = id.0.to_string();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let mut result = String::from("s");
    let (mut bits, mut value) = (0u32, 0u32);
    for byte in bytes {
        value = (value << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            result
                .push(b"abcdefghijklmnopqrstuvwxyz234567"[((value >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        result.push(
            b"abcdefghijklmnopqrstuvwxyz234567"[((value << (5 - bits)) & 31) as usize] as char,
        );
    }
    result
}
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
pub fn router(registry: Registry) -> Router {
    Router::new().fallback(read).with_state(Arc::new(registry))
}
async fn read(State(registry): State<Arc<Registry>>, request: Request) -> Response {
    let mut response = read_inner(registry, request).await;
    let headers = response.headers_mut();
    headers.insert("cache-control", "private, no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("origin-agent-cluster", "?1".parse().unwrap());
    headers.insert("permissions-policy", "document-domain=()".parse().unwrap());
    // Unique resource origins permit modules/fetch without credentialed CORS.
    // No service workers or external network; never hosted on the operator origin.
    headers.insert("content-security-policy", "sandbox allow-scripts allow-same-origin; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; media-src 'self'; worker-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}
async fn read_inner(registry: Arc<Registry>, request: Request) -> Response {
    if request.method() != "GET" && request.method() != "HEAD" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let h = request.headers();
    let Some(host) = single(h, "host") else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if single(h, "x-forwarded-host") != Some(host)
        || single(h, "x-forwarded-proto") != Some("https")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let host = host.to_owned();
    let origin = format!("https://{host}");
    let navigation = single(h, "sec-fetch-mode") == Some("navigate")
        && single(h, "sec-fetch-dest") == Some("document");
    if h.contains_key("origin") && single(h, "origin") != Some(&origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if h.contains_key("sec-fetch-site")
        && !matches!(single(h, "sec-fetch-site"), Some("same-origin" | "none"))
        && !navigation
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let user = single(h, "x-exedev-userid").map(str::to_owned);
    // Only an expiring, site-scoped Verify request is accepted here. It grants
    // read access, not mutation, and current policy is checked for every file.
    let proof = single(h, "x-cell-site-verification")
        .and_then(|v| serde_json::from_str::<SignedRequest>(v).ok());
    if h.contains_key("x-cell-site-verification") && proof.is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = request.uri().path().trim_start_matches('/');
    let path = if path.ends_with('/') || path.is_empty() {
        format!("{path}index.html")
    } else {
        path.into()
    };
    let mime = super::hosting::mime(&path);
    match tokio::task::spawn_blocking(move || {
        registry.private_file(&host, &path, user.as_deref(), proof.as_ref())
    })
    .await
    {
        Ok(Ok((receipt, bytes))) => (
            [
                ("content-type", mime),
                ("x-cell-site-version", &receipt.version.to_string()),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(super::registry::RegistryError::Denied)) => StatusCode::FORBIDDEN.into_response(),
        Ok(Err(super::registry::RegistryError::UnknownSite)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
pub async fn serve(registry: Registry, address: std::net::SocketAddr) -> crate::apply::Fallible {
    if !address.ip().is_loopback() {
        return Err(
            "Private hosting must bind loopback behind a trusted authentication gateway".into(),
        );
    }
    let ports = registry.private_ports()?;
    let router = router(registry);
    let addresses: Vec<_> = match ports {
        Some(range) => (range.first..=range.last)
            .map(|port| std::net::SocketAddr::new(address.ip(), port))
            .collect(),
        None => vec![address],
    };
    // Bind all configured ports before serving any of them; conflicts fail startup.
    let mut listeners = vec![];
    for address in addresses {
        listeners.push(tokio::net::TcpListener::bind(address).await?);
    }
    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let router = router.clone();
        tasks.spawn(async move { axum::serve(listener, router).await });
    }
    let result = tasks.join_next().await.ok_or("No private listeners")?;
    tasks.abort_all();
    result??;
    Err("Private reader stopped unexpectedly".into())
}
