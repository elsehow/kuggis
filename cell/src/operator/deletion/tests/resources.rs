use super::*;
use crate::publish::{
    artifact::{Artifact, Entry},
    registry::Registry,
    transport::{self, Command, SignedRequest},
    PublishScope, SiteId,
};
use crate::resource::{CellIdentity, ResourceOwner, ResourceState};

impl Fixture {
    fn site(&self, backend: &str) -> (Registry, SiteId) {
        let registry = Registry::create(&self.dir.join(backend)).unwrap();
        let ns = registry.allocate_namespace(self.secret.public()).unwrap();
        let cell = Cell::new(
            "project-dev".parse().unwrap(),
            [crate::Grant::Publish(PublishScope::Namespace(ns.clone()))],
        );
        registry
            .approve_tree(
                &Tree::new([(cell.clone(), None)]).unwrap(),
                &[(cell.name, self.secret.public())].into(),
            )
            .unwrap();
        registry
            .configure_public_base("https://sites.example.com".parse().unwrap())
            .unwrap();
        let site = SiteId {
            namespace: ns,
            name: "demo".parse().unwrap(),
        };
        self.publish(&registry, &site);
        let path = self.dir.join("tree/.resources.json");
        let mut config: serde_json::Value = fs::read(&path)
            .ok()
            .map(|b| serde_json::from_slice(&b).unwrap())
            .unwrap_or(json!({"backends":[]}));
        config["backends"].as_array_mut().unwrap().push(json!({"name":backend,"provider":registry.audience().unwrap(),"location":{"kind":"local","registry":self.dir.join(backend)}}));
        fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
        (registry, site)
    }
    fn publish(&self, registry: &Registry, site: &SiteId) {
        let artifact = Artifact {
            files: vec![Entry {
                path: "index.html".into(),
                bytes: b"hello".to_vec(),
            }],
        };
        let request = SignedRequest::sign(
            &self.secret,
            registry.audience().unwrap(),
            RequestId::generate(),
            transport::unix_time().unwrap() + 240,
            Command::Publish {
                site: site.clone(),
                artifact: artifact.hash().unwrap(),
            },
        );
        registry.execute_upload(&request, &artifact).unwrap();
    }
    fn assert_cleanup_before_commands(&self, backend: &str, site: &SiteId) {
        let path = self.dir.join(backend);
        let site = site.clone();
        let key = self.secret.public();
        *self.edge.before_command.lock().unwrap() = Some(Box::new(move || {
            let registry = Registry::open(&path).unwrap();
            let inventory = registry.resources().unwrap();
            assert!(inventory.frozen.contains(&CellIdentity(key)));
            assert!(registry.public_file(&site, "index.html").is_err());
            assert_eq!(inventory.resources[0].owner, ResourceOwner::Operator);
        }));
    }
}
#[tokio::test]
async fn sites_are_fenced_unpublished_and_retained_before_any_vm_action() {
    let f = Fixture::new();
    let (registry, site) = f.site("sites");
    f.assert_cleanup_before_commands("sites", &site);
    let job = f.preview("project-dev").await.unwrap();
    assert_eq!(job.view().resources.len(), 1);
    let _ = f.start(&job).await.unwrap();
    let done = f.settled(job.id).await;
    assert_eq!(done.phase, DeletionPhase::Deleted);
    assert_eq!(done.resources_done.len(), 1);
    assert_eq!(
        registry.resources().unwrap().resources[0].state,
        ResourceState::Unpublished
    );
    let request = SignedRequest::sign(
        &f.secret,
        registry.audience().unwrap(),
        RequestId::generate(),
        transport::unix_time().unwrap() + 240,
        Command::Unpublish { site },
    );
    assert!(registry.execute(&request).is_err());
}
#[tokio::test]
async fn drift_offline_missing_backend_and_legacy_approvals_cannot_delete_vm() {
    for case in ["drift", "offline", "missing", "legacy"] {
        let f = Fixture::new();
        let (registry, site) = f.site("sites");
        let mut job = f.preview("project-dev").await.unwrap();
        match case {
            "drift" => f.publish(&registry, &site),
            "offline" => fs::rename(f.dir.join("sites"), f.dir.join("offline")).unwrap(),
            "missing" => fs::remove_file(f.dir.join("tree/.resources.json")).unwrap(),
            "legacy" => {
                job.target.resources = None;
                job.approval = Hash::of(&crate::canon::bytes(&job.target));
                job.save(&f.op.config.state).unwrap();
            }
            _ => unreachable!(),
        }
        let _ = f.start(&job).await.unwrap();
        let failed = f.settled(job.id).await;
        assert!(
            matches!(failed.phase, DeletionPhase::Failed { .. }),
            "{case}"
        );
        assert!(f.edge.calls.lock().unwrap().is_empty(), "{case}");
        assert!(f.dir.join("tree/project/project-dev.json").exists());
        if case == "missing" {
            assert!(f.preview("project-dev").await.is_err());
        }
        if case == "legacy" {
            let updated = f.preview("project-dev").await.unwrap();
            assert!(updated.target.resources.is_some());
            assert!(f.start(&job).await.is_err());
        }
    }
}
#[tokio::test]
async fn publishing_grants_require_inventory_coverage_even_without_configuration() {
    let f = Fixture::new();
    let (_, site) = f.site("sites");
    fs::remove_file(f.dir.join("tree/.resources.json")).unwrap();
    for (name, path) in [
        ("project", "tree/project.json"),
        ("project-dev", "tree/project/project-dev.json"),
    ] {
        let cell = Cell::new(
            name.parse().unwrap(),
            [crate::Grant::Publish(PublishScope::Namespace(
                site.namespace.clone(),
            ))],
        );
        fs::write(f.dir.join(path), cell.canonical()).unwrap();
    }
    assert!(f.preview("project-dev").await.is_err());
    assert!(f.edge.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn keep_live_is_an_explicit_revision_bound_choice() {
    let f = Fixture::new();
    let (registry, site) = f.site("sites");
    let id = registry.resources().unwrap().resources[0].id;
    let view = preview_with(
        f.op.clone(),
        "project-dev".parse().unwrap(),
        Options {
            choices: [(id, ResourceOwner::Operator)].into(),
        },
    )
    .await
    .unwrap()
    .0;
    let job = Deletion::load(&f.op.config.state, view.id).unwrap();
    let _ = f.start(&job).await.unwrap();
    assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
    assert_eq!(registry.public_file(&site, "index.html").unwrap(), b"hello");
    assert_eq!(
        registry.resources().unwrap().resources[0].owner,
        ResourceOwner::Operator
    );
}
#[tokio::test]
async fn partial_cleanup_and_lost_receipt_are_carried_into_new_review() {
    for lose_receipt in [false, true] {
        let f = Fixture::new();
        let (first, _) = f.site("first");
        let (second, site) = f.site("second");
        let mut job = f.preview("project-dev").await.unwrap();
        let binding = job.target.resources.as_ref().unwrap().bindings[0].clone();
        let receipt = resource_cleanup::perform(&f.op, &binding, false).unwrap();
        job.begun = true;
        if !lose_receipt {
            job.resources_done.insert(binding.operation, receipt);
        }
        job.save(&f.op.config.state).unwrap();
        f.publish(&second, &site);
        let _ = f.start(&job).await.unwrap();
        let failed = f.settled(job.id).await;
        assert!(matches!(
            failed.phase,
            DeletionPhase::Failed {
                recovery: RecoveryAction::Review,
                ..
            }
        ));
        assert!(failed.active());
        assert!(f.edge.calls.lock().unwrap().is_empty());
        let new = f.preview("project-dev").await.unwrap();
        assert_ne!(new.id, job.id);
        assert_eq!(new.resources_done.len(), 1);
        assert!(new.active());
        assert!(f.start(&job).await.is_err());
        let _ = f.start(&new).await.unwrap();
        assert_eq!(f.settled(new.id).await.phase, DeletionPhase::Deleted);
        for registry in [first, second] {
            assert_eq!(
                registry.resources().unwrap().resources[0].owner,
                ResourceOwner::Operator
            );
        }
    }
}
#[tokio::test]
async fn completed_resource_cleanup_survives_restart_and_an_offline_backend() {
    let f = Fixture::new();
    let (_, site) = f.site("sites");
    f.assert_cleanup_before_commands("sites", &site);
    f.edge.lose_delete_reply.store(true, Ordering::SeqCst);
    let job = f.preview("project-dev").await.unwrap();
    let _ = f.start(&job).await.unwrap();
    let failed = f.settled(job.id).await;
    assert!(matches!(
        failed.phase,
        DeletionPhase::Failed {
            step: Some(DeletionStep::DeletingVm),
            ..
        }
    ));
    assert_eq!(failed.resources_done.len(), 1);
    *f.edge.before_command.lock().unwrap() = None;
    fs::rename(f.dir.join("sites"), f.dir.join("offline")).unwrap();
    recover(&f.op.config.state).unwrap();
    let _ = f
        .start(&Deletion::load(&f.op.config.state, job.id).unwrap())
        .await
        .unwrap();
    assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
    assert_eq!(
        f.edge
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a[0] == "rm")
            .count(),
        1
    );
}
#[tokio::test]
#[ignore = "requires built web/dist and PUBLISH_SMOKE_PLAYWRIGHT; real HTTP/browser with fake VM edge"]
async fn browser_resource_aware_deletion() {
    let mut f = Fixture::new();
    let (registry, site) = f.site("sites");
    f.assert_cleanup_before_commands("sites", &site);
    let job = f.preview("project-dev").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let op = Arc::get_mut(&mut f.op).unwrap();
    op.authority = addr.to_string();
    op.config.listen = addr;
    op.config.assets = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("web/dist");
    let app = super::super::super::router(f.op.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let script = r#"
const {chromium}=require(process.argv[1]);
(async()=>{const browser=await chromium.launch({headless:true});try{
 const page=await browser.newPage();const origin=new URL(process.argv[2]).origin;
 await page.goto(origin+'/?view=resources');
 await page.getByText('Register an external resource',{exact:true}).click();
 await page.getByRole('textbox',{name:'External provider',exact:true}).fill('runpod');
 await page.getByRole('textbox',{name:'External account',exact:true}).fill('account-a');
 await page.getByRole('textbox',{name:'External resource ID',exact:true}).fill('pod-existing');
 await page.getByRole('button',{name:'Review external registration…'}).click();
 await page.getByRole('heading',{name:'Review resource operation'}).waitFor();await page.reload();
 if(await page.getByRole('button',{name:'Apply resource operation'}).isEnabled())throw new Error('manual verification was not required');
 const approve=async()=>{await page.getByRole('checkbox',{name:/I verified the provider/}).check();await page.getByRole('button',{name:'Apply resource operation'}).click();await page.getByText('Local metadata updated. Provider access, revocation, and billing remain unverified.').waitFor();};
 await approve();
 const external=page.getByRole('article',{name:'pod-existing',exact:true});
 await external.getByRole('combobox',{name:'Attach pod-existing to'}).selectOption({label:'project-dev'});
 await external.getByRole('textbox',{name:'Credential reference for pod-existing'}).fill('1234567890abcdef1234567890abcdef');
 await external.getByRole('button',{name:'Review use attachment…'}).click();await approve();
 await external.getByText('Access (1)',{exact:true}).click();
 await external.getByRole('button',{name:'Review local detachment…'}).click();await approve();
 await external.getByText('Access (0)',{exact:true}).waitFor();
 await external.getByRole('combobox',{name:'Attach pod-existing to'}).selectOption({label:'project-dev'});
 await external.getByRole('button',{name:'Review use attachment…'}).click();await approve();
 const response=await page.request.post(origin+'/api/cells/project-dev/delete-plan',{data:{}});if(!response.ok())throw new Error(await response.text());
 const deletion=await response.json();await page.goto(origin+'/?deletion='+deletion.id);
 await page.getByRole('heading',{name:'Delete project-dev?'}).waitFor();
 await page.getByRole('heading',{name:'Resource cleanup',exact:true}).waitFor();
 await page.getByText('Public access will stop.').waitFor();
 const select=page.getByRole('combobox',{name:/Cleanup /});
 await select.selectOption('operator');
 if(await page.getByRole('button',{name:'Delete cell',exact:true}).isEnabled())throw new Error('unreviewed choices accepted');
 await page.getByRole('button',{name:'Review updated cleanup choices'}).click();
 await page.getByText('Public access will stop.').waitFor({state:'hidden'});
 await select.selectOption('default');await page.getByRole('button',{name:'Review updated cleanup choices'}).click();
 await page.getByText('Public access will stop.').waitFor();await page.reload();
 await page.getByRole('textbox',{name:'Cell name to delete'}).fill('project-dev');
 if(await page.getByRole('button',{name:'Delete cell',exact:true}).isEnabled())throw new Error('manual deletion obligations were not required');
 await page.getByRole('checkbox',{name:/I accept responsibility for manual provider/}).check();
 await page.getByRole('button',{name:'Delete cell',exact:true}).click();
 await page.getByText(/Provider cleanup is still manual/).waitFor();
 await page.getByRole('button',{name:'Done — I’ll handle provider cleanup'}).click();
 await page.getByRole('heading',{name:'Delete project-dev?'}).waitFor({state:'hidden'});
 await page.getByRole('button',{name:'Resources',exact:true}).click();
 await page.getByText('Unpublished; data retained').waitFor();await page.getByRole('definition').filter({hasText:/^Operator$/}).first().waitFor();
 await page.getByRole('article',{name:'pod-existing',exact:true}).getByText('Access (0)',{exact:true}).waitFor();
 await page.getByRole('article',{name:'pod-existing',exact:true}).getByText('Provider state unknown',{exact:true}).waitFor();
 console.log('PASS: real HTTP/browser external registration, attachment, detachment, acknowledgements, deletion review, reload, cleanup and retained resources');
}finally{await browser.close()}})().catch(e=>{console.error(e);process.exit(1)});
"#;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        tokio::process::Command::new("node")
            .kill_on_drop(true)
            .arg("-e")
            .arg(script)
            .arg(std::env::var("PUBLISH_SMOKE_PLAYWRIGHT").expect("Playwright module path"))
            .arg(format!("http://{addr}/?deletion={}", job.id))
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    server.abort();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
    assert!(registry.public_file(&site, "index.html").is_err());
}

#[tokio::test]
async fn stale_transfer_target_pin_invalidates_deletion_before_cleanup() {
    let f = Fixture::new();
    let (registry, _) = f.site("sites");
    let key = crate::Secret::generate().public();
    fs::write(f.dir.join("tree/.pins/project.pub"), key.to_string()).unwrap();
    let id = registry.resources().unwrap().resources[0].id;
    let view = preview_with(
        f.op.clone(),
        "project-dev".parse().unwrap(),
        Options {
            choices: [(id, ResourceOwner::Cell(CellIdentity(key)))].into(),
        },
    )
    .await
    .unwrap()
    .0;
    let job = Deletion::load(&f.op.config.state, view.id).unwrap();
    fs::write(
        f.dir.join("tree/.pins/project.pub"),
        crate::Secret::generate().public().to_string(),
    )
    .unwrap();
    let _ = f.start(&job).await.unwrap();
    assert!(matches!(
        f.settled(job.id).await.phase,
        DeletionPhase::Failed {
            recovery: RecoveryAction::Review,
            ..
        }
    ));
    assert!(f.edge.calls.lock().unwrap().is_empty());
    assert_eq!(
        registry.resources().unwrap().resources[0].owner,
        ResourceOwner::Cell(CellIdentity(f.secret.public()))
    );
}

#[tokio::test]
async fn deleting_a_delegate_only_removes_its_access() {
    let f = Fixture::new();
    let registry = Registry::create(&f.dir.join("sites")).unwrap();
    let owner = crate::Secret::generate();
    let ns = registry.allocate_namespace(owner.public()).unwrap();
    let grant = crate::Grant::Publish(PublishScope::Namespace(ns.clone()));
    let owned = Cell::new("owner".parse().unwrap(), [grant.clone()]);
    let delegated = Cell::new("project-dev".parse().unwrap(), [grant]);
    registry
        .approve_tree(
            &Tree::new([(owned.clone(), None), (delegated.clone(), None)]).unwrap(),
            &[
                (owned.name, owner.public()),
                (delegated.name, f.secret.public()),
            ]
            .into(),
        )
        .unwrap();
    registry
        .configure_public_base("https://sites.example.com".parse().unwrap())
        .unwrap();
    let site = SiteId {
        namespace: ns,
        name: "demo".parse().unwrap(),
    };
    let artifact = Artifact {
        files: vec![Entry {
            path: "index.html".into(),
            bytes: b"hello".to_vec(),
        }],
    };
    let request = SignedRequest::sign(
        &owner,
        registry.audience().unwrap(),
        RequestId::generate(),
        transport::unix_time().unwrap() + 240,
        Command::Publish {
            site: site.clone(),
            artifact: artifact.hash().unwrap(),
        },
    );
    registry.execute_upload(&request, &artifact).unwrap();
    fs::write(f.dir.join("tree/.resources.json"),json!({"backends":[{"name":"sites","provider":registry.audience().unwrap(),"location":{"kind":"local","registry":f.dir.join("sites")}}]}).to_string()).unwrap();
    let job = f.preview("project-dev").await.unwrap();
    let _ = f.start(&job).await.unwrap();
    assert_eq!(f.settled(job.id).await.phase, DeletionPhase::Deleted);
    let inventory = registry.resources().unwrap();
    assert_eq!(
        inventory.resources[0].owner,
        ResourceOwner::Cell(CellIdentity(owner.public()))
    );
    assert!(!inventory.resources[0]
        .attachments
        .iter()
        .any(|a| a.cell == CellIdentity(f.secret.public())));
    assert_eq!(registry.public_file(&site, "index.html").unwrap(), b"hello");
}

#[tokio::test]
async fn external_attachment_deletion_requires_manual_ack_and_preserves_other_access() {
    use crate::resource::*;
    let f = Fixture::new();
    let (registry, _) = f.site("sites");
    let registration = ExternalRegistration {
        account: ExternalAccount {
            provider: "runpod".to_string().try_into().unwrap(),
            account: "account-a".to_string().try_into().unwrap(),
        },
        kind: ResourceKind::Compute,
        external_id: "pod-existing".to_string().try_into().unwrap(),
    };
    let plan = registry.plan_external(registration).unwrap();
    let id = plan.resource.id;
    registry
        .apply_resource_acknowledged(RequestId::generate(), &plan, plan.approval(), true)
        .unwrap();
    let other = CellIdentity(crate::Secret::generate().public());
    for cell in [CellIdentity(f.secret.public()), other] {
        let plan = registry
            .plan_resource(
                id,
                ResourceIntent::Attach(ResourceAttachment {
                    cell,
                    scope: AccessScope::Use,
                    credential: None,
                }),
            )
            .unwrap();
        registry
            .apply_resource_acknowledged(RequestId::generate(), &plan, plan.approval(), true)
            .unwrap();
    }
    let job = f.preview("project-dev").await.unwrap();
    assert!(f.start(&job).await.is_err());
    assert!(f.edge.calls.lock().unwrap().is_empty());
    let _ = start(
        State(f.op.clone()),
        Path(job.id),
        Json(Confirm {
            name: job.cell().name.clone(),
            approval: job.approval,
            acknowledge_manual: true,
        }),
    )
    .await
    .unwrap();
    let done = f.settled(job.id).await;
    assert_eq!(done.phase, DeletionPhase::Deleted);
    assert!(done
        .resources_done
        .values()
        .any(|r| !r.manual_obligations.is_empty()));
    let record = registry
        .resources()
        .unwrap()
        .resources
        .into_iter()
        .find(|r| r.id == id)
        .unwrap();
    assert_eq!(record.owner, ResourceOwner::Operator);
    assert_eq!(record.attachments.len(), 1);
    assert_eq!(record.attachments[0].cell, other);
    assert_eq!(record.state, ResourceState::Unknown);
}
