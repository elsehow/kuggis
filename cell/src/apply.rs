//! Apply: the loop that walks exe.dev and the boxes toward the tree.
//!
//! One rule makes it safe. Before every action the plan is recomputed from
//! a fresh observation and must sit inside what the operator approved: the
//! same declaration, a subset of the actions. So a done action vanishes, a
//! failed one is retried by running apply again, success is proven by
//! observing rather than by trusting the edge's reply, and anything that
//! drifted beyond the approval stops the loop.
//!
//! Apply never destroys. It removes no VM, takes no tag off, and leaves
//! alone any attachment that other VMs share. Those stay the operator's.

use crate::cell::{Cell, TAG_PREFIX};
use crate::exe::{self, door_name, vm_of, Integration, Observed, Vm};
use crate::grant::Grant;
use crate::plan::{Action, Approved, Plan};
use crate::prim::{CellId, Hash, Repo};
use crate::said::{Key, Said, Statement};
use crate::tree::Tree;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

pub type Fallible<T = ()> = Result<T, Box<dyn Error>>;

/// What apply needs from exe.dev and from a box. One implementation speaks
/// ssh; the tests hold one in memory.
pub trait Edge {
    /// `ls --json` and `integrations list --json`, parsed.
    fn listing(&self) -> Fallible<(Vec<Vm>, Vec<Integration>)>;
    /// Hash of the binary `setup` would ship; `None` when there is none
    /// at hand, in which case a box is not asked to match it.
    fn binary(&self) -> Fallible<Option<Hash>>;
    /// One exe.dev command, by its words.
    fn run(&self, args: &[&str]) -> Fallible<String>;
    /// The box's public key and its statement log; `None` before setup.
    fn log(&self, vm: &str) -> Fallible<Option<(Key, Vec<Statement>)>>;
    /// Put the binary and the canonical cell file on the box and run
    /// `cell setup` as root. Returns what it printed.
    fn setup(&self, vm: &str, cell: &Cell) -> Fallible<String>;
    /// File a Said in the box's inbox as root, which is to say as you.
    /// Returns its inbox number.
    fn knock(&self, vm: &str, said: &Said) -> Fallible<u64>;
    /// Publish only this cell and its immediate relatives; never the full tree.
    fn context(&self, _vm: &str, _context: &crate::context::Context) -> Fallible {
        Err("transport does not support cell context publication".into())
    }
}

/// Pin a box's key on first contact; refuse a key that changed. The pin is
/// `<dir>/<cell>.pub`, and the operator removes it by hand to pin anew.
pub fn pin(dir: &Path, cell: &CellId, key: &Key) -> Fallible {
    let path = dir.join(format!("{cell}.pub"));
    match fs::read_to_string(&path) {
        Ok(s) => {
            let pinned: Key = s.parse()?;
            if pinned != *key {
                return Err(format!(
                    "{cell}: its key is {key}, not the pinned {pinned}; if the box was rebuilt on purpose, remove {}",
                    path.display()
                )
                .into());
            }
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(dir)?;
            fs::write(&path, format!("{key}\n"))?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// The box's log, pulled and verified under its pinned key: every
/// statement in sequence, each opened. `None` before setup.
pub fn pull(
    edge: &dyn Edge,
    pins: &Path,
    cell: &CellId,
    vm: &str,
) -> Fallible<Option<Vec<(Statement, Said)>>> {
    let Some((key, log)) = edge.log(vm)? else {
        return Ok(None);
    };
    pin(pins, cell, &key)?;
    let mut out = Vec::with_capacity(log.len());
    let mut seq = None;
    for s in log {
        if s.cell != *cell {
            return Err(format!("{cell}: its log holds a statement by {}", s.cell).into());
        }
        if !s.follows(seq) {
            return Err(format!("{cell}: statement {} is out of sequence", s.seq).into());
        }
        seq = Some(s.seq);
        let said = s
            .open(&key)
            .map_err(|e| format!("{cell}: statement {}: {e}", s.seq))?
            .clone();
        out.push((s, said));
    }
    Ok(Some(out))
}

/// What the box last proved it is running: the cell hash and the binary
/// hash of the latest `Checked` that passed. `None` before setup, before
/// any check, or when the last check failed, so that setup runs again.
pub fn on_box(
    edge: &dyn Edge,
    pins: &Path,
    cell: &CellId,
    vm: &str,
) -> Fallible<Option<(Hash, Hash)>> {
    let Some(log) = pull(edge, pins, cell, vm)? else {
        return Ok(None);
    };
    let last = log.iter().rev().find_map(|(_, said)| match said {
        Said::Checked {
            cell, ok, binary, ..
        } => Some((*cell, *binary, *ok)),
        _ => None,
    });
    Ok(last.and_then(|(cell, binary, ok)| ok.then_some((cell, binary))))
}

/// What each box last proved, remembered across the loop since only setup
/// changes it, and the hash of the binary setup would ship, read once.
struct Boxes {
    proved: BTreeMap<CellId, Option<(Hash, Hash)>>,
    bin: Option<Hash>,
}

impl Boxes {
    fn new(edge: &dyn Edge) -> Fallible<Boxes> {
        Ok(Boxes {
            proved: BTreeMap::new(),
            bin: edge.binary()?,
        })
    }
}

fn plan_one(
    edge: &dyn Edge,
    pins: &Path,
    tree: &Tree,
    seen: &Observed,
    vms: &[Vm],
    boxes: &mut Boxes,
    name: &CellId,
) -> Fallible<Plan> {
    let want = tree
        .get(name)
        .ok_or_else(|| format!("{name}: not in the tree"))?;
    let children = tree.children(name).map(|c| c.id.clone()).collect();
    let proved = match boxes.proved.get(name) {
        Some(h) => *h,
        None => {
            let h = match vm_of(vms, name) {
                Some(vm) => on_box(edge, pins, name, &vm.vm_name)?,
                None => None,
            };
            boxes.proved.insert(name.clone(), h);
            h
        }
    };
    Ok(Plan::new(want, &children, seen.of(name), proved, boxes.bin))
}

/// Plan every named cell against a fresh observation. The same function
/// prints the plan and drives the loop, so what you approved is what runs.
pub fn plans(
    edge: &dyn Edge,
    pins: &Path,
    tree: &Tree,
    names: &[CellId],
) -> Fallible<BTreeMap<CellId, Plan>> {
    let (vms, ints) = edge.listing()?;
    exe::cell_vms(&vms)?;
    let seen = exe::observe(&vms, &ints);
    let mut boxes = Boxes::new(edge)?;
    names
        .iter()
        .map(|n| {
            Ok((
                n.clone(),
                plan_one(edge, pins, tree, &seen, &vms, &mut boxes, n)?,
            ))
        })
        .collect()
}

/// Walk the approved cells, leaves first, one action at a time, re-planning
/// from a fresh observation before each. Returns how many actions ran.
pub fn apply(
    edge: &dyn Edge,
    pins: &Path,
    tree: &Tree,
    approved: &Approved,
    out: &mut dyn Write,
) -> Fallible<usize> {
    apply_with_progress(edge, pins, tree, approved, out, &mut |_, _| Ok(()))
}

/// The operator uses the same reconciler, with durable progress before each
/// action. A failed progress write stops before making the external change.
pub fn apply_with_progress(
    edge: &dyn Edge,
    pins: &Path,
    tree: &Tree,
    approved: &Approved,
    out: &mut dyn Write,
    progress: &mut dyn FnMut(&CellId, &Action) -> Fallible,
) -> Fallible<usize> {
    let mut order: Vec<&CellId> = approved.plans().keys().collect();
    order.sort_by_key(|n| std::cmp::Reverse(tree.depth(n)));
    let mut boxes = Boxes::new(edge)?;
    let mut done = 0;
    loop {
        let (vms, ints) = edge.listing()?;
        exe::cell_vms(&vms)?;
        let seen = exe::observe(&vms, &ints);
        let mut next = None;
        for name in &order {
            let plan = plan_one(edge, pins, tree, &seen, &vms, &mut boxes, name)?;
            approved.permits(name, &plan)?;
            if let Some(action) = plan.actions.into_iter().next() {
                next = Some(((*name).clone(), action));
                break;
            }
        }
        let Some((name, action)) = next else {
            // Also runs for no-op apply and retries after a failed publication.
            for id in &order {
                let vm = vm_of(&vms, id).ok_or("cell has no VM")?;
                edge.context(
                    &vm.vm_name,
                    &crate::context::Context::for_cell(tree, id, pins)?,
                )?;
            }
            return Ok(done);
        };
        progress(&name, &action)?;
        writeln!(out, "{name}: {action}")?;
        let cell = tree.get(&name).expect("planned, so in the tree");
        perform(edge, &vms, &ints, cell, &action, out)?;
        if action == Action::Setup {
            boxes.proved.remove(&name);
        }
        done += 1;
    }
}

fn perform(
    edge: &dyn Edge,
    vms: &[Vm],
    ints: &[Integration],
    cell: &Cell,
    action: &Action,
    out: &mut dyn Write,
) -> Fallible {
    let name = &cell.id;
    let tag = format!("tag:{TAG_PREFIX}{name}");
    let me =
        || vm_of(vms, name).ok_or_else(|| format!("{name}: no VM carries its tag; apply again"));
    match action {
        Action::Create => {
            edge.run(&[
                "new",
                &format!("--name={}", exe::provision_name(name)),
                &format!("--tag={TAG_PREFIX}{name}"),
                "--no-email",
            ])
            .map_err(|e| {
                format!("{e}; to make an existing VM this cell's root: cell adopt VM {name}")
            })?;
        }
        Action::Attach(g) => attach(edge, ints, name, &tag, g)?,
        Action::Detach(g) => {
            let me = me()?;
            let carrying = ints
                .iter()
                .filter(|i| i.door().is_none() && i.attached_to(me) && i.grants().contains(g));
            for i in carrying {
                for spec in &i.attachments {
                    if let Some(spec) = ours(spec, me, name, &i.name)? {
                        edge.run(&["integrations", "detach", &i.name, spec])?;
                    }
                }
            }
        }
        Action::Open(child) => {
            let door = door_name(child);
            let child_vm = vm_of(vms, child)
                .ok_or_else(|| format!("{name}: {child} has no VM yet; apply it first"))?;
            if ints.iter().any(|i| i.name == door) {
                edge.run(&["integrations", "attach", &door, &tag])?;
            } else {
                edge.run(&[
                    "integrations",
                    "add",
                    "http-proxy",
                    &format!("--name={door}"),
                    &format!("--target=https://{}.exe.xyz/", child_vm.vm_name),
                    "--peer",
                    &format!("--attach={tag}"),
                ])?;
            }
        }
        Action::Close(child) => {
            let door = door_name(child);
            let me = me()?;
            if let Some(i) = ints.iter().find(|i| i.name == door) {
                let mut left = i.attachments.len();
                for spec in &i.attachments {
                    if let Some(spec) = ours(spec, me, name, &door)? {
                        edge.run(&["integrations", "detach", &door, spec])?;
                        left -= 1;
                    }
                }
                if left == 0 {
                    edge.run(&["integrations", "remove", &door])?;
                }
            }
        }
        Action::Setup => {
            let printed = edge.setup(&me()?.vm_name, cell)?;
            out.write_all(printed.as_bytes())?;
        }
    }
    Ok(())
}

/// An attachment spec that reaches `me` and is ours alone to remove:
/// `vm:<me>` or our own tag. One by a tag other VMs may share is refused,
/// not silently detached; one that does not reach `me` is `None`.
fn ours<'a>(spec: &'a str, me: &Vm, cell: &CellId, int: &str) -> Fallible<Option<&'a str>> {
    match exe::AttachmentTarget::parse(spec) {
        exe::AttachmentTarget::Vm(vm) => Ok((vm.as_str() == me.vm_name).then_some(spec)),
        exe::AttachmentTarget::Tag(tag) if tag == format!("{TAG_PREFIX}{cell}") => Ok(Some(spec)),
        exe::AttachmentTarget::Tag(tag) if me.tags.iter().any(|owned| owned == &tag) => {
            Err(format!(
                "{cell}: {int} reaches its VM by {spec}, which other VMs may share; detach it by hand"
            ).into())
        }
        exe::AttachmentTarget::Tag(_) | exe::AttachmentTarget::Auto(_) | exe::AttachmentTarget::Unknown(_) => Ok(None),
    }
}

/// Attach exactly one integration that is exactly this grant. A repository
/// gets one made in its name if none exists; anything else must already.
fn attach(edge: &dyn Edge, ints: &[Integration], cell: &CellId, tag: &str, g: &Grant) -> Fallible {
    let exact = ints
        .iter()
        .find(|i| i.door().is_none() && i.grants() == [g.clone()]);
    if let Some(i) = exact {
        edge.run(&["integrations", "attach", &i.name, tag])?;
        return Ok(());
    }
    let Grant::Code { repo, write, .. } = g else {
        return Err(format!(
            "{cell}: no exe.dev integration is exactly {g}; add one, then apply again"
        )
        .into());
    };
    let (flag, suffix) = if *write {
        (None, "")
    } else {
        (Some("--readonly"), "-ro")
    };
    let mut args = vec![
        "integrations".to_string(),
        "add".into(),
        "github".into(),
        format!("--name={}", github_name(repo, suffix)),
        format!("--repository={repo}"),
    ];
    args.extend(flag.map(str::to_string));
    args.push(format!("--attach={tag}"));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    edge.run(&args)?;
    Ok(())
}

/// `gh-<owner>-<repo>` and a suffix, in the characters a name allows.
fn github_name(repo: &Repo, suffix: &str) -> String {
    let mut name = String::from("gh-");
    for c in repo.as_str().chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_lowercase());
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let mut name = name.trim_end_matches('-').to_string();
    name.push_str(suffix);
    name.chars().take(63).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::said::Secret;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// exe.dev and every box, in memory.
    #[derive(Default)]
    struct Fake {
        vms: RefCell<Vec<Vm>>,
        ints: RefCell<Vec<Integration>>,
        boxes: RefCell<BTreeMap<String, (Secret, Vec<Statement>)>>,
        inboxes: RefCell<BTreeMap<String, Vec<Said>>>,
        calls: RefCell<Vec<String>>,
        bin: RefCell<Option<Hash>>,
        contexts: RefCell<BTreeMap<String, crate::context::Context>>,
        fail_context: std::cell::Cell<bool>,
    }

    fn opt<'a>(args: &'a [&str], key: &str) -> Option<&'a str> {
        args.iter().find_map(|a| a.strip_prefix(key))
    }

    impl Fake {
        fn vm(&self, name: &str, tags: &[&str]) {
            self.vms.borrow_mut().push(Vm {
                created_at: Some("fixture-incarnation".into()),
                vm_name: name.into(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                status: "running".into(),
            });
        }

        fn int(&self, name: &str, kind: &str, attachments: &[&str], config: serde_json::Value) {
            self.ints.borrow_mut().push(Integration {
                name: name.into(),
                kind: kind.into(),
                attachments: attachments.iter().map(|a| a.to_string()).collect(),
                config,
            });
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn attachments(&self, int: &str) -> Vec<String> {
            self.ints
                .borrow()
                .iter()
                .find(|i| i.name == int)
                .map(|i| i.attachments.clone())
                .unwrap_or_default()
        }
    }

    impl Edge for Fake {
        fn context(&self, vm: &str, context: &crate::context::Context) -> crate::apply::Fallible {
            if self.fail_context.replace(false) {
                return Err("context transport failed".into());
            }
            self.contexts
                .borrow_mut()
                .insert(vm.into(), context.clone());
            Ok(())
        }
        fn listing(&self) -> Fallible<(Vec<Vm>, Vec<Integration>)> {
            Ok((self.vms.borrow().clone(), self.ints.borrow().clone()))
        }

        fn run(&self, args: &[&str]) -> Fallible<String> {
            self.calls.borrow_mut().push(args.join(" "));
            match args {
                ["new", rest @ ..] => {
                    let name = opt(rest, "--name=").ok_or("new: --name")?;
                    if self.vms.borrow().iter().any(|v| v.vm_name == name) {
                        return Err("name taken".into());
                    }
                    self.vm(name, &opt(rest, "--tag=").into_iter().collect::<Vec<_>>());
                }
                ["integrations", "attach", name, spec] => {
                    let mut ints = self.ints.borrow_mut();
                    let i = ints
                        .iter_mut()
                        .find(|i| i.name == *name)
                        .ok_or("no such integration")?;
                    i.attachments.push(spec.to_string());
                }
                ["integrations", "detach", name, spec] => {
                    let mut ints = self.ints.borrow_mut();
                    let i = ints
                        .iter_mut()
                        .find(|i| i.name == *name)
                        .ok_or("no such integration")?;
                    i.attachments.retain(|a| a != spec);
                }
                ["integrations", "remove", name] => {
                    self.ints.borrow_mut().retain(|i| i.name != *name);
                }
                ["integrations", "add", kind, rest @ ..] => {
                    let name = opt(rest, "--name=").ok_or("add: --name")?;
                    let config = match *kind {
                        "http-proxy" => serde_json::json!({
                            "target": opt(rest, "--target="),
                            "peer": rest.contains(&"--peer"),
                        }),
                        "github" => serde_json::json!({
                            "repositories": [opt(rest, "--repository=")],
                            "readonly": rest.contains(&"--readonly"),
                        }),
                        _ => serde_json::json!({}),
                    };
                    let attach: Vec<&str> = opt(rest, "--attach=").into_iter().collect();
                    self.int(name, kind, &attach, config);
                }
                ["tag", vm, tag] => {
                    let mut vms = self.vms.borrow_mut();
                    let v = vms
                        .iter_mut()
                        .find(|v| v.vm_name == *vm)
                        .ok_or("no such vm")?;
                    v.tags.push(tag.to_string());
                }
                _ => return Err(format!("fake exe.dev: {}", args.join(" ")).into()),
            }
            Ok(String::new())
        }

        fn log(&self, vm: &str) -> Fallible<Option<(Key, Vec<Statement>)>> {
            Ok(self
                .boxes
                .borrow()
                .get(vm)
                .map(|(s, log)| (s.public(), log.clone())))
        }

        fn binary(&self) -> Fallible<Option<Hash>> {
            Ok(*self.bin.borrow())
        }

        fn setup(&self, vm: &str, cell: &Cell) -> Fallible<String> {
            self.calls
                .borrow_mut()
                .push(format!("setup {vm} {}", cell.hash()));
            let mut boxes = self.boxes.borrow_mut();
            let (secret, log) = boxes
                .entry(vm.to_string())
                .or_insert_with(|| (Secret::generate(), Vec::new()));
            let seq = log.len() as u64 + 1;
            log.push(Statement::sign(
                secret,
                cell.id.clone(),
                seq,
                Said::Checked {
                    cell: cell.hash(),
                    ok: true,
                    report: String::new(),
                    binary: self.bin.borrow().unwrap_or(Hash::ZERO),
                    signed: BTreeSet::new(),
                },
            ));
            Ok("WALL OK\n".into())
        }

        fn knock(&self, vm: &str, said: &Said) -> Fallible<u64> {
            let mut inboxes = self.inboxes.borrow_mut();
            let inbox = inboxes.entry(vm.to_string()).or_default();
            inbox.push(said.clone());
            Ok(inbox.len() as u64)
        }
    }

    fn tree(nodes: &[(&str, &str)]) -> Tree {
        Tree::new(nodes.iter().map(|(parent, json)| {
            let under = (!parent.is_empty()).then(|| parent.parse().unwrap());
            (serde_json::from_str(json).unwrap(), under)
        }))
        .unwrap()
    }

    fn pins() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "cell-apply-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn all(tree: &Tree) -> Vec<CellId> {
        tree.iter().map(|c| c.id.clone()).collect()
    }

    fn approve(edge: &dyn Edge, pins: &Path, tree: &Tree) -> Approved {
        let plans = plans(edge, pins, tree, &all(tree)).unwrap();
        Approved::new(plans.clone(), &Approved::hash(&plans)).unwrap()
    }

    const A: &str = r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","grants":[{"provider":"anthropic"},{"code":{"repo":"example/x","write":true}}]}"#;
    const B: &str = r#"{"id":"46b16f13-11ac-5315-b431-845f196f907e","name":"46b16f13-11ac-5315-b431-845f196f907e","grants":[{"provider":"anthropic"}]}"#;

    #[test]
    fn a_fresh_tree_converges_leaves_first_and_then_rests() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A), ("904254dc-1b8f-5e80-9755-e5afc5b195c5", B)]);
        let approved = approve(&f, &pins, &t);
        let mut out = Vec::new();
        let n = apply(&f, &pins, &t, &approved, &mut out).unwrap();
        assert_eq!(n, 6);
        let a: Cell = serde_json::from_str(A).unwrap();
        let b: Cell = serde_json::from_str(B).unwrap();
        assert_eq!(
            f.calls(),
            [
                "new --name=cell-46b16f13-11ac-5315-b431-845f196f907e --tag=cell-46b16f13-11ac-5315-b431-845f196f907e --no-email".to_string(),
                format!("setup cell-46b16f13-11ac-5315-b431-845f196f907e {}", b.hash()),
                "new --name=cell-904254dc-1b8f-5e80-9755-e5afc5b195c5 --tag=cell-904254dc-1b8f-5e80-9755-e5afc5b195c5 --no-email".into(),
                "integrations add github --name=gh-example-x --repository=example/x --attach=tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5".into(),
                "integrations add http-proxy --name=door-46b16f13-11ac-5315-b431-845f196f907e --target=https://cell-46b16f13-11ac-5315-b431-845f196f907e.exe.xyz/ --peer --attach=tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5".into(),
                format!("setup cell-904254dc-1b8f-5e80-9755-e5afc5b195c5 {}", a.hash()),
            ]
        );
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.starts_with("46b16f13-11ac-5315-b431-845f196f907e: \"create\"\n"),
            "{out}"
        );
        assert!(out.contains("904254dc-1b8f-5e80-9755-e5afc5b195c5: {\"open\":\"46b16f13-11ac-5315-b431-845f196f907e\"}\n"), "{out}");
        assert!(
            pins.join("904254dc-1b8f-5e80-9755-e5afc5b195c5.pub")
                .exists()
                && pins
                    .join("46b16f13-11ac-5315-b431-845f196f907e.pub")
                    .exists()
        );

        let again = approve(&f, &pins, &t);
        assert!(again.plans().values().all(Plan::is_empty));
        assert_eq!(apply(&f, &pins, &t, &again, &mut Vec::new()).unwrap(), 0);
    }

    #[test]
    fn context_publication_retries_noop_apply_and_refreshes_renames() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A), ("904254dc-1b8f-5e80-9755-e5afc5b195c5", B)]);
        let approved = approve(&f, &pins, &t);
        f.fail_context.set(true);
        assert!(apply(&f, &pins, &t, &approved, &mut Vec::new()).is_err());
        // Infrastructure converged before metadata transport failed. Retrying
        // the original approval still publishes context despite zero actions.
        assert_eq!(apply(&f, &pins, &t, &approved, &mut Vec::new()).unwrap(), 0);
        let parent: Cell = serde_json::from_str(A).unwrap();
        let child: Cell = serde_json::from_str(B).unwrap();
        let before = f.contexts.borrow()[&crate::exe::provision_name(&parent.id)].clone();
        assert_eq!(before.children.len(), 1);
        assert_eq!(before.children[0].cell.id, child.id);
        assert!(before.children[0].key.is_some());
        let renamed = Tree::new(t.iter().map(|c| {
            let mut copy = c.clone();
            copy.name = if c.id == child.id {
                "renamed-child"
            } else {
                "renamed-parent"
            }
            .parse()
            .unwrap();
            (copy, t.parent(&c.id).map(|p| p.id.clone()))
        }))
        .unwrap();
        let approved = approve(&f, &pins, &renamed);
        assert!(approved.plans().values().all(Plan::is_empty));
        assert_eq!(
            apply(&f, &pins, &renamed, &approved, &mut Vec::new()).unwrap(),
            0
        );
        let after = f.contexts.borrow()[&crate::exe::provision_name(&parent.id)].clone();
        assert_eq!(after.children[0].cell.id, before.children[0].cell.id);
        assert_eq!(after.children[0].key, before.children[0].key);
        assert_eq!(after.children[0].cell.name.as_str(), "renamed-child");
        assert_eq!(after.cell.name.as_str(), "renamed-parent");
    }

    #[test]
    fn a_child_writes_its_fork_and_the_parent_writes_the_upstream() {
        let f = Fake::default();
        let pins = pins();
        let p = r#"{"id":"30f2fb95-bfa2-5caa-b55c-5f41d348f293","name":"p","grants":[{"provider":"anthropic"},{"code":{"repo":"example/x","write":true}},{"code":{"repo":"example/x","write":true,"via":"cells/x"}}]}"#;
        let c = r#"{"id":"acc27909-a4a4-5232-953d-15b8d534ae2a","name":"acc27909-a4a4-5232-953d-15b8d534ae2a","grants":[{"provider":"anthropic"},{"code":{"repo":"example/x","write":true,"via":"cells/x"}}]}"#;
        let t = tree(&[("", p), ("30f2fb95-bfa2-5caa-b55c-5f41d348f293", c)]);
        apply(&f, &pins, &t, &approve(&f, &pins, &t), &mut Vec::new()).unwrap();
        let calls = f.calls();
        for call in [
            "integrations add github --name=gh-cells-x --repository=cells/x --attach=tag:cell-acc27909-a4a4-5232-953d-15b8d534ae2a",
            "integrations attach gh-cells-x tag:cell-30f2fb95-bfa2-5caa-b55c-5f41d348f293",
            "integrations add github --name=gh-example-x --repository=example/x --attach=tag:cell-30f2fb95-bfa2-5caa-b55c-5f41d348f293",
        ] {
            assert!(calls.contains(&call.to_string()), "{call}\n{calls:?}");
        }
        assert!(approve(&f, &pins, &t).plans().values().all(Plan::is_empty));
    }

    #[test]
    fn a_new_binary_rolls_out_through_setup() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A)]);
        let a: Cell = serde_json::from_str(A).unwrap();
        apply(&f, &pins, &t, &approve(&f, &pins, &t), &mut Vec::new()).unwrap();
        assert!(approve(&f, &pins, &t).plans()[&a.id].is_empty());

        *f.bin.borrow_mut() = Some(Hash::of(b"v2"));
        let approved = approve(&f, &pins, &t);
        assert_eq!(approved.plans()[&a.id].actions, vec![Action::Setup]);
        assert_eq!(approved.plans()[&a.id].bin, Hash::of(b"v2"));
        assert_eq!(apply(&f, &pins, &t, &approved, &mut Vec::new()).unwrap(), 1);
        assert_eq!(
            f.calls().last().unwrap(),
            &format!(
                "setup cell-904254dc-1b8f-5e80-9755-e5afc5b195c5 {}",
                a.hash()
            )
        );
        assert!(approve(&f, &pins, &t).plans()[&a.id].is_empty());
    }

    #[test]
    fn drift_beyond_the_approval_stops_the_loop_before_anything_runs() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A)]);
        apply(&f, &pins, &t, &approve(&f, &pins, &t), &mut Vec::new()).unwrap();
        let settled = approve(&f, &pins, &t);
        let before = f.calls().len();

        f.int(
            "gh-extra",
            "github",
            &["vm:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
            serde_json::json!({"repositories": ["o/extra"]}),
        );
        let e = apply(&f, &pins, &t, &settled, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("904254dc-1b8f-5e80-9755-e5afc5b195c5: {\"detach\":")
                && e.contains("not approved"),
            "{e}"
        );
        assert_eq!(f.calls().len(), before, "nothing ran");

        let wider = tree(&[(
            "",
            r#"{"id":"904254dc-1b8f-5e80-9755-e5afc5b195c5","name":"904254dc-1b8f-5e80-9755-e5afc5b195c5","grants":[{"provider":"anthropic"},{"code":{"repo":"example/x","write":true}},{"host":"pypi.org"}]}"#,
        )]);
        let e = apply(&f, &pins, &wider, &settled, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("904254dc-1b8f-5e80-9755-e5afc5b195c5: not what was approved"),
            "{e}"
        );
    }

    #[test]
    fn a_shared_integration_comes_off_whole_and_the_rest_waits_for_approval() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A)]);
        f.vm(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            &["cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
        );
        f.int(
            "gh-both",
            "github",
            &[
                "tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5",
                "vm:70ee78ad-5e3a-510a-af20-6f485ecb6f16",
            ],
            serde_json::json!({"repositories": ["example/x", "o/y"]}),
        );
        f.setup(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            t.get(&"904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
                .unwrap(),
        )
        .unwrap();
        let approved = approve(&f, &pins, &t);
        let a: CellId = "904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap();
        assert_eq!(
            approved.plans()[&a].actions,
            [Action::Detach(Grant::Code {
                repo: "o/y".parse().unwrap(),
                write: true,
                via: None
            })]
        );
        let e = apply(&f, &pins, &t, &approved, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("{\"attach\":{\"code\":{\"repo\":\"example/x\""),
            "{e}"
        );
        assert_eq!(
            f.calls().last().unwrap(),
            "integrations detach gh-both tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"
        );
        assert_eq!(
            f.attachments("gh-both"),
            ["vm:70ee78ad-5e3a-510a-af20-6f485ecb6f16"],
            "70ee78ad-5e3a-510a-af20-6f485ecb6f16 VMs keep it"
        );
    }

    #[test]
    fn an_attachment_by_a_shared_tag_is_never_touched() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A)]);
        f.vm(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            &["cell-904254dc-1b8f-5e80-9755-e5afc5b195c5", "shared"],
        );
        f.int(
            "gh-x",
            "github",
            &["tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
            serde_json::json!({"repositories": ["example/x"]}),
        );
        f.int(
            "gh-z",
            "github",
            &["tag:shared"],
            serde_json::json!({"repositories": ["o/z"]}),
        );
        f.setup(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            t.get(&"904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap())
                .unwrap(),
        )
        .unwrap();
        let approved = approve(&f, &pins, &t);
        let before = f.calls().len();
        let e = apply(&f, &pins, &t, &approved, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(e.contains("tag:shared") && e.contains("by hand"), "{e}");
        assert_eq!(f.calls().len(), before);
    }

    #[test]
    fn a_door_closes_by_detaching_and_is_removed_only_when_nothing_else_holds_it() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A), ("", B)]);
        f.vm(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            &["cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
        );
        f.vm(
            "46b16f13-11ac-5315-b431-845f196f907e",
            &["cell-46b16f13-11ac-5315-b431-845f196f907e"],
        );
        f.int(
            "gh-x",
            "github",
            &["tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
            serde_json::json!({"repositories": ["example/x"]}),
        );
        f.int(
            "door-46b16f13-11ac-5315-b431-845f196f907e",
            "http-proxy",
            &["tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5", "vm:elsewhere"],
            serde_json::json!({"target": "https://46b16f13-11ac-5315-b431-845f196f907e.exe.xyz/", "peer": true}),
        );
        for c in t.iter() {
            f.setup(c.id.as_str(), c).unwrap();
        }
        let approved = approve(&f, &pins, &t);
        let n = apply(&f, &pins, &t, &approved, &mut Vec::new()).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            f.calls().last().unwrap(),
            "integrations detach door-46b16f13-11ac-5315-b431-845f196f907e tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"
        );
        assert_eq!(
            f.attachments("door-46b16f13-11ac-5315-b431-845f196f907e"),
            ["vm:elsewhere"]
        );

        f.run(&[
            "integrations",
            "detach",
            "door-46b16f13-11ac-5315-b431-845f196f907e",
            "vm:elsewhere",
        ])
        .unwrap();
        f.run(&[
            "integrations",
            "attach",
            "door-46b16f13-11ac-5315-b431-845f196f907e",
            "tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5",
        ])
        .unwrap();
        let approved = approve(&f, &pins, &t);
        apply(&f, &pins, &t, &approved, &mut Vec::new()).unwrap();
        assert_eq!(
            f.calls().last().unwrap(),
            "integrations remove door-46b16f13-11ac-5315-b431-845f196f907e"
        );
    }

    #[test]
    fn an_existing_door_is_reattached_rather_than_remade() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", A), ("904254dc-1b8f-5e80-9755-e5afc5b195c5", B)]);
        f.vm(
            "904254dc-1b8f-5e80-9755-e5afc5b195c5",
            &["cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
        );
        f.vm(
            "46b16f13-11ac-5315-b431-845f196f907e",
            &["cell-46b16f13-11ac-5315-b431-845f196f907e"],
        );
        f.int(
            "gh-x",
            "github",
            &["tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"],
            serde_json::json!({"repositories": ["example/x"]}),
        );
        f.int(
            "door-46b16f13-11ac-5315-b431-845f196f907e",
            "http-proxy",
            &[],
            serde_json::json!({"target": "https://46b16f13-11ac-5315-b431-845f196f907e.exe.xyz/", "peer": true}),
        );
        for c in t.iter() {
            f.setup(c.id.as_str(), c).unwrap();
        }
        let approved = approve(&f, &pins, &t);
        assert_eq!(apply(&f, &pins, &t, &approved, &mut Vec::new()).unwrap(), 1);
        assert_eq!(
            f.calls().last().unwrap(),
            "integrations attach door-46b16f13-11ac-5315-b431-845f196f907e tag:cell-904254dc-1b8f-5e80-9755-e5afc5b195c5"
        );
    }

    #[test]
    fn a_key_that_changes_is_refused_until_the_pin_is_removed() {
        let f = Fake::default();
        let pins = pins();
        let t = tree(&[("", B)]);
        let b: Cell = serde_json::from_str(B).unwrap();
        f.vm(
            "46b16f13-11ac-5315-b431-845f196f907e",
            &["cell-46b16f13-11ac-5315-b431-845f196f907e"],
        );
        f.setup("46b16f13-11ac-5315-b431-845f196f907e", &b).unwrap();
        assert!(approve(&f, &pins, &t).plans().values().all(Plan::is_empty));

        f.boxes.borrow_mut().clear();
        f.setup("46b16f13-11ac-5315-b431-845f196f907e", &b).unwrap();
        let e = plans(&f, &pins, &t, &all(&t)).unwrap_err().to_string();
        assert!(e.contains("not the pinned"), "{e}");
        fs::remove_file(pins.join("46b16f13-11ac-5315-b431-845f196f907e.pub")).unwrap();
        assert!(plans(&f, &pins, &t, &all(&t)).is_ok());
    }

    #[test]
    fn a_failed_check_and_a_broken_log_leave_the_box_unproven() {
        let f = Fake::default();
        let pins = pins();
        let b: Cell = serde_json::from_str(B).unwrap();
        let name = b.id.clone();
        let secret = Secret::generate();
        let checked = |seq, ok| {
            Statement::sign(
                &secret,
                name.clone(),
                seq,
                Said::Checked {
                    cell: b.hash(),
                    ok,
                    report: String::new(),
                    binary: Hash::ZERO,
                    signed: BTreeSet::new(),
                },
            )
        };
        f.boxes.borrow_mut().insert(
            "46b16f13-11ac-5315-b431-845f196f907e".into(),
            (secret.clone(), vec![checked(1, true), checked(2, false)]),
        );
        assert_eq!(
            on_box(&f, &pins, &name, "46b16f13-11ac-5315-b431-845f196f907e").unwrap(),
            None
        );

        f.boxes.borrow_mut().insert(
            "46b16f13-11ac-5315-b431-845f196f907e".into(),
            (secret.clone(), vec![checked(1, true)]),
        );
        assert_eq!(
            on_box(&f, &pins, &name, "46b16f13-11ac-5315-b431-845f196f907e").unwrap(),
            Some((b.hash(), Hash::ZERO))
        );

        f.boxes.borrow_mut().insert(
            "46b16f13-11ac-5315-b431-845f196f907e".into(),
            (secret.clone(), vec![checked(1, true), checked(3, true)]),
        );
        let e = on_box(&f, &pins, &name, "46b16f13-11ac-5315-b431-845f196f907e")
            .unwrap_err()
            .to_string();
        assert!(e.contains("out of sequence"), "{e}");

        let forged = Statement::sign(
            &Secret::generate(),
            name.clone(),
            1,
            Said::Fetch {
                url: "https://x.example/".parse().unwrap(),
            },
        );
        f.boxes.borrow_mut().insert(
            "46b16f13-11ac-5315-b431-845f196f907e".into(),
            (secret, vec![forged]),
        );
        let e = on_box(&f, &pins, &name, "46b16f13-11ac-5315-b431-845f196f907e")
            .unwrap_err()
            .to_string();
        assert!(e.contains("does not verify"), "{e}");
    }

    #[test]
    fn the_log_pulls_whole_and_a_knock_lands_in_the_inbox() {
        let f = Fake::default();
        let pins = pins();
        let b: Cell = serde_json::from_str(B).unwrap();
        f.vm(
            "46b16f13-11ac-5315-b431-845f196f907e",
            &["cell-46b16f13-11ac-5315-b431-845f196f907e"],
        );
        assert!(
            pull(&f, &pins, &b.id, "46b16f13-11ac-5315-b431-845f196f907e")
                .unwrap()
                .is_none(),
            "nothing before setup"
        );
        f.setup("46b16f13-11ac-5315-b431-845f196f907e", &b).unwrap();
        f.setup("46b16f13-11ac-5315-b431-845f196f907e", &b).unwrap();
        let log = pull(&f, &pins, &b.id, "46b16f13-11ac-5315-b431-845f196f907e")
            .unwrap()
            .unwrap();
        assert_eq!(log.len(), 2);
        assert!(matches!(log[1].1, Said::Checked { .. }));
        assert_eq!(log[1].0.seq, 2);
        let brief = Said::Brief {
            request: crate::RequestId::generate(),
            session: crate::SessionRef {
                cell: b.id.clone(),
                id: crate::SessionId::generate(),
            },
            text: "go".into(),
        };
        assert_eq!(
            f.knock("46b16f13-11ac-5315-b431-845f196f907e", &brief)
                .unwrap(),
            1
        );
        assert_eq!(
            f.inboxes.borrow()["46b16f13-11ac-5315-b431-845f196f907e"],
            [brief]
        );
    }

    #[test]
    fn github_integration_names() {
        let n = |r: &str, s| github_name(&r.parse().unwrap(), s);
        assert_eq!(n("example/widget", ""), "gh-example-widget");
        assert_eq!(n("example/widget", "-ro"), "gh-example-widget-ro");
        assert_eq!(n("Some-Org/my.repo_x", ""), "gh-some-org-my-repo-x");
    }
}
