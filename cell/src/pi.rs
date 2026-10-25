//! Pi's JSONL subprocess protocol. No model/provider logic lives here.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
pub(crate) mod models;
pub(crate) mod protocol;
use protocol::AssistantDelta;
use protocol::{Command as RpcCommand, CommandEnvelope, Response};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEntries {
    leaf_id: Option<String>,
    entries: Vec<SessionEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEntry {
    id: String,
    parent_id: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    is_sidechain: Option<bool>,
    message: Option<Value>,
    custom_type: Option<String>,
    content: Option<Value>,
    display: Option<bool>,
    details: Option<Value>,
    summary: Option<String>,
    from_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TranscriptMessage {
    role: String,
}
/// Whether a failed command could have reached Pi. This says nothing about
/// the eventual model result of a successfully accepted prompt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallError {
    NotDispatched { message: String },
    Rejected { message: String },
    OutcomeUnknown { message: String },
}
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotDispatched { message }
            | Self::Rejected { message }
            | Self::OutcomeUnknown { message } => f.write_str(message),
        }
    }
}
impl std::error::Error for CallError {}
type CallResult<T> = std::result::Result<T, CallError>;
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<CallResult<Value>>>>>;

// Kuggis owns the host instructions; Pi supplies the loop, tools and providers.
// Workspace context files still load normally. Pi's CLI-specific default prompt
// can cause subscription requests to fail with an unrelated extra-usage error.
// Every Kuggis prompt starts with SYSTEM_PROMPT, which marks a resumed
// session's system section as ours to bring up to date.
const SYSTEM_PROMPT: &str = "You are an assistant working in this workspace. Use the available tools to carry out the user's requests. Follow the workspace instructions, keep changes focused, and verify your work.";
const GUEST_PROMPT_SUFFIX: &str = "You control this persistent Linux environment and have passwordless sudo. You may install toolchains and packages, change system configuration, and run local services. Use the configured HTTPS proxy for external downloads; the host enforces network grants. Changes survive restarts. The host signing key and authority controls are outside your environment.";

fn instructions(shared: &Path, guest: bool) -> String {
    let shared = shared.display();
    let prompt = format!("{SYSTEM_PROMPT} To give the user a file or folder, copy it into {shared} and link its absolute path in Markdown, for example [Results]({shared}/results). Paths with spaces can use angle brackets, for example [Results](<{shared}/my results>). These links open the cell UI's read-only Files tab, which shows only {shared}; the user downloads files there, and folders as .tar.gz archives. Copy rather than symlink, because links leading out of {shared} cannot be downloaded. Verify that the path exists before linking it. Do not invent a host path or require SSH for downloads.");
    if guest {
        format!("{prompt} {GUEST_PROMPT_SUFFIX}")
    } else {
        prompt
    }
}

/// Pi 1.3 retains previous system sections in resumed request context. Replace
/// only its identifiable stock instructions and earlier Kuggis instructions with
/// `prompt`, keeping conversation bytes, entry IDs, branches and tool
/// definitions intact. Called before the child starts, under the chat server's
/// state lock and the session's process lock.
fn upgrade_instructions(sessions: &Path, id: &str, prompt: &str) -> std::io::Result<()> {
    if id.parse::<crate::SessionId>().is_err() {
        return Ok(());
    }
    for entry in fs::read_dir(sessions)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || !entry
                .file_name()
                .to_string_lossy()
                .ends_with(&format!("_{id}.jsonl"))
        {
            continue;
        }
        let path = entry.path();
        let original = fs::read_to_string(&path)?;
        let mut lines = original.split_inclusive('\n');
        let Some(header) = lines.next() else {
            continue;
        };
        let header: Value = serde_json::from_str(header)?;
        if header["type"] != "session" || header["id"] != id {
            return Err(std::io::Error::other(
                "Pi session header does not match its filename",
            ));
        }
        let mut updated = String::new();
        let mut changed = false;
        for line in original.split_inclusive('\n') {
            if line.trim().is_empty() {
                updated.push_str(line);
                continue;
            }
            let mut record: Value = serde_json::from_str(line)?;
            let message = &mut record["message"];
            let system = message["role"] == "system";
            let preamble = message["sections"]["preamble"].as_str().unwrap_or_default();
            let stock = system
                && preamble.starts_with("You are an expert coding assistant operating inside pi, a coding agent harness.")
                && message["sections"]["docs"].as_str().is_some_and(|s| s.starts_with("<docs>\nPi documentation (read only when the user asks about pi itself"));
            let outdated = system && preamble.starts_with(SYSTEM_PROMPT) && preamble != prompt;
            if stock || outdated {
                message["sections"]["preamble"] = json!(prompt);
                if stock {
                    message["sections"]["docs"] = Value::Null;
                }
                updated.push_str(&serde_json::to_string(&record)?);
                if line.ends_with('\n') {
                    updated.push('\n');
                }
                changed = true;
            } else {
                updated.push_str(line);
            }
        }
        if !changed {
            continue;
        }
        let backup = path.with_extension(format!(
            "jsonl.before-workspace-prompt-{}",
            crate::Hash::of(original.as_bytes())
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(original.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&backup)? != original.as_bytes() {
                    return Err(std::io::Error::other(
                        "Pi instruction backup differs from its source",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        fs::File::open(sessions)?.sync_all()?;
        let temp = path.with_extension(format!("{}.tmp", crate::RequestId::generate()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        fs::File::open(sessions)?.sync_all()?;
    }
    Ok(())
}

pub struct Rpc {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
}

impl Rpc {
    pub fn spawn(
        bin: &Path,
        workspace: &Path,
        shared: &Path,
        sessions: &Path,
        id: &str,
        agent_dir: Option<&Path>,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Value>)> {
        let system_prompt = instructions(
            shared,
            Path::new("/run/cell-runtime/environment-guest.py").exists(),
        );
        upgrade_instructions(sessions, id, &system_prompt)
            .map_err(|e| format!("prepare Pi instructions: {e}"))?;
        let mut command = Command::new(bin);
        if let Some(dir) = agent_dir {
            command.env("PI_CODING_AGENT_DIR", dir);
        }
        let mut child = command
            .args(["--system-prompt", &system_prompt])
            .args(["--mode", "rpc", "--session-id", id, "--session-dir"])
            .arg(sessions)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("start Pi ({}): {e}", bin.display()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (events, rx) = mpsc::channel(256);
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (stop, mut stopping) = watch::channel(false);
        let rpc = Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending: pending.clone(),
            next: AtomicU64::new(0),
            alive: alive.clone(),
            stop,
        });
        tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            let reason = loop {
                tokio::select! {
                    _ = stopping.changed() => {
                        let _ = child.kill().await;
                        break "Pi stopped".to_string();
                    }
                    line = record(&mut stdout) => {
                        match line {
                            Ok(Some(line)) => {
                                let value: Value = match serde_json::from_str(&line) {
                                    Ok(v) => v,
                                    Err(e) => break format!("invalid Pi JSONL: {e}"),
                                };
                                if value["type"] == "response" {
                                    let response: Response = match serde_json::from_value(value) {
                                        Ok(response) => response,
                                        Err(e) => break format!("invalid Pi response: {e}"),
                                    };
                                    if let Some(tx) = pending.lock().await.remove(&response.id) {
                                        let reply = if response.success { Ok(response.data) }
                                            else { Err(CallError::Rejected { message: response.error.unwrap_or_else(|| "Pi command failed".into()) }) };
                                        let _ = tx.send(reply);
                                    }
                                } else if events.send(value).await.is_err() { break "session closed".into(); }
                            }
                            Ok(None) => break "Pi exited; reopen this chat to resume its saved session".into(),
                            Err(e) => break format!("read Pi: {e}"),
                        }
                    }
                }
            };
            alive.store(false, Ordering::SeqCst);
            for (_, tx) in pending.lock().await.drain() {
                let _ = tx.send(Err(CallError::OutcomeUnknown {
                    message: reason.clone(),
                }));
            }
            let _ = events.send(json!({"type":"error", "error":reason})).await;
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        Ok((rpc, rx))
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    pub(crate) async fn write(&self, value: impl serde::Serialize) -> CallResult<()> {
        if !self.alive() {
            return Err(CallError::NotDispatched {
                message: "Pi is not running; reopen this chat to resume".into(),
            });
        }
        let mut bytes = serde_json::to_vec(&value).map_err(|e| CallError::NotDispatched {
            message: e.to_string(),
        })?;
        bytes.push(b'\n');
        self.stdin
            .lock()
            .await
            .write_all(&bytes)
            .await
            .map_err(|e| CallError::OutcomeUnknown {
                message: e.to_string(),
            })
    }

    pub(crate) async fn call<T: serde::de::DeserializeOwned>(
        &self,
        command: RpcCommand<'_>,
    ) -> CallResult<T> {
        let id = format!("rpc-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let command = CommandEnvelope {
            id: id.clone(),
            command,
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        let result = match tokio::time::timeout(Duration::from_secs(30), async {
            self.write(command).await?;
            rx.await.map_err(|_| CallError::OutcomeUnknown {
                message: "Pi closed the command channel".into(),
            })?
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(CallError::OutcomeUnknown {
                message: "Pi command timed out; its outcome is unknown".into(),
            }),
        };
        self.pending.lock().await.remove(&id);
        result.and_then(|value| {
            serde_json::from_value(value).map_err(|e| CallError::OutcomeUnknown {
                message: format!("Invalid Pi response body: {e}"),
            })
        })
    }
}

/// LF only, with a memory limit even when a child never finishes its record.
pub(crate) async fn record(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        let end = available.iter().position(|b| *b == b'\n');
        let len = end.map_or(available.len(), |n| n + 1);
        if line.len() + len > 16 * 1024 * 1024 {
            return Err(std::io::Error::other("Pi record exceeded 16 MiB"));
        }
        line.extend_from_slice(&available[..len]);
        reader.consume(len);
        if end.is_some() {
            break;
        }
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(std::io::Error::other)
}

/// Display the complete active branch, including messages before compaction.
/// Pi's get_messages is only the model's current, possibly compacted context.
pub fn transcript(entries: &Value) -> Result<Vec<Value>> {
    let parsed: SessionEntries = serde_json::from_value(entries.clone())
        .map_err(|e| format!("Pi returned malformed session entries: {e}"))?;
    let raw = entries["entries"]
        .as_array()
        .ok_or("Pi omitted session entries")?;
    if raw.len() != parsed.entries.len() {
        return Err("Pi session entries changed while parsing".into());
    }
    let Some(leaf_id) = parsed.leaf_id.as_deref() else {
        return if parsed.entries.is_empty() {
            Ok(Vec::new())
        } else {
            Err("Pi omitted the active session leaf".into())
        };
    };
    if leaf_id.is_empty() {
        return Err("Pi returned an empty active session leaf".into());
    }
    let mut by_id = HashMap::new();
    for (index, entry) in parsed.entries.iter().enumerate() {
        if entry.id.is_empty() || by_id.insert(entry.id.as_str(), index).is_some() {
            return Err("Pi session has an empty or duplicate entry ID".into());
        }
        match entry.kind.as_str() {
            "message" => {
                let message = entry
                    .message
                    .as_ref()
                    .ok_or("Pi message entry has no message")?;
                serde_json::from_value::<TranscriptMessage>(message.clone())
                    .map_err(|e| format!("Pi message entry is malformed: {e}"))?;
            }
            "custom_message" => {
                if entry.custom_type.as_deref().is_none_or(str::is_empty)
                    || raw[index].get("content").is_none()
                {
                    return Err("Pi custom message entry is malformed".into());
                }
            }
            "branch_summary" => {
                if entry.summary.is_none() || entry.from_id.is_none() {
                    return Err("Pi branch summary entry is malformed".into());
                }
            }
            _ => {}
        }
    }
    let mut branch = Vec::new();
    let mut id = Some(leaf_id);
    while let Some(current) = id {
        if branch.len() >= parsed.entries.len() {
            return Err("Pi session has a cyclic branch".into());
        }
        let index = by_id
            .get(current)
            .ok_or("Pi session has a missing parent entry")?;
        branch.push(*index);
        id = parsed.entries[*index].parent_id.as_deref();
    }
    let mut messages = Vec::new();
    for index in branch.into_iter().rev() {
        let entry = &parsed.entries[index];
        match entry.kind.as_str() {
            "message" if entry.is_sidechain != Some(true) => {
                let message = entry.message.as_ref().unwrap();
                let typed: TranscriptMessage = serde_json::from_value(message.clone()).unwrap();
                if typed.role != "system" {
                    messages.push(message.clone());
                }
            }
            "custom_message" => messages
                .push(json!({"role":"custom", "customType":entry.custom_type,
                "content":entry.content, "display":entry.display, "details":entry.details})),
            "branch_summary" => messages.push(
                json!({"role":"branchSummary","summary":entry.summary,"fromId":entry.from_id}),
            ),
            _ => {}
        }
    }
    Ok(messages)
}

/// Pi 1.2 sends deltas on the wire; react-pi expects cumulative messages.
/// Keep authoritative message_end content, including opaque provider fields.
pub fn apply_delta(message: &mut Value, event: &Value) {
    if let Some(full) = event.get("message") {
        *message = full.clone();
        return;
    }
    let Ok(delta) =
        serde_json::from_value::<AssistantDelta>(event["assistantMessageEvent"].clone())
    else {
        return;
    };
    apply_delta_typed(message, event, &delta);
}

pub(crate) fn apply_delta_typed(message: &mut Value, event: &Value, delta: &AssistantDelta) {
    let delta_value = &event["assistantMessageEvent"];
    if let Some(full) = delta_value.get("partial") {
        *message = full.clone();
        return;
    }
    if let Some(usage) = event.get("usage") {
        message["usage"] = usage.clone();
    }
    let Some(index) = delta.content_index.map(|i| i as usize) else {
        return;
    };
    // Reject an implausible index without allocating an unbounded sparse array.
    if index > 4096 {
        return;
    }
    let Some(content) = message["content"].as_array_mut() else {
        return;
    };
    while content.len() <= index {
        content.push(json!({}));
    }
    let block = &mut content[index];
    match delta.kind.as_str() {
        "text_start" => *block = json!({"type":"text", "text":""}),
        "thinking_start" => *block = json!({"type":"thinking", "thinking":""}),
        "text_delta" | "thinking_delta" => {
            let (kind, field) = if delta.kind == "text_delta" {
                ("text", "text")
            } else {
                ("thinking", "thinking")
            };
            let text = format!(
                "{}{}",
                block[field].as_str().unwrap_or(""),
                delta.delta.as_deref().unwrap_or("")
            );
            block["type"] = json!(kind);
            block[field] = json!(text);
        }
        "text_end" => *block = json!({"type":"text", "text":delta_value["content"]}),
        "thinking_end" => *block = json!({"type":"thinking", "thinking":delta_value["content"]}),
        "toolcall_start" => {
            *block = json!({"type":"toolCall", "id":delta_value["id"], "name":delta_value["toolName"], "arguments":{}})
        }
        "toolcall_end" => *block = delta_value["toolCall"].clone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumed_sessions_upgrade_only_stock_instructions_with_a_durable_backup() {
        let directory =
            std::env::temp_dir().join(format!("cell-prompt-{}", crate::RequestId::generate()));
        fs::create_dir(&directory).unwrap();
        let id = crate::SessionId::generate().to_string();
        let path = directory.join(format!("fixture_{id}.jsonl"));
        let header = json!({"type":"session","id":id,"version":3});
        let system = json!({"type":"message","id":"system-1","parentId":null,"message":{
            "role":"system","content":"","sections":{"preamble":"You are an expert coding assistant operating inside pi, a coding agent harness.","docs":"<docs>\nPi documentation (read only when the user asks about pi itself, its SDK):\n- docs/extensions.md\n</docs>","cwd":"fixture workspace"},
            "toolsAdded":[{"name":"read","description":"fixture tool"}]}});
        // Text that happens to discuss Pi remains the user's own words.
        let user = r#"{ "type": "message", "id": "user-1", "parentId": "system-1", "message": {"role":"user","content":"Pi documentation: docs/extensions.md"} }"#;
        let original = format!("{header}\n{system}\n{user}\n");
        fs::write(&path, &original).unwrap();
        let prompt = instructions(Path::new("/home/agent/shared"), false);
        upgrade_instructions(&directory, &id, &prompt).unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(
            updated.ends_with(&format!("{user}\n")),
            "user record bytes stay intact"
        );
        let changed: Value = serde_json::from_str(updated.lines().nth(1).unwrap()).unwrap();
        assert_eq!(changed["id"], system["id"]);
        assert_eq!(
            changed["message"]["toolsAdded"],
            system["message"]["toolsAdded"]
        );
        assert_eq!(changed["message"]["sections"]["cwd"], "fixture workspace");
        assert_eq!(changed["message"]["sections"]["preamble"], prompt);
        assert!(changed["message"]["sections"]["docs"].is_null());
        let backup = path.with_extension(format!(
            "jsonl.before-workspace-prompt-{}",
            crate::Hash::of(original.as_bytes())
        ));
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        upgrade_instructions(&directory, &id, &prompt).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            updated,
            "migration is idempotent"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn resumed_sessions_adopt_the_current_kuggis_instructions() {
        let directory =
            std::env::temp_dir().join(format!("cell-prompt-{}", crate::RequestId::generate()));
        fs::create_dir(&directory).unwrap();
        let shared = Path::new("/home/agent/shared");
        let custom = "A preamble Kuggis did not write.";
        for (before, guest) in [
            // Before Files existed, without and with the environment suffix.
            (SYSTEM_PROMPT.to_string(), false),
            (format!("{SYSTEM_PROMPT} {GUEST_PROMPT_SUFFIX}"), true),
            // A different shared folder.
            (instructions(Path::new("/srv/old"), true), true),
            (custom.to_string(), true),
        ] {
            let id = crate::SessionId::generate().to_string();
            let path = directory.join(format!("fixture_{id}.jsonl"));
            let header = json!({"type":"session","id":id,"version":3});
            let system = json!({"type":"message","id":"system-1","parentId":null,"message":{
                "role":"system","content":"","sections":{"preamble":before,"cwd":"fixture workspace"},
                "toolsAdded":[{"name":"read","description":"fixture tool"}]}});
            let original = format!("{header}\n{system}\n");
            fs::write(&path, &original).unwrap();
            let prompt = instructions(shared, guest);
            upgrade_instructions(&directory, &id, &prompt).unwrap();
            let updated = fs::read_to_string(&path).unwrap();
            if before == custom {
                assert_eq!(updated, original, "other instructions stay untouched");
                continue;
            }
            let changed: Value = serde_json::from_str(updated.lines().nth(1).unwrap()).unwrap();
            assert_eq!(changed["message"]["sections"]["preamble"], prompt);
            assert_eq!(
                prompt.ends_with(GUEST_PROMPT_SUFFIX),
                guest,
                "environment instructions survive the upgrade"
            );
            assert_eq!(
                changed["message"]["sections"]["cwd"], "fixture workspace",
                "only the preamble changes"
            );
            assert!(
                !changed["message"]["sections"]
                    .as_object()
                    .unwrap()
                    .contains_key("docs"),
                "Kuggis sessions keep their docs section untouched"
            );
            let backup = path.with_extension(format!(
                "jsonl.before-workspace-prompt-{}",
                crate::Hash::of(original.as_bytes())
            ));
            assert_eq!(fs::read_to_string(backup).unwrap(), original);
            upgrade_instructions(&directory, &id, &prompt).unwrap();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                updated,
                "migration is idempotent"
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn jsonl_preserves_unicode_separators_and_bounds_unfinished_records() {
        let bytes = "{\"text\":\"a\u{2028}b\u{2029}c\"}\r\n{\"ok\":true}\n";
        let mut reader = BufReader::new(bytes.as_bytes());
        let first: Value =
            serde_json::from_str(&record(&mut reader).await.unwrap().unwrap()).unwrap();
        assert_eq!(first["text"], "a\u{2028}b\u{2029}c");
        assert!(record(&mut reader).await.unwrap().is_some());
        assert!(record(&mut reader).await.unwrap().is_none());
        let oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
        assert!(record(&mut BufReader::new(oversized.as_slice()))
            .await
            .is_err());
    }

    #[test]
    fn deltas_reconstruct_text_reasoning_and_complete_tool_arguments() {
        let mut message = json!({"role":"assistant","content":[]});
        for delta in [
            json!({"type":"text_start","contentIndex":0}),
            json!({"type":"text_delta","contentIndex":0,"delta":"Hello "}),
            json!({"type":"text_delta","contentIndex":0,"delta":"world"}),
            json!({"type":"thinking_delta","contentIndex":1,"delta":"Consider this"}),
            json!({"type":"toolcall_start","contentIndex":2,"id":"t","toolName":"read"}),
            json!({"type":"toolcall_end","contentIndex":2,"toolCall":{"type":"toolCall","id":"t","name":"read","arguments":{"path":"README.md"}}}),
        ] {
            apply_delta(&mut message, &json!({"assistantMessageEvent":delta}));
        }
        assert_eq!(message["content"][0]["text"], "Hello world");
        assert_eq!(message["content"][1]["thinking"], "Consider this");
        assert_eq!(message["content"][2]["arguments"]["path"], "README.md");
        let complete =
            json!({"role":"assistant","content":[{"type":"text","text":"Authoritative"}]});
        apply_delta(&mut message, &json!({"message":complete}));
        assert_eq!(message, complete);
    }

    #[test]
    fn transcript_keeps_pre_compaction_history_and_excludes_other_branches() {
        let first = json!({"role":"user","content":"Remember this"});
        let latest = json!({"role":"assistant","content":[]});
        let entries = json!({"leafId":"d","entries":[
            {"id":"system","parentId":null,"type":"message","message":{"role":"system","content":"Internal runtime instructions"}},
            {"id":"a","parentId":"system","type":"message","message":first},
            {"id":"b","parentId":"a","type":"message","message":{"role":"user","content":"Other branch"}},
            {"id":"c","parentId":"a","type":"compaction","summary":"Compressed"},
            {"id":"d","parentId":"c","type":"message","message":latest}
        ]});
        assert_eq!(transcript(&entries).unwrap(), vec![first, latest]);
        assert!(transcript(&json!({"leafId":"x","entries":[]})).is_err());
    }

    #[test]
    fn transcript_rejects_malformed_known_records_but_keeps_unknown_entries_skippable() {
        for entries in [
            json!({"leafId":"a","entries":[{"id":"a","parentId":null,"type":"message","message":{"content":[]}}]}),
            json!({"leafId":"a","entries":[{"id":"a","parentId":null,"type":"message","isSidechain":"yes","message":{"role":"user"}}]}),
            json!({"leafId":"a","entries":[{"id":"a","parentId":null,"type":"branch_summary","summary":12,"fromId":"x"}]}),
            json!({"leafId":"a","entries":[{"id":"a","parentId":null,"type":"custom_message","customType":"fixture"}]}),
        ] {
            assert!(transcript(&entries).is_err());
        }
        let future = json!({"leafId":"a","entries":[{"id":"a","parentId":null,"type":"future_record","extension":{"preserved":true}}]});
        assert!(transcript(&future).unwrap().is_empty());
        assert!(transcript(&json!({"leafId":null,"entries":[]}))
            .unwrap()
            .is_empty());
        assert!(transcript(
            &json!({"leafId":null,"entries":[{"id":"1","parentId":null,"type":"future"}]})
        )
        .is_err());
    }
}
