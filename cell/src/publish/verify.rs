//! Verify the actual configured HTTPS URLs, not an internal listener. These
//! checks establish served bytes and access boundaries, not browser rendering.
use super::{
    registry::{Registry, RegistryError},
    transport::SignedRequest,
    PublicationReceipt, Visibility,
};
use serde::{Deserialize, Serialize};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    pub publication: PublicationReceipt,
    pub checked_files: usize,
    pub checked_at: u64,
    pub error: Option<String>,
}
fn get(
    url: &str,
    proof: Option<&str>,
    limit: usize,
    timeout: u64,
) -> Result<(u16, Vec<u8>), String> {
    // No redirects: a login page or a route on another host cannot pass.
    let mut command = Command::new("curl");
    command.args([
        "--disable",
        "--proto",
        "=https",
        "--silent",
        "--show-error",
        "--connect-timeout",
        "5",
        "--max-time",
        &timeout.to_string(),
        "--max-filesize",
        &(limit.max(65536)).to_string(),
        "--write-out",
        "\n%{http_code}",
    ]);
    if let Some(proof) = proof {
        command.args(["--header", &format!("x-cell-site-verification: {proof}")]);
        // Optional VM-scoped ingress credential, readable only by the trusted
        // publisher. Never send it on anonymous checks or return it to a cell.
        if let Some(path) = std::env::var_os("CELL_PUBLISH_INGRESS_HEADERS") {
            let path = std::path::PathBuf::from(path);
            use std::os::unix::fs::PermissionsExt;
            let meta =
                std::fs::symlink_metadata(&path).map_err(|_| "Ingress credential unavailable")?;
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err("Ingress credential must be a private regular file".into());
            }
            command.arg("--header").arg(format!("@{}", path.display()));
        }
    }
    let output = command.arg(url).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("HTTPS request failed or timed out".into());
    }
    let split = output
        .stdout
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or("Missing HTTP status")?;
    let status = std::str::from_utf8(&output.stdout[split + 1..])
        .map_err(|_| "Invalid HTTP status")?
        .parse()
        .map_err(|_| "Invalid HTTP status")?;
    Ok((status, output.stdout[..split].to_vec()))
}
pub fn run(registry: &Registry, request: &SignedRequest) -> Result<Verification, RegistryError> {
    run_with(registry, request, get)
}
fn run_with(
    registry: &Registry,
    request: &SignedRequest,
    mut fetch: impl FnMut(&str, Option<&str>, usize, u64) -> Result<(u16, Vec<u8>), String>,
) -> Result<Verification, RegistryError> {
    let (publication, artifact) = registry.verification_snapshot(request)?;
    let mut result = Verification {
        publication,
        checked_files: 0,
        checked_at: super::transport::unix_time()?,
        error: None,
    };
    let proof =
        serde_json::to_string(request).map_err(|e| RegistryError::InvalidState(e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(50);
    let check = (|| -> Result<(), String> {
        let base = result.publication.url.as_str();
        // Check the exact URL returned to the user, including index routing.
        let root = artifact
            .files
            .iter()
            .find(|f| f.path == "index.html")
            .ok_or("Missing index.html")?;
        for (url, expected) in std::iter::once((base.to_owned(), root.bytes.as_slice())).chain(
            artifact
                .files
                .iter()
                .map(|file| (format!("{base}{}", file.path), file.bytes.as_slice())),
        ) {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
            if remaining == 0 {
                return Err("External verification timed out".into());
            }
            if result.publication.visibility == Visibility::Private {
                let (status, _) = fetch(&url, None, 65536, remaining.min(10))?;
                if !matches!(status, 401 | 403 | 302 | 303 | 307 | 308) {
                    return Err(format!("Private URL did not require authentication: {url}"));
                }
            }
            let private_proof =
                (result.publication.visibility == Visibility::Private).then_some(proof.as_str());
            let (status, bytes) = fetch(&url, private_proof, expected.len(), remaining.min(10))?;
            if status != 200 {
                return Err(format!("Expected HTTP 200, received {status} at {url}"));
            }
            if bytes != expected {
                return Err(format!(
                    "Served content does not match this version at {url}"
                ));
            }
            // Check again after the authenticated response: an ingress cache
            // must not make those bytes available to the next anonymous reader.
            if result.publication.visibility == Visibility::Private {
                let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
                if remaining == 0 {
                    return Err("External verification timed out".into());
                }
                let (status, _) = fetch(&url, None, 65536, remaining.min(10))?;
                if !matches!(status, 401 | 403 | 302 | 303 | 307 | 308) {
                    return Err(format!(
                        "Private URL became readable without authentication: {url}"
                    ));
                }
            }
            if url != base {
                result.checked_files += 1;
            }
        }
        if registry
            .verification_snapshot(request)
            .map_err(|e| e.to_string())?
            .0
            != result.publication
        {
            return Err("Site changed during verification; verify the current version".into());
        }
        Ok(())
    })();
    result.error = check.err();
    registry.record_verification(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{
        artifact::{Artifact, Entry},
        private,
        provision::PolicyCell,
        registry::Registry,
        transport::{self, Command, Reply, SignedRequest},
        PublishScope, SiteId,
    };
    use crate::resource::ProviderRef;
    use crate::{Cell, Grant, RequestId, Secret};
    #[test]
    fn external_verification_rejects_login_pages_missing_assets_wrong_bytes_and_anonymous_private_access(
    ) {
        let root =
            std::env::temp_dir().join(format!("site-verification-{}", RequestId::generate()));
        let registry = Registry::create(&root).unwrap();
        let secret = Secret::generate();
        let audience = registry.audience().unwrap();
        registry
            .configure_private(private::Config {
                domain: "private.example".parse().unwrap(),
                ports: None,
                users: ["reader".into()].into(),
            })
            .unwrap();
        registry
            .reconcile_publishing(
                ProviderRef(audience),
                "https://publisher.example".parse().unwrap(),
                vec![PolicyCell {
                    cell: Cell::new(
                        "author".parse().unwrap(),
                        [
                            Grant::Publish(PublishScope::Private),
                            Grant::Host("publisher.example".parse().unwrap()),
                        ],
                    ),
                    parent: None,
                    key: secret.public(),
                }],
            )
            .unwrap();
        let site = SiteId {
            namespace: private::namespace(audience, secret.public()),
            name: "demo".parse().unwrap(),
        };
        let sign = |command| {
            SignedRequest::sign(
                &secret,
                audience,
                RequestId::generate(),
                transport::unix_time().unwrap() + 240,
                command,
            )
        };
        let artifact = Artifact {
            files: vec![
                Entry {
                    path: "index.html".into(),
                    bytes: b"intended page".to_vec(),
                },
                Entry {
                    path: "values.json".into(),
                    bytes: b"private data".to_vec(),
                },
            ],
        };
        let Reply::Published(receipt) = registry
            .execute_upload(
                &sign(Command::PublishPrivate {
                    site: site.clone(),
                    artifact: artifact.hash().unwrap(),
                }),
                &artifact,
            )
            .unwrap()
        else {
            panic!()
        };
        let proof = sign(Command::Verify { site: site.clone() });
        for failure in ["login", "missing", "wrong", "anonymous", "redirect"] {
            let mut cached = false;
            let result = run_with(&registry, &proof, |_, header, _, _| {
                if header.is_some() {
                    cached = true;
                    return Ok((200, b"intended page".to_vec()));
                }
                Ok(if cached {
                    (200, b"intended page".to_vec())
                } else {
                    (403, vec![])
                })
            })
            .unwrap();
            assert!(result.error.unwrap().contains("became readable"));
            let result = run_with(&registry, &proof, |url, header, _, _| {
                if header.is_none() {
                    return Ok((if failure == "anonymous" { 200 } else { 403 }, vec![]));
                }
                assert_eq!(
                    serde_json::from_str::<SignedRequest>(header.unwrap())
                        .unwrap()
                        .verify()
                        .unwrap()
                        .command
                        .site(),
                    &site
                );
                if failure == "login" {
                    return Ok((200, b"Login page".to_vec()));
                }
                if failure == "redirect" {
                    return Ok((302, vec![]));
                }
                if url.ends_with("values.json") {
                    return Ok(match failure {
                        "missing" => (404, vec![]),
                        "wrong" => (200, b"old data".to_vec()),
                        _ => (200, b"private data".to_vec()),
                    });
                }
                Ok((200, b"intended page".to_vec()))
            })
            .unwrap();
            assert!(result.error.is_some(), "{failure}");
            assert!(registry.resources().unwrap().sites[0]
                .verification
                .as_ref()
                .unwrap()
                .error
                .is_some());
        }
        let result = run_with(&registry, &proof, |url, header, _, _| {
            Ok(if header.is_none() {
                (403, vec![])
            } else if url.ends_with("values.json") {
                (200, b"private data".to_vec())
            } else {
                (200, b"intended page".to_vec())
            })
        })
        .unwrap();
        assert_eq!(result.error, None);
        assert_eq!(result.checked_files, 2);
        assert_eq!(result.publication, receipt);
        assert_eq!(
            registry.resources().unwrap().sites[0].verification,
            Some(result)
        );
        registry
            .execute_upload(
                &sign(Command::PublishPrivate {
                    site,
                    artifact: artifact.hash().unwrap(),
                }),
                &artifact,
            )
            .unwrap();
        assert!(registry.resources().unwrap().sites[0]
            .verification
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
