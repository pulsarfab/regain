use anyhow::Result;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub fn action_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("zwogain.")
        .map_or_else(|| lower.clone(), |tail| format!("regain.{tail}"))
}

/// Copy old default profiles once, preserving UUIDs and never replacing new settings.
pub fn migrate_profiles(base: &Path) -> Result<PathBuf> {
    let target = base.join("Regain/Alpaca");
    let legacy = base.join("ZwoGain/Alpaca");
    if legacy.is_dir() {
        for entry in std::fs::read_dir(legacy)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if !entry.file_type()?.is_file()
                || !name_text.starts_with("cameras")
                || !name_text.ends_with(".json")
            {
                continue;
            }
            let destination = target.join(name);
            if destination.exists() {
                continue;
            }
            std::fs::create_dir_all(&target)?;
            let mut temporary = tempfile::NamedTempFile::new_in(&target)?;
            temporary.write_all(&std::fs::read(entry.path())?)?;
            temporary.as_file().sync_all()?;
            match temporary.persist_noclobber(&destination) {
                Ok(_) => (),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(target.join("cameras.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_profiles_and_existing_destinations() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("ZwoGain/Alpaca");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("cameras.json"), r#"[{"uniqueId":"kept"}]"#).unwrap();
        std::fs::write(old.join("cameras.fc3.json"), r#"{"serial":"device"}"#).unwrap();
        std::fs::write(old.join("unrelated.json"), "private").unwrap();
        let path = migrate_profiles(root.path()).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(old.join("cameras.json")).unwrap()
        );
        assert!(path.with_file_name("cameras.fc3.json").exists());
        assert!(!path.with_file_name("unrelated.json").exists());
        std::fs::write(&path, "new settings").unwrap();
        migrate_profiles(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "new settings");
        assert!(old.join("cameras.json").exists());
        assert_eq!(action_name("ZwoGain.CAA.Status"), "regain.caa.status");
    }
}
