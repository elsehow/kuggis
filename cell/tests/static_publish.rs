#[path = "fixtures/common.rs"]
mod fixtures;
use cell::publish::{
    artifact::{Artifact, Entry},
    hosting,
    registry::{Registry, RegistryError},
    transport::{self, Command, Reply, SignedRequest, Upload},
    PublishScope, SiteId,
};
use cell::{Grant, RequestId, Secret, Tree};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    registry: Registry,
    secret: Secret,
    site: SiteId,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cell-static-{}", RequestId::generate()));
        fs::create_dir(&root).unwrap();
        let registry = Registry::create(&root.join("registry")).unwrap();
        let secret = Secret::generate();
        let ns = registry.allocate_namespace(secret.public()).unwrap();
        let cell = fixtures::cell(
            "489d239e-1b05-546d-a0ca-abd828de38b9".parse().unwrap(),
            [Grant::Publish(PublishScope::Namespace(ns.clone()))],
        );
        registry
            .approve_tree(
                &Tree::new([(cell.clone(), None)]).unwrap(),
                &[(cell.id, secret.public())].into(),
            )
            .unwrap();
        registry
            .configure_public_base("https://sites.example.com/".parse().unwrap())
            .unwrap();
        Self {
            root,
            registry,
            secret,
            site: SiteId {
                namespace: ns,
                name: "demo".parse().unwrap(),
            },
        }
    }
    fn sign(&self, command: Command) -> SignedRequest {
        SignedRequest::sign(
            &self.secret,
            self.registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            command,
        )
    }
    fn upload(&self, artifact: &Artifact) -> SignedRequest {
        self.sign(Command::Publish {
            site: self.site.clone(),
            artifact: artifact.hash().unwrap(),
        })
    }
    fn publish(&self, artifact: &Artifact) -> cell::publish::PublicationReceipt {
        match self
            .registry
            .execute_upload(&self.upload(artifact), artifact)
            .unwrap()
        {
            Reply::Published(receipt) => receipt,
            other => panic!("{other:?}"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn artifact(html: &str) -> Artifact {
    Artifact {
        files: vec![Entry {
            path: "index.html".into(),
            bytes: html.as_bytes().to_vec(),
        }],
    }
}

#[test]
fn artifacts_reject_traversal_duplicates_hidden_files_and_resource_exhaustion() {
    for bad in [
        "../secret",
        "/secret",
        "904254dc-1b8f-5e80-9755-e5afc5b195c5/../b",
        "904254dc-1b8f-5e80-9755-e5afc5b195c5\\b",
        "904254dc-1b8f-5e80-9755-e5afc5b195c5//b",
        ".env",
        ".git/config",
        "x/%2e%2e/904254dc-1b8f-5e80-9755-e5afc5b195c5",
        "x?y",
        "ü",
        "",
    ] {
        let mut artifact = artifact("ok");
        artifact.files.push(Entry {
            path: bad.into(),
            bytes: vec![],
        });
        artifact.files.sort_by(|a, b| a.path.cmp(&b.path));
        assert!(artifact.validate().is_err(), "{bad}");
    }
    let mut duplicate = artifact("ok");
    duplicate.files.push(duplicate.files[0].clone());
    assert!(duplicate.validate().is_err());
    let mut conflict = artifact("ok");
    conflict.files.push(Entry {
        path: "index.html/e3e321ad-3f5e-5221-9ec8-ccc87947084b".into(),
        bytes: vec![],
    });
    assert!(conflict.validate().is_err());
    let mut large = artifact("ok");
    large.files[0].bytes = vec![0; cell::publish::artifact::MAX_BYTES + 1];
    assert!(large.validate().is_err());
    assert!(Artifact { files: vec![] }.validate().is_err());
    let mut many = artifact("ok");
    for i in 0..512 {
        many.files.push(Entry {
            path: format!("z{i:04}"),
            bytes: vec![],
        });
    }
    assert!(many.validate().is_err());
}

// The packer walks directories through /proc/self/fd, which only Linux has.
#[cfg(target_os = "linux")]
#[test]
fn snapshot_never_follows_links_or_special_files_and_copies_only_selected_content() {
    let f = Fixture::new();
    let dist = f.root.join("dist");
    fs::create_dir(&dist).unwrap();
    fs::write(dist.join("index.html"), "first").unwrap();
    fs::write(f.root.join("private.key"), "not public").unwrap();
    let snapshot = Artifact::snapshot(&dist).unwrap();
    fs::write(dist.join("index.html"), "second").unwrap();
    assert_eq!(snapshot.files[0].bytes, b"first");
    assert_eq!(snapshot.files.len(), 1);
    let link = dist.join("leak");
    std::os::unix::fs::symlink(f.root.join("private.key"), &link).unwrap();
    assert!(Artifact::snapshot(&dist).is_err());
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&f.root, &link).unwrap();
    assert!(Artifact::snapshot(&dist).is_err());
    fs::remove_file(&link).unwrap();
    let fifo = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(Artifact::snapshot(&dist).is_err()); // must not block opening the FIFO
    fs::remove_file(&link).unwrap();
    fs::write(dist.join(".env"), "secret").unwrap();
    assert!(Artifact::snapshot(&dist).is_err());
}

#[test]
fn publication_versions_are_immutable_and_rollback_unpublish_survive_restart() {
    let f = Fixture::new();
    let first = f.publish(&artifact("one"));
    let second = f.publish(&artifact("two"));
    assert_ne!(first.version, second.version);
    assert_eq!(first.url, second.url);
    let restarted = Registry::open(&f.root.join("registry")).unwrap();
    assert_eq!(
        restarted.public_file(&f.site, "index.html").unwrap(),
        b"two"
    );
    let rollback = f.sign(Command::Rollback {
        site: f.site.clone(),
        version: first.version,
    });
    assert!(matches!(restarted.execute(&rollback).unwrap(), Reply::RolledBack(r) if r == first));
    assert_eq!(
        restarted.public_file(&f.site, "index.html").unwrap(),
        b"one"
    );
    restarted
        .execute(&f.sign(Command::Unpublish {
            site: f.site.clone(),
        }))
        .unwrap();
    assert!(restarted.public_file(&f.site, "index.html").is_err());
    assert_eq!(
        restarted.owner(&f.site.namespace).unwrap(),
        f.secret.public()
    );
    assert!(matches!(
        restarted.create_site(f.secret.public(), &f.site),
        Err(RegistryError::SiteExists)
    ));
    restarted
        .execute(&f.sign(Command::Rollback {
            site: f.site.clone(),
            version: second.version,
        }))
        .unwrap();
    assert_eq!(
        restarted.public_file(&f.site, "index.html").unwrap(),
        b"two"
    );
    assert!(restarted
        .configure_public_base("https://different.example.com/".parse().unwrap())
        .is_err());
    assert!(restarted.public_file(&f.site, "../state.json").is_err());
    match restarted
        .execute(&f.sign(Command::Status {
            site: f.site.clone(),
        }))
        .unwrap()
    {
        Reply::Status {
            active, versions, ..
        } => {
            assert_eq!(active.unwrap(), second);
            assert_eq!(versions.len(), 2);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn mismatched_artifact_replay_revocation_and_foreign_versions_cannot_change_live_site() {
    let f = Fixture::new();
    f.publish(&artifact("original"));
    let intended = artifact("intended");
    let request = f.upload(&intended);
    assert!(f
        .registry
        .execute_upload(&request, &artifact("tampered"))
        .is_err());
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"original"
    );
    f.registry.execute_upload(&request, &intended).unwrap();
    assert!(matches!(
        f.registry.execute_upload(&request, &intended),
        Err(RegistryError::Replay)
    ));
    let other_site = SiteId {
        namespace: f.site.namespace.clone(),
        name: "70ee78ad-5e3a-510a-af20-6f485ecb6f16".parse().unwrap(),
    };
    let other_command = f.sign(Command::Publish {
        site: other_site.clone(),
        artifact: intended.hash().unwrap(),
    });
    let other_receipt = match f
        .registry
        .execute_upload(&other_command, &intended)
        .unwrap()
    {
        Reply::Published(r) => r,
        _ => panic!(),
    };
    assert!(f
        .registry
        .execute(&f.sign(Command::Rollback {
            site: f.site.clone(),
            version: other_receipt.version
        }))
        .is_err());
    let stranger = Secret::generate();
    let steal = SignedRequest::sign(
        &stranger,
        f.registry.audience().unwrap(),
        RequestId::generate(),
        transport::unix_time().unwrap() + 240,
        Command::Unpublish {
            site: f.site.clone(),
        },
    );
    assert!(matches!(
        f.registry.execute(&steal),
        Err(RegistryError::Denied)
    ));
    let request = f.upload(&artifact("after revocation"));
    f.registry
        .approve_tree(&Tree::new([]).unwrap(), &Default::default())
        .unwrap();
    assert!(matches!(
        f.registry
            .execute_upload(&request, &artifact("after revocation")),
        Err(RegistryError::Denied)
    ));
    assert_eq!(
        f.registry.public_file(&f.site, "index.html").unwrap(),
        b"intended"
    );
}

async fn http(address: std::net::SocketAddr, method: &str, path: &str, body: Vec<u8>) -> Vec<u8> {
    let method = method.to_owned();
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let mut result = vec![]; stream.read_to_end(&mut result).unwrap(); result
    }).await.unwrap()
}
#[tokio::test]
async fn real_http_upload_and_public_serving_are_separate_and_never_expose_state_or_old_blobs() {
    let f = Fixture::new();
    let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control.local_addr().unwrap();
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = public.local_addr().unwrap();
    let registry = Registry::open(&f.root.join("registry")).unwrap();
    let control_task = tokio::spawn(async move {
        axum::serve(control, transport::router(registry))
            .await
            .unwrap()
    });
    let registry = Registry::open(&f.root.join("registry")).unwrap();
    let public_task = tokio::spawn(async move {
        axum::serve(public, hosting::router(registry))
            .await
            .unwrap()
    });
    // Larger than the control-request cap: upload routing has its own limit.
    let artifact = artifact(&"x".repeat(20_000));
    let upload = Upload {
        request: f.upload(&artifact),
        artifact,
    };
    let reply = http(
        control_addr,
        "POST",
        "/v1/uploads",
        serde_json::to_vec(&upload).unwrap(),
    )
    .await;
    assert!(
        reply.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let path = format!("/sites/{}/{}/", f.site.namespace.0, f.site.name);
    let page = http(public_addr, "GET", &path, vec![]).await;
    assert!(page.starts_with(b"HTTP/1.1 200"));
    let headers = String::from_utf8_lossy(&page);
    assert!(headers.contains("sandbox allow-scripts"));
    assert!(!headers.contains("allow-same-origin"));
    assert!(headers.contains("cache-control: no-store"));
    assert!(headers.contains("access-control-allow-origin: *"));
    assert!(page.ends_with(&vec![b'x'; 20_000]));
    for private in [
        "/state.json".into(),
        "/v1/requests".into(),
        format!("{path}../state.json"),
        format!("/artifacts/{}.json", upload.artifact.hash().unwrap()),
    ] {
        assert!(
            http(public_addr, "GET", &private, vec![])
                .await
                .starts_with(b"HTTP/1.1 404"),
            "{private}"
        );
    }
    assert!(http(control_addr, "GET", &path, vec![])
        .await
        .starts_with(b"HTTP/1.1 404"));
    f.registry
        .execute(&f.sign(Command::Unpublish {
            site: f.site.clone(),
        }))
        .unwrap();
    assert!(http(public_addr, "GET", &path, vec![])
        .await
        .starts_with(b"HTTP/1.1 404"));
    control_task.abort();
    public_task.abort();
}
