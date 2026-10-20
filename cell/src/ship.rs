//! The agent's voice. The agent cannot sign, so it drops a `Said` in the
//! outbox, and the shipper, the one user besides root that can read the
//! key, signs what it admits as the cell and appends it to the log. The
//! outbox file is renamed to `<name>.<seq>` as the receipt, or replaced by
//! `<name>.refused` holding the reason. The agent writes `<name>.tmp` and
//! renames it to `<name>.json`, so the shipper never sees half a file.
//!
//! Everything in the outbox is the adversary's. It is read without
//! following links, capped in size, parsed strictly, and checked against
//! the cell before the key touches it.

use crate::cell::Cell;
use crate::grant::Grant;
use crate::log;
use crate::said::{Said, Secret};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

pub const MAX_BYTES: usize = 65536;

/// What the agent may say, checked at the edge. The wall check alone says
/// `Checked`; a session names a harness the cell runs; a verdict names a
/// repository the cell holds. The rest is its to say, and the laptop or
/// the parent decides what it means.
pub fn admit(cell: &Cell, bytes: &[u8]) -> Result<Said, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("larger than {MAX_BYTES} bytes"));
    }
    let said: Said = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let holds = |repo| {
        cell.grants
            .iter()
            .any(|g| matches!(g, Grant::Code { repo: r, .. } if r == repo))
    };
    match &said {
        Said::Checked { .. } => Err("only the wall check says checked".into()),
        Said::Session { harness, .. } if !cell.harnesses().contains(harness) => {
            Err(format!("no grant to run {harness}"))
        }
        Said::Verdict { repo, .. } if !holds(repo) => Err(format!("no grant on {repo}")),
        _ => Ok(said),
    }
}

/// Read a regular file the agent left, never a link: the inode opened must
/// be the inode the name has, looked up without following.
fn read_regular(path: &Path) -> Result<Vec<u8>, String> {
    let named = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !named.is_file() {
        return Err("not a regular file".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if opened.ino() != named.ino() || opened.dev() != named.dev() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// One pass over the outbox, in name order: each `<name>.json` is signed
/// and renamed to its receipt, or refused. Returns what was done.
pub fn ship(
    cell: &Cell,
    secret: &Secret,
    outbox: &Path,
    said: &Path,
) -> io::Result<Vec<(String, Result<u64, String>)>> {
    let mut names: Vec<String> = fs::read_dir(outbox)?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    let mut done = Vec::new();
    for name in names {
        let path = outbox.join(&name);
        let stem = name.strip_suffix(".json").expect("filtered");
        let outcome = match read_regular(&path).and_then(|b| admit(cell, &b)) {
            Ok(s) => {
                let statement = log::append(said, secret, cell.name.clone(), s)?;
                fs::rename(&path, outbox.join(format!("{stem}.{}", statement.seq)))?;
                Ok(statement.seq)
            }
            Err(why) => {
                fs::write(outbox.join(format!("{stem}.refused")), format!("{why}\n"))?;
                if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
                Err(why)
            }
        };
        done.push((name, outcome));
    }
    Ok(done)
}

/// Serve forever, one log line per file.
pub fn serve(
    cell: &Cell,
    secret: &Secret,
    outbox: &Path,
    said: &Path,
    every: Duration,
) -> io::Result<()> {
    loop {
        for (name, outcome) in ship(cell, secret, outbox, said)? {
            match outcome {
                Ok(seq) => eprintln!("{name} -> {seq}"),
                Err(why) => eprintln!("{name} refused: {why}"),
            }
        }
        thread::sleep(every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::said::Statement;
    use std::path::PathBuf;

    const CELL: &str =
        r#"{"name":"a","grants":[{"agent":"claude"},{"code":{"repo":"elsehow/x","write":true}}]}"#;

    fn cell() -> Cell {
        serde_json::from_str(CELL).unwrap()
    }

    fn dirs(tag: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("cell-ship-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("outbox")).unwrap();
        fs::create_dir_all(d.join("said")).unwrap();
        (d.join("outbox"), d.join("said"))
    }

    #[test]
    fn what_the_agent_may_say() {
        let c = cell();
        let ok = |s: &str| admit(&c, s.as_bytes()).unwrap();
        let no = |s: &str| admit(&c, s.as_bytes()).unwrap_err();
        ok(r#"{"session":{"name":"s","harness":"claude","state":"question","text":"which?"}}"#);
        ok(r#"{"brief":{"to":"b","session":"s","text":"go"}}"#);
        ok(r#"{"fetch":{"url":"https://docs.example/"}}"#);
        ok(&format!(
            r#"{{"verdict":{{"repo":"elsehow/x","head":"{}","pass":true}}}}"#,
            "a".repeat(40)
        ));
        ok(&format!(
            r#"{{"reply":{{"of":"{}","body":{{"Ok":"yes"}}}}}}"#,
            "0".repeat(64)
        ));
        assert!(no(r#"{"checked":{"cell":"0000000000000000000000000000000000000000000000000000000000000000","ok":true,"report":""}}"#).contains("wall check"));
        assert!(
            no(r#"{"session":{"name":"s","harness":"codex","state":"working"}}"#)
                .contains("no grant to run \"codex\"")
        );
        assert!(no(&format!(
            r#"{{"verdict":{{"repo":"o/y","head":"{}","pass":true}}}}"#,
            "a".repeat(40)
        ))
        .contains("no grant on o/y"));
        assert!(no(r#"{"fetch":{"url":"https://x/","extra":1}}"#).contains("unknown field"));
        assert!(no("nonsense").contains("expected"));
        let big = format!(
            r#"{{"fetch":{{"url":"https://x.example/{}"}}}}"#,
            "a".repeat(MAX_BYTES)
        );
        assert!(no(&big).contains("larger"));
    }

    #[test]
    fn a_pass_signs_receipts_refuses_and_shares_the_log() {
        let c = cell();
        let secret = Secret::generate();
        let (outbox, said) = dirs("pass");
        let drop = |name: &str, body: &str| fs::write(outbox.join(name), body).unwrap();
        drop(
            "a.json",
            r#"{"session":{"name":"s","harness":"claude","state":"working"}}"#,
        );
        drop(
            "b.json",
            r#"{"checked":{"cell":"0000000000000000000000000000000000000000000000000000000000000000","ok":true,"report":""}}"#,
        );
        drop("c.json", "junk");
        drop("f.tmp", r#"{"fetch":{"url":"https://half.example/"}}"#);
        fs::create_dir(outbox.join("d.json")).unwrap();
        let elsewhere = outbox.join("..").join("fetch");
        fs::write(&elsewhere, r#"{"fetch":{"url":"https://linked.example/"}}"#).unwrap();
        std::os::unix::fs::symlink(&elsewhere, outbox.join("e.json")).unwrap();

        let done = ship(&c, &secret, &outbox, &said).unwrap();
        let names: Vec<&str> = done.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a.json", "b.json", "c.json", "d.json", "e.json"]);
        assert_eq!(done[0].1, Ok(1));
        assert!(done[1].1.as_ref().unwrap_err().contains("wall check"));
        assert!(done[2].1.is_err());
        assert_eq!(done[3].1.as_ref().unwrap_err(), "not a regular file");
        assert_eq!(done[4].1.as_ref().unwrap_err(), "not a regular file");

        assert!(outbox.join("a.1").exists() && !outbox.join("a.json").exists());
        assert!(fs::read_to_string(outbox.join("b.refused"))
            .unwrap()
            .contains("wall check"));
        assert!(!outbox.join("b.json").exists());
        assert!(!outbox.join("d.json").exists() && !outbox.join("e.json").exists());
        assert!(outbox.join("f.tmp").exists(), "a half-written file waits");
        assert!(elsewhere.exists(), "the link's target is untouched");

        // root's check appends to the same log, under the same lock
        let checked = log::append(
            &said,
            &secret,
            c.name.clone(),
            Said::Checked {
                cell: c.hash(),
                ok: true,
                report: String::new(),
                binary: crate::prim::Hash::ZERO,
                signed: Default::default(),
            },
        )
        .unwrap();
        assert_eq!(checked.seq, 2);
        fs::rename(outbox.join("f.tmp"), outbox.join("f.json")).unwrap();
        let done = ship(&c, &secret, &outbox, &said).unwrap();
        assert_eq!(done, [("f.json".to_string(), Ok(3))]);
        assert!(ship(&c, &secret, &outbox, &said).unwrap().is_empty());

        let key = secret.public();
        let mut last = None;
        for seq in 1..=3 {
            let s: Statement =
                serde_json::from_slice(&fs::read(said.join(format!("{seq}.json"))).unwrap())
                    .unwrap();
            assert_eq!(
                s.cell.as_str(),
                "a",
                "signed as the cell, whatever the agent wrote"
            );
            assert!(s.follows(last));
            s.open(&key).unwrap();
            last = Some(seq);
        }
    }
}
