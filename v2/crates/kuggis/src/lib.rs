//! Flat, deployment-configured workspaces. No agent or approval protocol.
//!
//! Pure configuration/policy compilation is separate from SSH and Linux effects.
//! A rendered description is not evidence that a live boundary has passed checks.

pub mod client;
pub mod config;
pub mod deploy;
pub mod environment;
pub mod network;
pub mod onboarding;
pub mod proxy;
pub mod ssh;
pub mod transfer;
pub mod web;

pub use config::Workspace;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}
