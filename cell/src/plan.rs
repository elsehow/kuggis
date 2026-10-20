//! A plan is the diff between the cell you declared, with the doors the
//! tree gives it, and what exe.dev shows, plus whether the box itself is
//! running your declaration. Approval is the hash of the plan's bytes, and
//! applying recomputes the plan and refuses if anything moved.

use crate::canon;
use crate::cell::Cell;
use crate::grant::Grant;
use crate::prim::{Hash, Name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Action {
    /// Make the VM and tag it with the cell's name.
    Create,
    /// Attach an edge grant: an exe.dev integration, by the cell's tag.
    Attach(Grant),
    /// Detach one.
    Detach(Grant),
    /// Open a door to a child: a peer integration targeting the child's VM,
    /// attached to this cell's tag. The edge is the tree's, not the cell's.
    Open(Name),
    /// Close one.
    Close(Name),
    /// Push the cell file and the plumbing, run the bootstrap as root, and
    /// run the wall check. Reconciles every grant the edge cannot see.
    Setup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Hash of the declared cell: what the box must report in `Checked`.
    pub want: Hash,
    /// Hash of the cell as observed at the edge, or zero when there is no VM.
    pub have: Hash,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The approval does not match this plan.
    Stale { approved: Hash, current: Hash },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Stale { approved, current } => {
                write!(
                    f,
                    "approved {approved} but the plan is now {current}; review it again"
                )
            }
        }
    }
}

impl std::error::Error for PlanError {}

impl Plan {
    /// `want` is the declared cell and `children` the cells the tree puts
    /// under it. `have` is the cell and its doors as reconstructed from
    /// exe.dev's attachments, or `None` when no VM exists. `on_box` is the
    /// cell hash the box last reported in a `Checked` statement, or `None`
    /// when it never has.
    pub fn new(
        want: &Cell,
        children: &BTreeSet<Name>,
        have: Option<(&Cell, &BTreeSet<Name>)>,
        on_box: Option<&Hash>,
    ) -> Plan {
        let want_hash = want.hash();
        let mut actions = Vec::new();
        match have {
            None => {
                actions.push(Action::Create);
                actions.extend(want.edge_grants().cloned().map(Action::Attach));
                actions.extend(children.iter().cloned().map(Action::Open));
                actions.push(Action::Setup);
            }
            Some((have, doors)) => {
                for g in want.edge_grants() {
                    if !have.grants.contains(g) {
                        actions.push(Action::Attach(g.clone()));
                    }
                }
                for g in have.edge_grants() {
                    if !want.grants.contains(g) {
                        actions.push(Action::Detach(g.clone()));
                    }
                }
                actions.extend(children.difference(doors).cloned().map(Action::Open));
                actions.extend(doors.difference(children).cloned().map(Action::Close));
                if on_box != Some(&want_hash) {
                    actions.push(Action::Setup);
                }
            }
        }
        Plan {
            want: want_hash,
            have: have.map(|(c, _)| c.hash()).unwrap_or(Hash::ZERO),
            actions,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn canonical(&self) -> Vec<u8> {
        canon::bytes(self)
    }

    /// What the operator approves. Binds the inputs and every action.
    pub fn hash(&self) -> Hash {
        Hash::of(&self.canonical())
    }

    /// Apply only a plan whose hash the operator approved, recomputed now.
    pub fn approve(&self, approval: &Hash) -> Result<(), PlanError> {
        let current = self.hash();
        if *approval == current {
            Ok(())
        } else {
            Err(PlanError::Stale {
                approved: *approval,
                current,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::Harness;

    fn cell(json: &str) -> Cell {
        serde_json::from_str(json).unwrap()
    }

    fn names(list: &[&str]) -> BTreeSet<Name> {
        list.iter().map(|n| n.parse().unwrap()).collect()
    }

    const WANT: &str = r#"{"name":"airo","grants":[{"agent":"claude"},{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}},{"host":"pypi.org"}]}"#;

    #[test]
    fn a_new_cell() {
        let want = cell(WANT);
        let p = Plan::new(&want, &names(&["scratch"]), None, None);
        assert_eq!(p.have, Hash::ZERO);
        assert_eq!(
            p.actions,
            vec![
                Action::Create,
                Action::Attach(Grant::Agent(Harness::Codex)),
                Action::Attach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: true
                }),
                Action::Open("scratch".parse().unwrap()),
                Action::Setup,
            ]
        );
        assert_eq!(
            serde_json::to_value(&p.actions[3]).unwrap(),
            serde_json::json!({"open": "scratch"})
        );
    }

    #[test]
    fn a_settled_cell_plans_nothing() {
        let want = cell(WANT);
        let have = cell(
            r#"{"name":"airo","grants":[{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}}]}"#,
        );
        let doors = names(&["scratch"]);
        let p = Plan::new(&want, &doors, Some((&have, &doors)), Some(&want.hash()));
        assert!(p.is_empty(), "{:?}", p.actions);
    }

    #[test]
    fn drift_at_the_edge_and_on_the_box() {
        let want = cell(WANT);
        let have = cell(
            r#"{"name":"airo","grants":[{"code":{"repo":"elsehow/airo","write":false}},{"edge":"brave"}]}"#,
        );
        let stale = Hash::of(b"older cell");
        let p = Plan::new(
            &want,
            &names(&["scratch", "new"]),
            Some((&have, &names(&["scratch", "old"]))),
            Some(&stale),
        );
        assert_eq!(
            p.actions,
            vec![
                Action::Attach(Grant::Agent(Harness::Codex)),
                Action::Attach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: true
                }),
                Action::Detach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: false
                }),
                Action::Detach(Grant::Edge("brave".parse().unwrap())),
                Action::Open("new".parse().unwrap()),
                Action::Close("old".parse().unwrap()),
                Action::Setup,
            ]
        );
    }

    #[test]
    fn a_door_is_edge_work_and_leaves_the_box_alone() {
        let want = cell(r#"{"name":"a","grants":[{"agent":"claude"}]}"#);
        let none = BTreeSet::new();
        let p = Plan::new(
            &want,
            &names(&["b"]),
            Some((&want, &none)),
            Some(&want.hash()),
        );
        assert_eq!(p.actions, vec![Action::Open("b".parse().unwrap())]);
        let p = Plan::new(
            &want,
            &none,
            Some((&want, &names(&["b"]))),
            Some(&want.hash()),
        );
        assert_eq!(p.actions, vec![Action::Close("b".parse().unwrap())]);
    }

    #[test]
    fn box_side_grants_only_need_setup() {
        let want = cell(r#"{"name":"a","grants":[{"agent":"claude"},{"host":"pypi.org"}]}"#);
        let have = cell(r#"{"name":"a"}"#);
        let none = BTreeSet::new();
        let p = Plan::new(&want, &none, Some((&have, &none)), None);
        assert_eq!(p.actions, vec![Action::Setup]);
        let p = Plan::new(&want, &none, Some((&have, &none)), Some(&want.hash()));
        assert!(p.is_empty());
    }

    #[test]
    fn approval_binds_the_plan() {
        let want = cell(WANT);
        let none = BTreeSet::new();
        let p = Plan::new(&want, &none, None, None);
        let ok = p.hash();
        assert!(p.approve(&ok).is_ok());
        let mut moved = p.clone();
        moved.actions.pop();
        assert!(matches!(moved.approve(&ok), Err(PlanError::Stale { .. })));
        let mut widened = want.clone();
        widened
            .grants
            .insert(Grant::Host("evil.example".parse().unwrap()));
        let p2 = Plan::new(&widened, &none, None, None);
        assert!(p2.approve(&ok).is_err());
        let p3 = Plan::new(&want, &names(&["b"]), None, None);
        assert!(p3.approve(&ok).is_err(), "a door changes the plan");
        assert!(serde_json::from_str::<Plan>(r#"{"want":"00","have":"00","actions":[]}"#).is_err());
    }
}
