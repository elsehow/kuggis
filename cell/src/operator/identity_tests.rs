use super::*;
use crate::{Grant, Said, Secret, Session, Statement};

struct NoContact;
impl Edge for NoContact {
    fn listing(&self) -> apply::Fallible<(Vec<crate::exe::Vm>, Vec<crate::exe::Integration>)> {
        panic!("rename contacted edge")
    }
    fn binary(&self) -> apply::Fallible<Option<Hash>> {
        panic!("rename inspected runtime")
    }
    fn run(&self, _: &[&str]) -> apply::Fallible<String> {
        panic!("rename changed infrastructure")
    }
    fn log(&self, _: &str) -> apply::Fallible<Option<(crate::Key, Vec<Statement>)>> {
        panic!("rename pulled history")
    }
    fn setup(&self, _: &str, _: &Cell) -> apply::Fallible<String> {
        panic!("rename restarted cell")
    }
    fn knock(&self, _: &str, _: &Said) -> apply::Fallible<u64> {
        panic!("rename sent command")
    }
}

#[tokio::test]
async fn parent_and_child_rename_preserve_identity_authority_and_history() {
    let dir = std::env::temp_dir().join(format!("cell-identity-{}", RequestId::generate()));
    let tree_dir = dir.join("tree");
    let parent = Cell::new(
        "project".parse().unwrap(),
        [Grant::Host("example.org".parse().unwrap())],
    );
    let child = Cell::new("worker".parse().unwrap(), []);
    let child_dir = tree_dir.join(parent.id.as_str());
    fs::create_dir_all(&child_dir).unwrap();
    let parent_path = tree_dir.join(format!("{}.json", parent.id));
    let child_path = child_dir.join(format!("{}.json", child.id));
    fs::write(&parent_path, parent.canonical()).unwrap();
    fs::set_permissions(&parent_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&child_path, child.canonical()).unwrap();
    let secret = Secret::generate();
    let pins = tree_dir.join(".pins");
    apply::pin(&pins, &parent.id, &secret.public()).unwrap();
    let session = Session::new(parent.id.clone(), "Conversation".into());
    let statement = Statement::sign(
        &secret,
        parent.id.clone(),
        1,
        Said::Session {
            session: session.reference(),
            state: crate::State::Working,
            text: None,
        },
    );
    let history = crate::canon::bytes(&statement);
    let op = Arc::new(Operator {
        config: Config {
            tree: tree_dir.clone(),
            state: dir.join("state"),
            assets: dir.clone(),
            listen: "127.0.0.1:0".parse().unwrap(),
            access: Access::Local,
            binary: None,
            sockets: None,
        },
        authority: "localhost".into(),
        relays: Mutex::new(BTreeMap::new()),
        vms: tokio::sync::RwLock::new([(parent.id.clone(), "original-vm".into())].into()),
        edge: Arc::new(NoContact),
        changes: Mutex::new(BTreeMap::new()),
        applying: Arc::new(Mutex::new(())),
        stopping: tokio::sync::watch::channel(false).0,
    });
    let rename_to = |id: CellId, name: &str| {
        rename(
            State(op.clone()),
            Path(id),
            Json(Rename {
                name: name.parse().unwrap(),
            }),
        )
    };
    assert_eq!(
        rename_to(parent.id.clone(), "renamed-project")
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(fs::read(&child_path).unwrap(), child.canonical());
    assert_eq!(
        rename_to(child.id.clone(), "renamed-worker").await.unwrap(),
        StatusCode::NO_CONTENT
    );
    let (tree, paths) = op.tree().unwrap();
    assert_eq!(paths[&parent.id], parent_path);
    assert_eq!(
        fs::metadata(&parent_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(paths[&child.id], child_path);
    assert_eq!(tree.parent(&child.id).unwrap().id, parent.id);
    assert_eq!(
        tree.get(&parent.id).unwrap().name.as_str(),
        "renamed-project"
    );
    for original in [&parent, &child] {
        let renamed = tree.get(&original.id).unwrap();
        assert_eq!(renamed.hash(), original.hash());
        assert_eq!(renamed.tag(), original.tag());
        assert_eq!(renamed.grants, original.grants);
        let children = tree.children(&original.id).map(|c| c.id.clone()).collect();
        assert!(Plan::new(
            renamed,
            &children,
            Some((original, &children)),
            Some((original.hash(), Hash::ZERO)),
            None
        )
        .is_empty());
    }
    session
        .reference()
        .check_owner(&tree.get(&parent.id).unwrap().id)
        .unwrap();
    let restored: Statement = serde_json::from_slice(&history).unwrap();
    assert!(restored.open(&secret.public()).is_ok());
    assert_eq!(restored.cell, parent.id);
    assert_eq!(
        fs::read_to_string(pins.join(format!("{}.pub", parent.id))).unwrap(),
        format!("{}\n", secret.public())
    );
    assert_eq!(op.vms.read().await[&parent.id], "original-vm");
    assert_eq!(
        rename_to(child.id.clone(), "renamed-project")
            .await
            .unwrap_err()
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        rename_to(CellId::generate(), "missing")
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
    let lock = op.applying.lock().await;
    assert_eq!(
        rename_to(parent.id.clone(), "busy").await.unwrap_err().0,
        StatusCode::CONFLICT
    );
    drop(lock);
    assert!(
        serde_json::from_value::<Rename>(json!({"name":"valid", "id":CellId::generate()})).is_err()
    );
    assert!(serde_json::from_value::<Rename>(json!({"name":"../escape"})).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn uuid_identity_is_required_and_configuration_changes_remain_bound() {
    let cell = Cell::new("example".parse().unwrap(), []);
    let other = Cell::new(cell.name.clone(), []);
    assert_ne!(cell.id, other.id);
    assert_ne!(cell.hash(), other.hash());
    assert_eq!(cell.id.as_str().as_bytes()[14], b'4');
    assert!(matches!(
        cell.id.as_str().as_bytes()[19],
        b'8' | b'9' | b'a' | b'b'
    ));
    assert_eq!(cell.id.as_str().parse::<CellId>().unwrap(), cell.id);
    for bad in [
        "dev",
        "",
        "../../file",
        "00000000000000000000000000000000",
        "00000000-0000-4000-8000-00000000000G",
    ] {
        assert!(bad.parse::<CellId>().is_err());
    }
    assert!(serde_json::from_value::<Cell>(json!({"name":"old-cell","grants":[]})).is_err());
    let mut changed = cell.clone();
    changed
        .grants
        .insert(Grant::Host("example.org".parse().unwrap()));
    assert_ne!(changed.hash(), cell.hash());
}
