//! The typed core of a kuggis cell.
//!
//! A cell is a root, a set of grants and a key. A durable [`Session`] belongs
//! to one cell; its identity survives runtime and model changes. Everything
//! a cell says is one signed envelope, [`Statement`], containing [`Said`].

pub mod apply;
pub mod canon;
pub mod cell;
pub mod chat;
pub mod connection;
pub mod contracts;
pub mod door;
pub mod exe;
pub mod environment;
pub mod grant;
pub mod identity;
mod legacy;
pub mod log;
pub mod migration;
pub mod operator;
pub mod pi;
pub mod plan;
pub mod prim;
pub mod provider;
pub mod proxy;
pub mod publish;
pub mod request;
pub mod resource;
pub mod resource_rpc;
pub mod said;
pub mod session;
pub mod ship;
pub mod tree;
pub mod wall;

pub use cell::Cell;
pub use exe::Observed;
pub use grant::{Grant, Network, Provider};
pub use plan::{Action, Approved, Plan, PlanError};
pub use prim::{CellId, Commit, Hash, Host, Invalid, Name, Repo, RequestId, SessionId, Url};
pub use said::{Key, Said, Secret, State, Statement, Unverified};
pub use session::{Prompt, Session, SessionAction, SessionCommand, SessionRef};
pub use tree::{Tree, TreeError};

pub mod context;
