//! Seed compatible providers through Pi's models.json; Pi owns the transport.
use crate::{Cell, Provider};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(crate) fn configure(directory: &Path, cell: &Cell) -> crate::apply::Fallible {
    if !cell.providers().contains(&Provider::PaperInstruments) {
        return Ok(());
    }
    let path = directory.join("models.json");
    let mut config: Value = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"providers":{}}),
        Err(e) => return Err(e.into()),
    };
    let providers = config
        .as_object_mut()
        .ok_or("Pi models.json must be an object")?
        .entry("providers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Pi models.json providers must be an object")?;
    // Preserve user configuration, including overrides for this provider.
    if providers.contains_key("paperinstruments") {
        return Ok(());
    }
    providers.insert(
        "paperinstruments".into(),
        serde_json::from_str(include_str!("paperinstruments.json"))?,
    );
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let tmp = directory.join("models.json.kuggis-tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(&config)?)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adds_only_granted_provider_preserving_existing_configuration() {
        let dir =
            std::env::temp_dir().join(format!("cell-models-{}", crate::RequestId::generate()));
        let mut cell: Cell = serde_json::from_value(
            json!({"id":"b4bbe201-acbf-5487-b110-783340bb3fd1","name":"test", "grants":[]}),
        )
        .unwrap();
        configure(&dir, &cell).unwrap();
        assert!(!dir.exists());
        fs::create_dir(&dir).unwrap();
        let path = dir.join("models.json");
        fs::write(
            &path,
            br#"{"providers":{"custom":{"baseUrl":"https://custom.example"}}}"#,
        )
        .unwrap();
        cell.grants
            .insert(crate::Grant::Provider(Provider::PaperInstruments));
        configure(&dir, &cell).unwrap();
        let first = fs::read(&path).unwrap();
        let models: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(
            models["providers"]["custom"]["baseUrl"],
            "https://custom.example"
        );
        assert_eq!(
            models["providers"]["paperinstruments"]["models"][0]["id"],
            "ultramarine-1-flash"
        );
        assert!(models["providers"]["paperinstruments"]
            .get("apiKey")
            .is_none());
        configure(&dir, &cell).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        fs::write(
            &path,
            br#"{"providers":{"paperinstruments":{"baseUrl":"https://custom.example"}}}"#,
        )
        .unwrap();
        let custom = fs::read(&path).unwrap();
        configure(&dir, &cell).unwrap();
        assert_eq!(fs::read(&path).unwrap(), custom);
        fs::remove_dir_all(dir).unwrap();
    }
}
