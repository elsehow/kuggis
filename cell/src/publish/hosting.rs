//! Public read-only surface, deliberately separate from the signed control API.
//! Serving traverses no agent paths: only registry-selected artifact bytes.
use super::{registry::Registry, SiteId};
use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use std::sync::Arc;

pub fn router(registry: Registry) -> Router {
    Router::new()
        .route("/sites/:namespace/:name", get(redirect))
        .route("/sites/:namespace/:name/", get(index))
        .route("/sites/:namespace/:name/*path", get(file))
        .with_state(Arc::new(registry))
}
async fn redirect(Path((namespace, name)): Path<(String, String)>) -> Response {
    match site(&namespace, &name) {
        Some(site) => Redirect::temporary(&format!("/sites/{}/{}/", site.namespace.0, site.name))
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
fn site(namespace: &str, name: &str) -> Option<SiteId> {
    Some(SiteId {
        namespace: super::NamespaceId(namespace.parse().ok()?),
        name: name.parse().ok()?,
    })
}
async fn index(
    State(registry): State<Arc<Registry>>,
    Path((ns, name)): Path<(String, String)>,
) -> Response {
    content(registry, ns, name, "index.html".into()).await
}
async fn file(
    State(registry): State<Arc<Registry>>,
    Path((ns, name, path)): Path<(String, String, String)>,
) -> Response {
    let path = if path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path
    };
    content(registry, ns, name, path).await
}
async fn content(registry: Arc<Registry>, ns: String, name: String, path: String) -> Response {
    let Some(site) = site(&ns, &name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime(&path);
    let bytes = tokio::task::spawn_blocking(move || registry.public_file(&site, &path)).await;
    let mut response = match bytes {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Ok(Err(super::registry::RegistryError::UnknownSite)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    // Path-based sites share a host, but NOT an executable browser origin.
    // In particular, no same-origin sandbox escape or service-worker takeover.
    headers.insert("content-security-policy", HeaderValue::from_static("sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; media-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    // Modules and fetch in sandboxed (opaque-origin) pages need anonymous CORS.
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}
pub(super) fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
pub async fn serve(registry: Registry, address: std::net::SocketAddr) -> crate::apply::Fallible {
    if !address.ip().is_loopback() {
        return Err("public artifact server must bind loopback behind HTTPS".into());
    }
    axum::serve(
        tokio::net::TcpListener::bind(address).await?,
        router(registry),
    )
    .await?;
    Ok(())
}
