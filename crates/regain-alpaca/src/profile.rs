use anyhow::{Result, ensure};
use regain_core::{RecoveryOptions, invalid};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub unique_id: String,
    pub label: String,
    pub camera: Option<Value>,
    pub serial: Option<String>,
    pub direct: bool,
    pub sdk_fallback: bool,
    pub recovery: RecoveryOptions,
    pub controls: BTreeMap<i32, i64>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            unique_id: Uuid::new_v4().to_string(),
            label: "PulsarFab regain Retryable Camera".into(),
            camera: None,
            serial: None,
            direct: false,
            sdk_fallback: false,
            recovery: RecoveryOptions::default(),
            controls: BTreeMap::new(),
        }
    }
}
impl Profile {
    /// Alpaca clients such as NINA cache the discovery name as FITS INSTRUME.
    /// Keep user labels in setup, and publish the selected camera model.
    pub fn camera_name(&self) -> &str {
        self.camera
            .as_ref()
            .and_then(|camera| camera["name"].as_str())
            .unwrap_or(&self.label)
    }

    pub fn validate(&self) -> Result<()> {
        self.recovery.validate()?;
        ensure!(
            !self.label.trim().is_empty()
                && self.label.len() <= 100
                && Uuid::parse_str(&self.unique_id).is_ok(),
            "Invalid camera label or ID"
        );
        ensure!(
            self.serial
                .as_ref()
                .is_none_or(|s| !s.is_empty() && s.len() <= 128),
            "Invalid camera serial number"
        );
        if let Some(c) = &self.camera {
            let w = c["width"]
                .as_u64()
                .ok_or_else(|| invalid("Missing camera width"))?;
            let h = c["height"]
                .as_u64()
                .ok_or_else(|| invalid("Missing camera height"))?;
            ensure!(
                w > 0
                    && h > 0
                    && w <= 65536
                    && h <= 65536
                    && w * h <= 268435456
                    && c["name"]
                        .as_str()
                        .is_some_and(|n| !n.is_empty() && n.len() < 200),
                "Invalid camera description"
            );
            ensure!(
                c["bins"].as_array().is_some_and(|a| !a.is_empty()
                    && a.iter()
                        .all(|b| b.as_u64().is_some_and(|v| (1..=16).contains(&v)))),
                "Invalid camera binning"
            );
            if self.direct {
                ensure!(
                    matches!(
                        c["name"].as_str(),
                        Some(
                            "ZWO ASI585MM Pro"
                                | "ZWO ASI676MC"
                                | "ZWO ASI662MC"
                                | "ZWO ASI2600MM Duo"
                                | "ZWO ASI2600MM Pro"
                                | "ZWO ASI220MM Mini"
                                | "ZWO ASI6200MM Pro"
                        )
                    ),
                    "Camera is not supported by the direct driver"
                );
            }
        }
        ensure!(
            self.controls
                .keys()
                .all(|k| matches!(k, 0 | 5 | 6 | 16 | 17 | 21 | 22 | 23)),
            "Unsupported saved camera control"
        );
        Ok(())
    }
}
pub struct Profiles {
    pub focusers: crate::focuser::Slots,
    pub rotators: crate::slots::Slots,
    path: Option<PathBuf>,
    values: Mutex<Vec<Profile>>,
}
impl Profiles {
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        let values: Vec<Profile> = match &path {
            Some(p) if p.exists() => serde_json::from_slice(&std::fs::read(p)?)?,
            _ => vec![
                Profile {
                    label: "PulsarFab regain Main Camera".into(),
                    ..Profile::default()
                },
                Profile {
                    label: "PulsarFab regain Guide Camera".into(),
                    ..Profile::default()
                },
            ],
        };
        Self::validate(&values)?;
        let profiles = Self {
            focusers: crate::focuser::Slots::new(path.clone())?,
            rotators: crate::slots::Slots::new_rotators(path.clone())?,
            path,
            values: Mutex::new(values),
        };
        profiles.write(&profiles.values.lock().unwrap())?;
        Ok(profiles)
    }
    pub fn rotator_path(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_extension("rotator.json"))
    }
    pub fn accessory_path(&self, kind: &str) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|p| p.with_extension(format!("{kind}.json")))
    }
    pub fn all(&self) -> Vec<Profile> {
        self.values.lock().unwrap().clone()
    }
    pub fn reload(&self) -> Result<()> {
        if let Some(path) = &self.path {
            let values: Vec<Profile> = serde_json::from_slice(&std::fs::read(path)?)?;
            Self::validate(&values)?;
            *self.values.lock().unwrap() = values;
        }
        Ok(())
    }
    pub fn get(&self, slot: usize) -> Result<Profile> {
        self.values
            .lock()
            .unwrap()
            .get(slot)
            .cloned()
            .ok_or_else(|| invalid("Unknown camera slot"))
    }
    pub fn add(&self) -> Result<usize> {
        let mut values = self.values.lock().unwrap();
        let mut next = values.clone();
        let slot = next.len();
        next.push(Profile {
            label: format!("PulsarFab regain Camera {}", slot + 1),
            ..Profile::default()
        });
        self.write(&next)?;
        *values = next;
        Ok(slot)
    }
    pub fn save(&self, slot: usize, mut profile: Profile) -> Result<()> {
        let mut values = self.values.lock().unwrap();
        profile.unique_id = values
            .get(slot)
            .ok_or_else(|| invalid("Unknown camera slot"))?
            .unique_id
            .clone();
        profile.serial = profile
            .serial
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        profile.validate()?;
        let mut next = values.clone();
        next[slot] = profile;
        self.write(&next)?;
        *values = next;
        Ok(())
    }
    fn write(&self, values: &[Profile]) -> Result<()> {
        if let Some(path) = &self.path {
            let dir = path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(dir)?;
            let mut temp = tempfile::NamedTempFile::new_in(dir)?;
            temp.write_all(&serde_json::to_vec_pretty(values)?)?;
            temp.as_file().sync_all()?;
            temp.persist(path)?;
        }
        Ok(())
    }
    fn validate(values: &[Profile]) -> Result<()> {
        // An explicit empty list is an accessory-only installation. Missing
        // settings retain the ordinary main/guide defaults in new().
        let mut ids = std::collections::HashSet::new();
        for profile in values {
            profile.validate()?;
            ensure!(ids.insert(&profile.unique_id), "Duplicate camera ID");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_empty_profiles_survive_restart_and_can_gain_a_first_camera() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, b"[]").unwrap();
        let profiles = Profiles::new(Some(path.clone())).unwrap();
        assert!(profiles.all().is_empty());
        assert!(profiles.get(0).is_err());
        assert!(Profiles::new(Some(path.clone())).unwrap().all().is_empty());
        assert_eq!(profiles.add().unwrap(), 0);
        let id = profiles.get(0).unwrap().unique_id;
        let reopened = Profiles::new(Some(path)).unwrap();
        assert_eq!(reopened.all().len(), 1);
        assert_eq!(reopened.get(0).unwrap().unique_id, id);
    }

    #[test]
    fn missing_settings_keep_normal_camera_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let profiles = Profiles::new(Some(path.clone())).unwrap();
        assert_eq!(profiles.all().len(), 2);
        let ids: Vec<_> = profiles.all().into_iter().map(|p| p.unique_id).collect();
        assert_ne!(ids[0], ids[1]);
        assert_eq!(Profiles::new(None).unwrap().all().len(), 2);
        assert_eq!(
            Profiles::new(Some(path)).unwrap().get(0).unwrap().unique_id,
            ids[0]
        );
    }

    #[test]
    fn reload_validates_before_replacing_slots_and_accepts_an_empty_list() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let profiles = Profiles::new(Some(path.clone())).unwrap();
        let original = serde_json::to_value(profiles.all()).unwrap();
        for invalid in [
            serde_json::json!({}),
            serde_json::to_value(vec![profiles.get(0).unwrap(); 2]).unwrap(),
            serde_json::to_value(vec![Profile {
                label: String::new(),
                ..Profile::default()
            }])
            .unwrap(),
        ] {
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(profiles.reload().is_err());
            assert_eq!(serde_json::to_value(profiles.all()).unwrap(), original);
            assert!(Profiles::new(Some(path.clone())).is_err());
        }
        std::fs::write(&path, b"[]").unwrap();
        profiles.reload().unwrap();
        assert!(profiles.all().is_empty());
        assert_eq!(profiles.add().unwrap(), 0);
    }
}
