//! A durable submission receipt. Claim before dispatch; an interrupted claim
//! remains uncertain and is never automatically dispatched a second time.

use crate::{Hash, SessionCommand};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Receipt {
    Pending,
    Accepted,
    NotDispatched { status: u16, message: String },
    Rejected { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    command: Hash,
    receipt: Receipt,
}

/// Only a fresh claim grants permission to dispatch. Dropping it preserves
/// the uncertain receipt, including after a crash or a lost acknowledgement.
#[derive(Debug)]
pub enum Claim {
    Fresh(DispatchPermit),
    Recorded(Receipt),
}
#[derive(Debug)]
pub struct DispatchPermit {
    directory: PathBuf,
    path: PathBuf,
    command: Hash,
}

pub enum SubmissionOutcome {
    Accepted,
    NotDispatched { status: u16, message: String },
    Rejected { message: String },
}

pub struct RequestJournal(PathBuf);

impl RequestJournal {
    pub fn new(path: &Path) -> io::Result<Self> {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(Self(path.into()))
    }

    pub fn claim(&self, command: &SessionCommand) -> io::Result<Claim> {
        let path = self.0.join(format!("{}.json", command.request));
        let hash = Hash::of(&crate::canon::bytes(command));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                let entry = Entry {
                    command: hash,
                    receipt: Receipt::Pending,
                };
                file.write_all(&serde_json::to_vec(&entry)?)?;
                file.sync_all()?;
                fs::File::open(&self.0)?.sync_all()?;
                Ok(Claim::Fresh(DispatchPermit {
                    directory: self.0.clone(),
                    path,
                    command: hash,
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let entry: Entry = serde_json::from_slice(&fs::read(path)?)?;
                if entry.command != hash {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "request ID was already used for a different command",
                    ));
                }
                Ok(Claim::Recorded(entry.receipt))
            }
            Err(e) => Err(e),
        }
    }
}

impl DispatchPermit {
    /// Consumes the capability: a caller cannot finish an unclaimed request or
    /// accidentally replace a prior acceptance with a later failure.
    pub fn finish(self, outcome: SubmissionOutcome) -> io::Result<()> {
        let receipt = match outcome {
            SubmissionOutcome::Accepted => Receipt::Accepted,
            SubmissionOutcome::NotDispatched { status, message } => {
                Receipt::NotDispatched { status, message }
            }
            SubmissionOutcome::Rejected { message } => Receipt::Rejected { message },
        };
        let temp = self.path.with_extension("tmp");
        let entry = Entry {
            command: self.command,
            receipt,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(&entry)?)?;
        file.sync_all()?;
        fs::rename(temp, self.path)?;
        fs::File::open(&self.directory)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestId, Session, SessionAction};

    #[test]
    fn receipts_survive_restart_and_refuse_reuse_for_different_commands() {
        let path = std::env::temp_dir().join(format!("cell-requests-{}", RequestId::generate()));
        let journal = RequestJournal::new(&path).unwrap();
        let session = Session::new(
            "b48244b7-ed96-57fa-a3a2-e7629b8c921e".parse().unwrap(),
            "".into(),
        );
        let mut command = SessionCommand {
            request: RequestId::generate(),
            session: session.reference(),
            action: SessionAction::Cancel,
        };
        let Claim::Fresh(permit) = journal.claim(&command).unwrap() else {
            panic!("new request")
        };
        assert!(matches!(
            RequestJournal::new(&path).unwrap().claim(&command).unwrap(),
            Claim::Recorded(Receipt::Pending)
        ));
        permit.finish(SubmissionOutcome::Accepted).unwrap();
        assert!(matches!(
            RequestJournal::new(&path).unwrap().claim(&command).unwrap(),
            Claim::Recorded(Receipt::Accepted)
        ));
        command.session.cell = "70ee78ad-5e3a-510a-af20-6f485ecb6f16".parse().unwrap();
        assert_eq!(
            journal.claim(&command).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        fs::remove_dir_all(path).unwrap();
    }
}
