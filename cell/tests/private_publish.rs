use cell::publish::{
    artifact::{Artifact, Entry},
    private,
    provision::PolicyCell,
    registry::{Registry, RegistryError},
    transport::{self, Command, Reply, SignedRequest},
    PublishScope, SiteId, Visibility,
};
use cell::resource::{
    CellIdentity, ProviderRef, ResourceIntent, ResourceOwner, TransferVisibility,
};
use cell::{Cell, Grant, RequestId, Secret, Tree};
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    registry: Registry,
    owner: Secret,
    child: Secret,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("private-publish-{}", RequestId::generate()));
        let registry = Registry::create(&root).unwrap();
        registry
            .configure_private(private::Config {
                domain: "private.example".parse().unwrap(),
                users: ["reader".into()].into(),
            })
            .unwrap();
        let f = Self {
            root,
            registry,
            owner: Secret::generate(),
            child: Secret::generate(),
        };
        f.sync(true);
        f
    }
    fn cells(&self, child_grant: bool) -> Vec<PolicyCell> {
        let grants = [
            Grant::Publish(PublishScope::Private),
            Grant::Host("publisher.example".parse().unwrap()),
        ];
        vec![
            PolicyCell {
                cell: Cell::new("parent".parse().unwrap(), grants.clone()),
                parent: None,
                key: self.owner.public(),
            },
            PolicyCell {
                cell: Cell::new(
                    "child".parse().unwrap(),
                    if child_grant { grants.to_vec() } else { vec![] },
                ),
                parent: Some("parent".parse().unwrap()),
                key: self.child.public(),
            },
        ]
    }
    fn sync(
        &self,
        child_grant: bool,
    ) -> std::collections::BTreeMap<cell::Name, cell::publish::provision::Discovery> {
        self.registry
            .reconcile_publishing(
                ProviderRef(self.registry.audience().unwrap()),
                "https://publisher.example".parse().unwrap(),
                self.cells(child_grant),
            )
            .unwrap()
    }
    fn site(&self, child: bool, name: &str) -> SiteId {
        SiteId {
            namespace: private::namespace(
                self.registry.audience().unwrap(),
                if child {
                    self.child.public()
                } else {
                    self.owner.public()
                },
            ),
            name: name.parse().unwrap(),
        }
    }
    fn sign(&self, child: bool, command: Command) -> SignedRequest {
        SignedRequest::sign(
            if child { &self.child } else { &self.owner },
            self.registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            command,
        )
    }
    fn artifact() -> Artifact {
        Artifact { files: vec![Entry { path: "index.html".into(), bytes: b"<!doctype html><h1>Private</h1><script type=module src=./main.js></script>".to_vec() }, Entry { path: "main.js".into(), bytes: b"fetch('./values.json').then(r=>r.json()).then(x=>document.querySelector('h1').textContent=x.title)".to_vec() }, Entry { path: "values.json".into(), bytes: br#"{"title":"Modules and private fetch work"}"#.to_vec() }] }
    }
    fn publish(&self, child: bool, name: &str) -> cell::publish::PublicationReceipt {
        let artifact = Self::artifact();
        match self
            .registry
            .execute_upload(
                &self.sign(
                    child,
                    Command::PublishPrivate {
                        site: self.site(child, name),
                        artifact: artifact.hash().unwrap(),
                    },
                ),
                &artifact,
            )
            .unwrap()
        {
            Reply::Published(receipt) => receipt,
            _ => panic!(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn provisioning_is_idempotent_and_sites_are_existing_resources_owned_by_pinned_keys() {
    let f = Fixture::new();
    let initial = f.sync(true);
    let receipt = f.publish(true, "pilot");
    let again = f.sync(true);
    let child: cell::Name = "child".parse().unwrap();
    assert_eq!(initial[&child].config, again[&child].config);
    assert_eq!(receipt.visibility, Visibility::Private);
    assert_eq!(again[&child].sites[0].publication, receipt);
    let inventory = f.registry.resources().unwrap();
    assert_eq!(inventory.resources.len(), 1);
    assert_eq!(
        inventory.resources[0].owner,
        ResourceOwner::Cell(CellIdentity(f.child.public()))
    );
    assert_eq!(inventory.sites[0].resource, inventory.resources[0].id);
    let mut renamed = f.cells(true);
    renamed[1].cell.name = "new-binding".parse().unwrap();
    let result = f
        .registry
        .reconcile_publishing(
            inventory.provider,
            "https://publisher.example".parse().unwrap(),
            renamed,
        )
        .unwrap();
    assert_eq!(
        result[&"new-binding".parse().unwrap()].sites[0]
            .publication
            .url,
        receipt.url
    );
    assert_eq!(
        Registry::open(&f.root).unwrap().resources().unwrap().sites[0]
            .publication
            .url,
        receipt.url
    );
}

#[test]
fn private_permission_never_creates_public_sites_or_reads_other_cells_sites() {
    let f = Fixture::new();
    let receipt = f.publish(true, "pilot");
    let artifact = Fixture::artifact();
    assert!(matches!(
        f.registry.execute_upload(
            &f.sign(
                true,
                Command::Publish {
                    site: f.site(true, "public"),
                    artifact: artifact.hash().unwrap()
                }
            ),
            &artifact
        ),
        Err(RegistryError::Denied)
    ));
    assert!(f.registry.public_file(&receipt.site, "index.html").is_err());
    assert!(f
        .registry
        .private_file(receipt.url.host().as_str(), "index.html", None, None)
        .is_err());
    let wrong = f.sign(
        false,
        Command::Verify {
            site: receipt.site.clone(),
        },
    );
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            None,
            Some(&wrong)
        )
        .is_err());
    let proof = f.sign(
        true,
        Command::Verify {
            site: receipt.site.clone(),
        },
    );
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            None,
            Some(&proof)
        )
        .is_ok());
    f.sync(false);
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            None,
            Some(&proof)
        )
        .is_err());
    // Reader access survives author revocation, like the resource itself.
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            Some("reader"),
            None
        )
        .is_ok());
}

#[test]
fn parent_containment_and_backend_identity_are_checked_before_any_policy_change() {
    let f = Fixture::new();
    let before = f.registry.resources().unwrap();
    let mut cells = f.cells(true);
    cells[0].cell.grants.clear();
    assert!(f
        .registry
        .reconcile_publishing(
            before.provider,
            "https://publisher.example".parse().unwrap(),
            cells
        )
        .is_err());
    assert!(f
        .registry
        .reconcile_publishing(
            ProviderRef(RequestId::generate()),
            "https://publisher.example".parse().unwrap(),
            f.cells(true)
        )
        .is_err());
    assert_eq!(f.registry.resources().unwrap(), before);
    let mut cells = f.cells(true);
    cells[1]
        .cell
        .grants
        .retain(|g| !matches!(g, Grant::Host(_)));
    let discovery = f
        .registry
        .reconcile_publishing(
            before.provider,
            "https://publisher.example".parse().unwrap(),
            cells,
        )
        .unwrap();
    assert!(discovery[&"child".parse().unwrap()].config.is_none());
    assert!(discovery[&"child".parse().unwrap()]
        .error
        .as_ref()
        .unwrap()
        .contains("publisher.example"));
}

#[test]
fn transfer_and_cleanup_preserve_private_resource_lifecycle_and_prevent_recapture() {
    let f = Fixture::new();
    let receipt = f.publish(true, "pilot");
    let record = f.registry.resources().unwrap().resources.remove(0);
    let plan = f
        .registry
        .plan_resource(
            record.id,
            ResourceIntent::Transfer {
                to: ResourceOwner::Operator,
                visibility: TransferVisibility::KeepCurrent,
            },
        )
        .unwrap();
    f.registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .unwrap();
    f.sync(true);
    assert!(f
        .registry
        .execute(&f.sign(
            true,
            Command::Unpublish {
                site: receipt.site.clone()
            }
        ))
        .is_err());
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            Some("reader"),
            None
        )
        .is_ok());
    let plan = f
        .registry
        .plan_resource(record.id, ResourceIntent::Unpublish)
        .unwrap();
    f.registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .unwrap();
    assert!(f
        .registry
        .private_file(
            receipt.url.host().as_str(),
            "index.html",
            Some("reader"),
            None
        )
        .is_err());
    assert!(f.registry.resources().unwrap().sites.is_empty());
}

#[test]
fn old_public_receipts_and_urls_survive_upgrade_and_visibility_cannot_change() {
    let f = Fixture::new();
    let site = f.site(true, "public");
    let grant = Grant::Publish(PublishScope::Namespace(site.namespace.clone()));
    f.registry
        .approve_tree(
            &Tree::new([(Cell::new("child".parse().unwrap(), [grant]), None)]).unwrap(),
            &[("child".parse().unwrap(), f.child.public())].into(),
        )
        .unwrap();
    f.registry
        .configure_public_base("https://public.example".parse().unwrap())
        .unwrap();
    let artifact = Fixture::artifact();
    let Reply::Published(receipt) = f
        .registry
        .execute_upload(
            &f.sign(
                true,
                Command::Publish {
                    site: site.clone(),
                    artifact: artifact.hash().unwrap(),
                },
            ),
            &artifact,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        receipt.url.as_str(),
        format!("https://public.example/sites/{}/public/", site.namespace.0)
    );
    let path = f.root.join("state.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for history in old["publications"].as_object_mut().unwrap().values_mut() {
        for receipt in history["versions"].as_object_mut().unwrap().values_mut() {
            receipt.as_object_mut().unwrap().remove("visibility");
        }
    }
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(Registry::open(&f.root)
        .unwrap()
        .public_file(&site, "index.html")
        .is_ok());
    assert!(f
        .registry
        .execute_upload(
            &f.sign(
                true,
                Command::PublishPrivate {
                    site,
                    artifact: artifact.hash().unwrap()
                }
            ),
            &artifact
        )
        .is_err());
}

async fn get(address: std::net::SocketAddr, host: String, extra: String, path: &str) -> String {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read,Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(stream,"GET {path} HTTP/1.1\r\nHost: {host}\r\nX-Forwarded-Host: {host}\r\nX-Forwarded-Proto: https\r\n{extra}Connection: close\r\n\r\n").unwrap();
        let mut data = String::new();stream.read_to_string(&mut data).unwrap();data
    }).await.unwrap()
}
#[tokio::test]
async fn private_http_enforces_reader_identity_origin_and_signed_verification_scope() {
    let f = Fixture::new();
    let receipt = f.publish(true, "pilot");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = private::router(Registry::open(&f.root).unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let host = receipt.url.host().to_string();
    for headers in [
        "".to_owned(),
        "X-ExeDev-UserID: outsider\r\n".into(),
        "X-ExeDev-UserID: reader\r\nX-ExeDev-UserID: reader\r\n".into(),
        "X-ExeDev-UserID: reader\r\nOrigin: https://evil.example\r\n".into(),
        "X-ExeDev-UserID: reader\r\nSec-Fetch-Site: cross-site\r\n".into(),
    ] {
        let response = get(address, host.clone(), headers, "/").await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("<h1>Private"));
    }
    let response = get(
        address,
        host.clone(),
        "X-ExeDev-UserID: reader\r\n".into(),
        "/",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("private, no-store"));
    assert!(!response.contains("access-control-allow-origin"));
    assert!(response.contains("worker-src 'none'"));
    let proof = f.sign(
        true,
        Command::Verify {
            site: receipt.site.clone(),
        },
    );
    let headers = format!(
        "X-Cell-Site-Verification: {}\r\n",
        serde_json::to_string(&proof).unwrap()
    );
    assert!(get(address, host.clone(), headers.clone(), "/values.json")
        .await
        .starts_with("HTTP/1.1 200"));
    for path in ["/state.json", "/v1/requests", "/../state.json"] {
        assert!(get(address, host.clone(), headers.clone(), path)
            .await
            .starts_with("HTTP/1.1 404"));
    }
    f.registry
        .execute(&f.sign(true, Command::Unpublish { site: receipt.site }))
        .unwrap();
    assert!(get(address, host, headers, "/")
        .await
        .starts_with("HTTP/1.1 404"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires KUGGIS_TEST_PLAYWRIGHT and KUGGIS_TEST_BROWSER"]
async fn private_sites_in_a_browser() {
    let f = Fixture::new();
    let first = f.publish(true, "first");
    let second = f.publish(false, "second");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = private::router(Registry::open(&f.root).unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let output = tokio::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scripts/test-private-sites-browser.mjs"
        ))
        .arg(address.to_string())
        .arg(first.url.as_str())
        .arg(second.url.as_str())
        .output()
        .await
        .unwrap();
    task.abort();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn private_permission_cannot_restore_a_public_version_after_public_authority_is_revoked() {
    let f = Fixture::new();
    let site = f.site(true, "public");
    let grant = Grant::Publish(PublishScope::Namespace(site.namespace.clone()));
    f.registry
        .approve_tree(
            &Tree::new([(Cell::new("child".parse().unwrap(), [grant]), None)]).unwrap(),
            &[("child".parse().unwrap(), f.child.public())].into(),
        )
        .unwrap();
    f.registry
        .configure_public_base("https://public.example".parse().unwrap())
        .unwrap();
    let artifact = Fixture::artifact();
    let Reply::Published(receipt) = f
        .registry
        .execute_upload(
            &f.sign(
                true,
                Command::Publish {
                    site: site.clone(),
                    artifact: artifact.hash().unwrap(),
                },
            ),
            &artifact,
        )
        .unwrap()
    else {
        panic!()
    };
    f.registry
        .execute(&f.sign(true, Command::Unpublish { site: site.clone() }))
        .unwrap();
    f.sync(true);
    assert!(f
        .registry
        .execute(&f.sign(
            true,
            Command::Rollback {
                site: site.clone(),
                version: receipt.version
            }
        ))
        .is_err());
    assert!(f.registry.public_file(&site, "index.html").is_err());
}

#[test]
fn automatic_policy_updates_do_not_restore_an_explicitly_detached_site() {
    let f = Fixture::new();
    let first = f.publish(true, "first");
    let second = f.publish(true, "second");
    let mut cells = f.cells(true);
    cells[0]
        .cell
        .grants
        .insert(Grant::Publish(PublishScope::Site(first.site.clone())));
    let sync = |cells| {
        f.registry
            .reconcile_publishing(
                ProviderRef(f.registry.audience().unwrap()),
                "https://publisher.example".parse().unwrap(),
                cells,
            )
            .unwrap()
    };
    sync(cells.clone());
    let inventory = f.registry.resources().unwrap();
    let id = inventory
        .sites
        .iter()
        .find(|s| s.publication.site == first.site)
        .unwrap()
        .resource;
    assert!(inventory
        .resources
        .iter()
        .find(|r| r.id == id)
        .unwrap()
        .attachments
        .iter()
        .any(|a| a.cell.0 == f.owner.public()));
    let plan = f
        .registry
        .plan_resource(
            id,
            ResourceIntent::Detach {
                cell: CellIdentity(f.owner.public()),
            },
        )
        .unwrap();
    f.registry
        .apply_resource(RequestId::generate(), &plan, plan.approval())
        .unwrap();
    cells[0]
        .cell
        .grants
        .insert(Grant::Publish(PublishScope::Site(second.site)));
    sync(cells);
    assert!(f
        .registry
        .resources()
        .unwrap()
        .resources
        .iter()
        .find(|r| r.id == id)
        .unwrap()
        .attachments
        .is_empty());
    assert!(f
        .registry
        .verification_snapshot(&f.sign(false, Command::Verify { site: first.site }))
        .is_err());
}
