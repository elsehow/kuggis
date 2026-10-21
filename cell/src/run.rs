//! The adapter: what runs the harness inside the wall. It watches the inbox
//! for briefs, runs each as a headless session in the agent's work
//! directory, and speaks through the outbox: Starting, Working as the model
//! talks, Done with its answer, or Blocked with what went wrong. The
//! harness-specific parts, the command line and the event stream, live here
//! and nowhere else; nothing below this module knows which harness runs.

use crate::cell::Cell;
use crate::door::Knock;
use crate::grant::{Grant, Harness, GITHUB_EDGE};
use crate::prim::Name;
use crate::said::{Said, State};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where sessions remember their harness ids, under the agent's home.
const SESSIONS: &str = ".cell-sessions.json";
/// The last inbox number handled.
const SEEN: &str = ".cell-seen";
/// How much of what the model says goes into a statement.
const CLIP: usize = 2000;

/// One line of the harness's event stream, as the cell understands it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The harness opened a session with this id; keep it to resume.
    Opened(String),
    Said(State, Option<String>),
}

fn clip(s: &str) -> String {
    if s.chars().count() <= CLIP {
        s.to_string()
    } else {
        let head: String = s.chars().take(CLIP).collect();
        format!("{head}…")
    }
}

/// Claude Code's `--output-format stream-json`, one JSON object per line.
pub fn translate(line: &str) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        "system" if v["subtype"] == "init" => v["session_id"]
            .as_str()
            .map(|s| Event::Opened(s.to_string())),
        "assistant" => {
            let text = v["message"]["content"]
                .as_array()?
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let text = (!text.trim().is_empty()).then(|| clip(text.trim()));
            Some(Event::Said(State::Working, text))
        }
        "result" => {
            let ok = v["subtype"] == "success" && v["is_error"] != true;
            let text = v["result"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| clip(s.trim()))
                .or_else(|| v["subtype"].as_str().map(str::to_string));
            Some(Event::Said(
                if ok { State::Done } else { State::Blocked },
                text,
            ))
        }
        _ => None,
    }
}

struct Agent {
    cell: Cell,
    inbox: PathBuf,
    outbox: PathBuf,
    home: PathBuf,
    sessions: BTreeMap<Name, String>,
    seen: u64,
    dropped: u64,
}

impl Agent {
    fn new(cell: Cell, inbox: &Path, outbox: &Path, home: &Path) -> io::Result<Agent> {
        let sessions = fs::read_to_string(home.join(SESSIONS))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let seen = fs::read_to_string(home.join(SEEN))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        Ok(Agent {
            cell,
            inbox: inbox.into(),
            outbox: outbox.into(),
            home: home.into(),
            sessions,
            seen,
            dropped: 0,
        })
    }

    fn work(&self) -> PathBuf {
        self.home.join("work")
    }

    /// Every granted repository, cloned once through the edge, with
    /// commits in the cell's name.
    fn clone_repos(&self) {
        let _ = fs::create_dir_all(self.work());
        for (key, value) in [
            ("user.name", self.cell.name.to_string()),
            ("user.email", format!("{}@cell.invalid", self.cell.name)),
        ] {
            let _ = Command::new("git")
                .args(["config", "--global", key, &value])
                .status();
        }
        for g in &self.cell.grants {
            let Grant::Code { repo, .. } = g.carried() else {
                continue;
            };
            let dir = self.work().join(repo.as_str());
            if dir.exists() {
                continue;
            }
            let url = format!("https://{GITHUB_EDGE}/{repo}.git");
            match Command::new("git")
                .args(["clone", "-q", &url])
                .arg(&dir)
                .output()
            {
                Ok(o) if o.status.success() => eprintln!("cloned {repo}"),
                Ok(o) => eprintln!(
                    "clone {repo}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                Err(e) => eprintln!("clone {repo}: {e}"),
            }
        }
    }

    fn new_knocks(&self) -> io::Result<Vec<u64>> {
        let mut found: Vec<u64> = fs::read_dir(&self.inbox)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".json")?
                    .parse::<u64>()
                    .ok()
            })
            .filter(|n| *n > self.seen)
            .collect();
        found.sort_unstable();
        Ok(found)
    }

    fn mark(&mut self, n: u64) -> io::Result<()> {
        self.seen = n;
        fs::write(self.home.join(SEEN), format!("{n}\n"))
    }

    /// Drop a session statement in the outbox for the shipper.
    fn say(&mut self, session: &Name, state: State, text: Option<String>) -> io::Result<()> {
        let said = Said::Session {
            name: session.clone(),
            harness: Harness::Claude,
            state,
            text,
        };
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        self.dropped += 1;
        let stem = format!("{ms}-{}", self.dropped);
        let tmp = self.outbox.join(format!("{stem}.tmp"));
        fs::write(&tmp, serde_json::to_vec(&said)?)?;
        fs::rename(&tmp, self.outbox.join(format!("{stem}.json")))?;
        eprintln!("{session}: {state:?}");
        Ok(())
    }

    fn handle(&mut self, n: u64, knock: Knock) -> io::Result<()> {
        let who = knock
            .from
            .as_ref()
            .map_or("you".to_string(), ToString::to_string);
        match knock.said {
            Said::Brief { to, session, text } if to == self.cell.name => {
                eprintln!("inbox {n}: a brief from {who} for {session}");
                self.session(session, &text)
            }
            other => {
                eprintln!("inbox {n}: {other} from {who}: nothing to do with it yet");
                Ok(())
            }
        }
    }

    /// Run one brief as a session: resume it if the harness has seen it.
    fn session(&mut self, name: Name, brief: &str) -> io::Result<()> {
        if !self.cell.harnesses().contains(&Harness::Claude) {
            eprintln!("{name}: the cell runs no harness I can drive");
            return Ok(());
        }
        self.say(&name, State::Starting, None)?;
        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            brief,
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
        ]);
        if let Some(id) = self.sessions.get(&name) {
            cmd.args(["--resume", id]);
        }
        cmd.current_dir(self.work())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return self.say(
                    &name,
                    State::Blocked,
                    Some(format!("cannot start claude: {e}")),
                )
            }
        };
        let mut stderr = child.stderr.take().expect("piped");
        let drain = thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut ended = false;
        for line in stdout.lines() {
            match translate(&line?) {
                Some(Event::Opened(id)) => {
                    self.sessions.insert(name.clone(), id);
                    fs::write(
                        self.home.join(SESSIONS),
                        serde_json::to_vec(&self.sessions)?,
                    )?;
                }
                Some(Event::Said(state, text)) => {
                    ended = matches!(state, State::Done | State::Blocked);
                    self.say(&name, state, text)?;
                }
                None => {}
            }
        }
        let status = child.wait()?;
        let err = drain.join().unwrap_or_default();
        if !ended {
            let lines: Vec<&str> = err.lines().collect();
            let tail = &lines[lines.len().saturating_sub(5)..];
            let why = format!("claude exited with {status}: {}", tail.join(" | "));
            self.say(&name, State::Blocked, Some(clip(&why)))?;
        }
        Ok(())
    }
}

/// Serve forever as the agent.
pub fn serve(cell: Cell, inbox: &Path, outbox: &Path, home: &Path) -> io::Result<()> {
    let mut me = Agent::new(cell, inbox, outbox, home)?;
    eprintln!(
        "cell {}: watching {} from {}, working in {}",
        me.cell.name,
        inbox.display(),
        me.seen,
        me.work().display()
    );
    me.clone_repos();
    loop {
        for n in me.new_knocks()? {
            match fs::read(me.inbox.join(format!("{n}.json")))
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<Knock>(&b).map_err(|e| e.to_string()))
            {
                Ok(knock) => me.handle(n, knock)?,
                Err(e) => eprintln!("inbox {n}: {e}"),
            }
            me.mark(n)?;
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_becomes_states() {
        assert_eq!(
            translate(r#"{"type":"system","subtype":"init","session_id":"abc","tools":[]}"#),
            Some(Event::Opened("abc".into()))
        );
        assert_eq!(
            translate(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t","name":"Read","input":{}}]}}"#
            ),
            Some(Event::Said(State::Working, Some("Looking.".into())))
        );
        assert_eq!(
            translate(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Read","input":{}}]}}"#
            ),
            Some(Event::Said(State::Working, None)),
            "a turn of tools alone is still working"
        );
        assert_eq!(
            translate(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Three files.","session_id":"abc"}"#
            ),
            Some(Event::Said(State::Done, Some("Three files.".into())))
        );
        assert_eq!(
            translate(
                r#"{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"abc"}"#
            ),
            Some(Event::Said(State::Blocked, Some("error_max_turns".into())))
        );
        assert_eq!(translate(r#"{"type":"user","message":{}}"#), None);
        assert_eq!(translate("not json"), None);
        let long = format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":"{}"}}"#,
            "x".repeat(5000)
        );
        let Some(Event::Said(State::Done, Some(text))) = translate(&long) else {
            panic!()
        };
        assert_eq!(text.chars().count(), CLIP + 1);
    }
}
