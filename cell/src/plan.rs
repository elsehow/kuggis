//! A plan is the diff between the cell you declared, with the doors the
//! tree gives it, and what exe.dev shows, plus whether the box itself is
//! running your declaration. The operator approves every cell's plan at
//! once, by one hash, and apply re-plans before each action: a fresh plan
//! may only shrink, or the loop stops.

use crate::canon;
use crate::cell::Cell;
use crate::grant::Grant;
use crate::prim::{Hash, Name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        canon::display(self, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Hash of the declared cell: what the box must report in `Checked`.
    pub want: Hash,
    /// Hash of the cell as observed at the edge, or zero when there is no VM.
    pub have: Hash,
    /// Hash of the binary setup would ship, or zero when planning without one.
    #[serde(default = "Hash::zero", skip_serializing_if = "Hash::is_zero")]
    pub bin: Hash,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The approval does not match these plans.
    Stale { approved: Hash, current: Hash },
    /// A cell's declaration is not the one approved, or it was never approved.
    Changed(Name),
    /// A fresh plan asks for something the approved one did not.
    Beyond { cell: Name, action: Action },
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
            PlanError::Changed(cell) => write!(f, "{cell}: not what was approved; plan again"),
            PlanError::Beyond { cell, action } => {
                write!(f, "{cell}: {action} was not approved; plan again")
            }
        }
    }
}

impl std::error::Error for PlanError {}

impl Plan {
    /// `want` is the declared cell and `children` the cells the tree puts
    /// under it. `have` is the cell and its doors as reconstructed from
    /// exe.dev's attachments, or `None` when no VM exists. `on_box` is what
    /// the box last proved in a `Checked` that passed, its cell hash and its
    /// binary's, or `None` when it never has. `bin` is the hash of the
    /// binary setup would ship; given, a box running another needs setup.
    pub fn new(
        want: &Cell,
        children: &BTreeSet<Name>,
        have: Option<(&Cell, &BTreeSet<Name>)>,
        on_box: Option<(Hash, Hash)>,
        bin: Option<Hash>,
    ) -> Plan {
        let want_hash = want.hash();
        let proved = on_box
            .is_some_and(|(cell, binary)| cell == want_hash && bin.is_none_or(|b| b == binary));
        let mut actions = Vec::new();
        match have {
            None => {
                actions.push(Action::Create);
                actions.extend(want.edge_grants().map(Grant::carried).map(Action::Attach));
                actions.extend(children.iter().cloned().map(Action::Open));
                actions.push(Action::Setup);
            }
            Some((have, doors)) => {
                let carried: BTreeSet<Grant> = want.edge_grants().map(Grant::carried).collect();
                for g in &carried {
                    if !have.grants.contains(g) {
                        actions.push(Action::Attach(g.clone()));
                    }
                }
                for g in have.edge_grants() {
                    if !carried.contains(g) {
                        actions.push(Action::Detach(g.clone()));
                    }
                }
                actions.extend(children.difference(doors).cloned().map(Action::Open));
                actions.extend(doors.difference(children).cloned().map(Action::Close));
                if !proved {
                    actions.push(Action::Setup);
                }
            }
        }
        Plan {
            want: want_hash,
            have: have.map(|(c, _)| c.hash()).unwrap_or(Hash::ZERO),
            bin: bin.unwrap_or(Hash::ZERO),
            actions,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Every cell's plan, approved as one. It is the only thing apply takes,
/// so an unapproved plan cannot be applied by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved(BTreeMap<Name, Plan>);

impl Approved {
    /// What the operator reads and repeats back: one hash over every plan.
    pub fn hash(plans: &BTreeMap<Name, Plan>) -> Hash {
        Hash::of(&canon::bytes(plans))
    }

    pub fn new(plans: BTreeMap<Name, Plan>, approval: &Hash) -> Result<Approved, PlanError> {
        let current = Approved::hash(&plans);
        if *approval == current {
            Ok(Approved(plans))
        } else {
            Err(PlanError::Stale {
                approved: *approval,
                current,
            })
        }
    }

    pub fn plans(&self) -> &BTreeMap<Name, Plan> {
        &self.0
    }

    /// The subset rule. A plan recomputed after some actions ran may ask
    /// only for actions the approved one asked for, toward the same
    /// declaration, shipping the same binary.
    pub fn permits(&self, cell: &Name, fresh: &Plan) -> Result<(), PlanError> {
        let approved = self
            .0
            .get(cell)
            .filter(|p| p.want == fresh.want && p.bin == fresh.bin);
        let Some(approved) = approved else {
            return Err(PlanError::Changed(cell.clone()));
        };
        match fresh.actions.iter().find(|a| !approved.actions.contains(a)) {
            None => Ok(()),
            Some(action) => Err(PlanError::Beyond {
                cell: cell.clone(),
                action: action.clone(),
            }),
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
        let p = Plan::new(&want, &names(&["scratch"]), None, None, None);
        assert_eq!(p.have, Hash::ZERO);
        assert_eq!(
            p.actions,
            vec![
                Action::Create,
                Action::Attach(Grant::Agent(Harness::Codex)),
                Action::Attach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: true,
                    via: None
                }),
                Action::Open("scratch".parse().unwrap()),
                Action::Setup,
            ]
        );
        assert_eq!(
            serde_json::to_value(&p.actions[3]).unwrap(),
            serde_json::json!({"open": "scratch"})
        );
        assert_eq!(p.actions[3].to_string(), r#"{"open":"scratch"}"#);
    }

    #[test]
    fn a_settled_cell_plans_nothing() {
        let want = cell(WANT);
        let have = cell(
            r#"{"name":"airo","grants":[{"agent":"codex"},{"code":{"repo":"elsehow/airo","write":true}}]}"#,
        );
        let doors = names(&["scratch"]);
        let p = Plan::new(
            &want,
            &doors,
            Some((&have, &doors)),
            Some((want.hash(), Hash::ZERO)),
            None,
        );
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
            Some((stale, Hash::ZERO)),
            None,
        );
        assert_eq!(
            p.actions,
            vec![
                Action::Attach(Grant::Agent(Harness::Codex)),
                Action::Attach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: true,
                    via: None
                }),
                Action::Detach(Grant::Code {
                    repo: "elsehow/airo".parse().unwrap(),
                    write: false,
                    via: None
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
            Some((want.hash(), Hash::ZERO)),
            None,
        );
        assert_eq!(p.actions, vec![Action::Open("b".parse().unwrap())]);
        let p = Plan::new(
            &want,
            &none,
            Some((&want, &names(&["b"]))),
            Some((want.hash(), Hash::ZERO)),
            None,
        );
        assert_eq!(p.actions, vec![Action::Close("b".parse().unwrap())]);
    }

    #[test]
    fn box_side_grants_only_need_setup() {
        let want = cell(r#"{"name":"a","grants":[{"agent":"claude"},{"host":"pypi.org"}]}"#);
        let have = cell(r#"{"name":"a"}"#);
        let none = BTreeSet::new();
        let p = Plan::new(&want, &none, Some((&have, &none)), None, None);
        assert_eq!(p.actions, vec![Action::Setup]);
        let settled = Some((want.hash(), Hash::ZERO));
        let p = Plan::new(&want, &none, Some((&have, &none)), settled, None);
        assert!(p.is_empty());
    }

    #[test]
    fn a_new_binary_needs_setup_and_the_approval_binds_it() {
        let want = cell(r#"{"name":"a","grants":[{"agent":"claude"}]}"#);
        let none = BTreeSet::new();
        let (old, new) = (Hash::of(b"v1"), Hash::of(b"v2"));
        let have = Some((&want, &none));
        let runs = Some((want.hash(), old));
        assert!(Plan::new(&want, &none, have, runs, Some(old)).is_empty());
        assert!(Plan::new(&want, &none, have, runs, None).is_empty());
        let p = Plan::new(&want, &none, have, runs, Some(new));
        assert_eq!(p.actions, vec![Action::Setup]);
        assert_eq!(p.bin, new);
        // a box that never said which binary it runs is upgraded too
        let silent = Some((want.hash(), Hash::ZERO));
        let p = Plan::new(&want, &none, have, silent, Some(new));
        assert_eq!(p.actions, vec![Action::Setup]);

        let plans = BTreeMap::from([(want.name.clone(), p.clone())]);
        let approved = Approved::new(plans.clone(), &Approved::hash(&plans)).unwrap();
        assert!(approved.permits(&want.name, &p).is_ok());
        let other = Plan::new(&want, &none, have, runs, Some(Hash::of(b"v3")));
        assert!(matches!(
            approved.permits(&want.name, &other),
            Err(PlanError::Changed(_))
        ));
    }

    #[test]
    fn approval_binds_every_plan_and_a_fresh_plan_may_only_shrink() {
        let want = cell(WANT);
        let none = BTreeSet::new();
        let p = Plan::new(&want, &none, None, None, None);
        let plans = BTreeMap::from([(want.name.clone(), p.clone())]);
        let ok = Approved::hash(&plans);
        assert!(matches!(
            Approved::new(plans.clone(), &Hash::ZERO),
            Err(PlanError::Stale { .. })
        ));
        let approved = Approved::new(plans, &ok).unwrap();
        assert_eq!(approved.plans().len(), 1);

        let mut less = p.clone();
        less.actions.remove(0);
        assert!(approved.permits(&want.name, &less).is_ok());
        let mut more = p.clone();
        more.actions.push(Action::Open("b".parse().unwrap()));
        assert!(matches!(
            approved.permits(&want.name, &more),
            Err(PlanError::Beyond { .. })
        ));

        let mut widened = want.clone();
        widened
            .grants
            .insert(Grant::Host("evil.example".parse().unwrap()));
        let p2 = Plan::new(&widened, &none, None, None, None);
        assert!(matches!(
            approved.permits(&want.name, &p2),
            Err(PlanError::Changed(_))
        ));
        assert!(approved.permits(&"b".parse().unwrap(), &p).is_err());

        let p3 = Plan::new(&want, &names(&["b"]), None, None, None);
        assert_ne!(
            Approved::hash(&BTreeMap::from([(want.name.clone(), p3)])),
            ok,
            "a door changes the plan"
        );
        assert!(serde_json::from_str::<Plan>(r#"{"want":"00","have":"00","actions":[]}"#).is_err());
    }
}
