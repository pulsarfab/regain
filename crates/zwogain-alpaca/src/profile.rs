use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;
use zwogain_core::{RecoveryOptions, invalid};

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
            label: "ZWOgain Retryable Camera".into(),
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
                            "ZWO ASI676MC"
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
    path: Option<PathBuf>,
    values: Mutex<Vec<Profile>>,
}
impl Profiles {
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        let values: Vec<Profile> = match &path {
            Some(p) if p.exists() => serde_json::from_slice(&std::fs::read(p)?)?,
            _ => vec![
                Profile {
                    label: "ZWOgain Main Camera".into(),
                    ..Profile::default()
                },
                Profile {
                    label: "ZWOgain Guide Camera".into(),
                    ..Profile::default()
                },
            ],
        };
        ensure!(!values.is_empty(), "No camera slots in settings");
        let mut ids = std::collections::HashSet::new();
        for p in &values {
            p.validate()?;
            ensure!(ids.insert(&p.unique_id), "Duplicate camera ID");
        }
        let profiles = Self {
            path,
            values: Mutex::new(values),
        };
        profiles.write(&profiles.values.lock().unwrap())?;
        Ok(profiles)
    }
    pub fn all(&self) -> Vec<Profile> {
        self.values.lock().unwrap().clone()
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
            label: format!("ZWOgain Camera {}", slot + 1),
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
}
