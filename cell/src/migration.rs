//! Import the previous VM runner's conversations without modifying its files.
//! Tools are rendered as text because Claude and Pi tool protocols differ.
use crate::{Cell, Name, Session};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

fn find(dir: &Path, files: &mut BTreeMap<String, std::path::PathBuf>) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            find(&entry.path(), files)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|x| x == "jsonl") {
            files.insert(
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            );
        }
    }
    Ok(())
}

fn content(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match block["type"].as_str() {
            Some("text") => block["text"].as_str().map(str::to_owned),
            Some("tool_use") => Some(format!(
                "Tool {}: {}",
                block["name"].as_str().unwrap_or(""),
                block["input"]
            )),
            Some("tool_result") => Some(format!("Tool result: {}", content(&block["content"]))),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn import_claude(
    cell: &Cell,
    home: &Path,
    workspace: &Path,
    state: &Path,
) -> crate::apply::Fallible {
    let mapping: BTreeMap<Name, String> = match fs::read(home.join(".cell-sessions.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut files = BTreeMap::new();
    find(&home.join(".claude/projects"), &mut files)?;
    fs::create_dir_all(state.join("threads"))?;
    fs::create_dir_all(state.join("pi"))?;
    for (name, runtime) in mapping {
        let id = crate::legacy::session_id(&cell.name, &name);
        let metadata = state.join("threads").join(format!("{id}.json"));
        if metadata.exists() {
            continue;
        }
        let Some(source) = files.get(&format!("{runtime}.jsonl")) else {
            eprintln!("legacy session {name}: transcript not found; original mapping retained");
            continue;
        };
        let bytes = fs::read_to_string(source)?;
        let rows: Vec<Value> = bytes
            .lines()
            .filter(|s| !s.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let by_id: BTreeMap<_, _> = rows
            .iter()
            .filter_map(|r| r["uuid"].as_str().map(|id| (id, r)))
            .collect();
        let mut row = rows.iter().rev().find(|r| {
            r["isSidechain"] != true && matches!(r["type"].as_str(), Some("user" | "assistant"))
        });
        let mut branch = vec![];
        let mut visited = BTreeSet::new();
        while let Some(current) = row {
            let uuid = current["uuid"]
                .as_str()
                .ok_or("legacy message missing uuid")?;
            if !visited.insert(uuid) {
                return Err("legacy transcript contains a cycle".into());
            }
            branch.push(current);
            row = current["parentUuid"]
                .as_str()
                .and_then(|parent| by_id.get(parent).copied());
        }
        branch.reverse();
        let timestamp = branch
            .first()
            .and_then(|r| r["timestamp"].as_str())
            .unwrap_or("1970-01-01T00:00:00Z");
        let mut output = vec![
            json!({"type":"session","version":3,"id":id,"timestamp":timestamp,"cwd":workspace}),
        ];
        let mut parent = None;
        for (index, row) in branch.iter().enumerate() {
            let role = row["message"]["role"].as_str().unwrap_or("");
            if !["user", "assistant"].contains(&role) {
                continue;
            }
            let text = content(&row["message"]["content"]);
            if text.is_empty() {
                continue;
            }
            let entry_id = format!("legacy-{index}");
            let message = if role == "user" {
                json!({"role":"user","content":text,"timestamp":index})
            } else {
                json!({"role":"assistant","content":[{"type":"text","text":text}],"api":"anthropic-messages","provider":"anthropic","model":row["message"]["model"].as_str().unwrap_or("legacy"),"stopReason":"stop","timestamp":index,
                    "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}})
            };
            output.push(json!({"type":"message","id":entry_id,"parentId":parent,"timestamp":row["timestamp"].as_str().unwrap_or(timestamp),"message":message}));
            parent = Some(entry_id);
        }
        let transcript = state.join("pi").join(format!("imported_{id}.jsonl"));
        let text = output
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let temp = transcript.with_extension("tmp");
        fs::write(&temp, text)?;
        fs::rename(temp, transcript)?;
        let record = Session::restore(id, cell.name.clone(), name.to_string(), true);
        let temp = metadata.with_extension("tmp");
        fs::write(&temp, serde_json::to_vec(&record)?)?;
        fs::rename(temp, metadata)?;
        eprintln!("imported legacy session {name} as {id} (archived)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_preserves_sources_and_follows_active_branch() {
        let home =
            std::env::temp_dir().join(format!("cell-import-{}", crate::RequestId::generate()));
        fs::create_dir_all(home.join(".claude/projects/p")).unwrap();
        fs::write(home.join(".cell-sessions.json"), r#"{"s1":"old"}"#).unwrap();
        let source = home.join(".claude/projects/p/old.jsonl");
        let original = [json!({"type":"user","uuid":"a","parentUuid":null,"message":{"role":"user","content":"hello"}}),json!({"type":"assistant","uuid":"wrong","parentUuid":"a","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"wrong branch"}]}}),json!({"type":"assistant","uuid":"b","parentUuid":"a","message":{"role":"assistant","content":[{"type":"text","text":"saved reply"}]}})].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        fs::write(&source, &original).unwrap();
        let cell = Cell::new("dev".parse().unwrap(), []);
        let state = home.join("state");
        import_claude(&cell, &home, &home, &state).unwrap();
        import_claude(&cell, &home, &home, &state).unwrap();
        assert_eq!(fs::read_to_string(source).unwrap(), original);
        let id = crate::legacy::session_id(&cell.name, &"s1".parse().unwrap());
        let session: Session =
            serde_json::from_slice(&fs::read(state.join(format!("threads/{id}.json"))).unwrap())
                .unwrap();
        assert!(session.archived);
        let saved = fs::read_to_string(state.join(format!("pi/imported_{id}.jsonl"))).unwrap();
        assert!(saved.contains("saved reply"));
        assert!(!saved.contains("wrong branch"));
        fs::remove_dir_all(home).unwrap();
    }
}
