//! Local development and cell execution require different resources.
use crate::{Cell, CellId};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub enum ExecutionMode {
    Local {
        listen: SocketAddr,
    },
    Cell {
        declaration: Cell,
        context: PathBuf,
        socket: PathBuf,
        agent_dir: PathBuf,
        inbox: PathBuf,
        outbox: PathBuf,
    },
}

#[derive(Clone)]
pub struct Config {
    pub mode: ExecutionMode,
    pub workspace: PathBuf,
    pub state: PathBuf,
    pub assets: PathBuf,
    pub pi: PathBuf,
}

impl Config {
    // Standalone local development has no provisioned cell. Its fixed UUID is
    // scoped to the local state directory and is never used as a VM binding.
    pub fn cell(&self) -> CellId {
        self.declaration()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "00000000-0000-4000-8000-000000000000".parse().unwrap())
    }
    pub fn context(&self) -> Option<&Path> {
        match &self.mode {
            ExecutionMode::Cell { context, .. } => Some(context),
            _ => None,
        }
    }
    pub fn declaration(&self) -> Option<&Cell> {
        match &self.mode {
            ExecutionMode::Cell { declaration, .. } => Some(declaration),
            _ => None,
        }
    }
    pub fn agent_dir(&self) -> Option<&Path> {
        match &self.mode {
            ExecutionMode::Cell { agent_dir, .. } => Some(agent_dir),
            _ => None,
        }
    }
    pub fn socket(&self) -> Option<&Path> {
        match &self.mode {
            ExecutionMode::Cell { socket, .. } => Some(socket),
            _ => None,
        }
    }
    pub fn inbox(&self) -> Option<&Path> {
        match &self.mode {
            ExecutionMode::Cell { inbox, .. } => Some(inbox),
            _ => None,
        }
    }
    pub fn outbox(&self) -> Option<&Path> {
        match &self.mode {
            ExecutionMode::Cell { outbox, .. } => Some(outbox),
            _ => None,
        }
    }
}
