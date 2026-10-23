//! The statement log on a box: `<seq>.json`, one signed statement each,
//! appended under one lock by the two writers that can read the key,
//! root's wall check and the shipper, so no two statements take one number.

use crate::prim::CellId;
use crate::said::{Said, Secret, Statement};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The lock file in the log directory. Opened read-only by whoever appends;
/// the lock is on the open file, not its contents.
pub const LOCK: &str = ".lock";

/// The next free number in a directory of `<n>.json`.
pub fn next_number(dir: &Path) -> io::Result<u64> {
    let mut last = 0u64;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(n) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .and_then(|s| s.parse::<u64>().ok())
        {
            last = last.max(n);
        }
    }
    last.checked_add(1)
        .ok_or_else(|| io::Error::other("numbers exhausted"))
}

/// Sign `said` as the cell's next statement and write it, world-readable,
/// holding the log's lock from numbering to rename.
pub fn append(dir: &Path, secret: &Secret, cell: CellId, said: Said) -> io::Result<Statement> {
    let lock = lock(dir)?;
    lock.lock()?;
    let seq = next_number(dir)?;
    let statement = Statement::sign(secret, cell, seq, said);
    let tmp = dir.join(format!("{seq}.json.tmp"));
    fs::write(
        &tmp,
        serde_json::to_vec(&statement).map_err(io::Error::other)?,
    )?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, dir.join(format!("{seq}.json")))?;
    Ok(statement)
}

fn lock(dir: &Path) -> io::Result<File> {
    let path = dir.join(LOCK);
    match File::open(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cell-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn two_writers_never_take_one_number() {
        let dir = Arc::new(dir());
        let secret = Arc::new(Secret::generate());
        let name: CellId = "904254dc-1b8f-5e80-9755-e5afc5b195c5".parse().unwrap();
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let (dir, secret, name) = (Arc::clone(&dir), Arc::clone(&secret), name.clone());
                thread::spawn(move || {
                    for _ in 0..25 {
                        append(
                            &dir,
                            &secret,
                            name.clone(),
                            Said::Fetch {
                                url: "https://x.example/".parse().unwrap(),
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        assert_eq!(next_number(&dir).unwrap(), 101);
        let mut last = None;
        for seq in 1..=100 {
            let s: Statement =
                serde_json::from_slice(&fs::read(dir.join(format!("{seq}.json"))).unwrap())
                    .unwrap();
            assert!(s.follows(last));
            assert!(s.open(&secret.public()).is_ok());
            last = Some(s.seq);
        }
        assert!(dir.join(LOCK).exists());
        assert!(!dir.join("1.json.tmp").exists());
    }
}
