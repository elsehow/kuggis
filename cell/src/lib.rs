//! The typed core of a kuggis cell.
//!
//! A cell is a root, a set of grants and a key. Sixteen types carry the whole
//! design: six parsed primitives, and ten domain types. Everything a cell says
//! is one signed envelope, [`Statement`], with one enum inside, [`Said`].
//! Everything else is a function of these.

pub mod apply;
pub mod canon;
pub mod cell;
pub mod door;
pub mod exe;
pub mod grant;
pub mod log;
pub mod plan;
pub mod prim;
pub mod proxy;
pub mod run;
pub mod said;
pub mod ship;
pub mod tree;
pub mod wall;

pub use cell::Cell;
pub use exe::Observed;
pub use grant::{Grant, Harness};
pub use plan::{Action, Approved, Plan, PlanError};
pub use prim::{Commit, Hash, Host, Invalid, Name, Repo, Url};
pub use said::{Key, Said, Secret, State, Statement, Unverified};
pub use tree::{Tree, TreeError};
