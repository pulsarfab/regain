//! Persisted accessory slots. Device numbers identify instances, not models.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Write, path::PathBuf, sync::Mutex};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Eaf,
    Fc3,
    Eta,
    Caa,
    Falcon,
}
impl Kind {
    pub fn worker(self) -> &'static str {
        match self {
            Self::Eaf => "eaf",
            Self::Fc3 => "fc3",
            Self::Eta => "eta",
            Self::Caa => "caa",
            Self::Falcon => "falcon",
        }
    }
    fn legacy_number(self) -> usize {
        match self {
            Self::Eaf => 0,
            Self::Fc3 => 1,
            Self::Eta => 2,
            Self::Caa => 0,
            Self::Falcon => usize::MAX,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Slot {
    pub number: usize,
    pub kind: Kind,
    pub unique_id: String,
    #[serde(default)]
    legacy: bool,
}
pub struct Slots {
    base: Option<PathBuf>,
    values: Mutex<Vec<Slot>>,
    rotators: bool,
}
impl Slots {
    pub fn new(base: Option<PathBuf>) -> Result<Self> {
        Self::new_group(base, false)
    }
    pub fn new_rotators(base: Option<PathBuf>) -> Result<Self> {
        Self::new_group(base, true)
    }
    fn new_group(base: Option<PathBuf>, rotators: bool) -> Result<Self> {
        let path = base.as_ref().map(|p| {
            p.with_extension(if rotators {
                "rotators.json"
            } else {
                "focusers.json"
            })
        });
        let mut values: Vec<Slot> = Vec::new();
        if let Some(path) = &path
            && path.exists()
        {
            values = serde_json::from_slice(&std::fs::read(path)?)?;
        } else if let Some(base) = &base {
            // Preserve existing URLs and UUIDs. Never assign IDs from discovery order.
            for kind in if rotators {
                vec![Kind::Caa]
            } else {
                vec![Kind::Eaf, Kind::Fc3, Kind::Eta]
            } {
                let old = base.with_extension(if rotators {
                    "rotator.json".into()
                } else {
                    format!("{}.json", kind.worker())
                });
                if old.exists() {
                    let profile: serde_json::Value = serde_json::from_slice(&std::fs::read(old)?)?;
                    values.push(Slot {
                        number: kind.legacy_number(),
                        kind,
                        unique_id: profile["uniqueId"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing legacy UUID"))?
                            .into(),
                        legacy: true,
                    });
                }
            }
        }
        let (mut numbers, mut ids) = (HashSet::new(), HashSet::new());
        for slot in &values {
            ensure!(
                matches!(slot.kind, Kind::Caa | Kind::Falcon) == rotators,
                "Wrong device type in slot registry"
            );
            ensure!(
                slot.number <= u32::MAX as usize && numbers.insert(slot.number),
                "Invalid or duplicate device number"
            );
            ensure!(
                Uuid::parse_str(&slot.unique_id).is_ok() && ids.insert(&slot.unique_id),
                "Invalid or duplicate device UUID"
            );
            ensure!(
                !slot.legacy || slot.number == slot.kind.legacy_number(),
                "Invalid legacy device slot"
            );
        }
        let slots = Self {
            base,
            rotators,
            values: Mutex::new(values),
        };
        slots.write(&slots.values.lock().unwrap())?;
        Ok(slots)
    }
    pub fn all(&self) -> Vec<Slot> {
        self.values.lock().unwrap().clone()
    }
    pub fn get(&self, number: usize) -> Option<Slot> {
        self.values
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.number == number)
            .cloned()
    }
    pub fn profile_path(&self, slot: &Slot) -> Option<PathBuf> {
        self.base.as_ref().map(|p| {
            p.with_extension(if slot.legacy {
                if self.rotators {
                    "rotator.json".into()
                } else {
                    format!("{}.json", slot.kind.worker())
                }
            } else {
                format!(
                    "{}-{}.json",
                    if self.rotators { "rotator" } else { "focuser" },
                    slot.number
                )
            })
        })
    }
    pub fn add(&self, kind: Kind) -> Result<usize> {
        ensure!(
            matches!(kind, Kind::Caa | Kind::Falcon) == self.rotators,
            "Wrong device type for slot"
        );
        let mut values = self.values.lock().unwrap();
        let number = values
            .iter()
            .map(|s| s.number)
            .max()
            .map_or(Some(0), |n| n.checked_add(1))
            .filter(|n| *n <= u32::MAX as usize)
            .ok_or_else(|| anyhow::anyhow!("No more device numbers"))?;
        let mut next = values.clone();
        next.push(Slot {
            number,
            kind,
            unique_id: Uuid::new_v4().to_string(),
            legacy: false,
        });
        self.write(&next)?;
        *values = next;
        Ok(number)
    }
    fn write(&self, values: &[Slot]) -> Result<()> {
        if let Some(base) = &self.base {
            let path = base.with_extension(if self.rotators {
                "rotators.json"
            } else {
                "focusers.json"
            });
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir)?;
            let mut temp = tempfile::NamedTempFile::new_in(dir)?;
            temp.write_all(&serde_json::to_vec_pretty(values)?)?;
            temp.as_file().sync_all()?;
            temp.persist(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotators_are_instances_and_preserve_legacy_caa_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let fresh = Slots::new_rotators(None).unwrap();
        assert!(fresh.all().is_empty());
        assert_eq!(fresh.add(Kind::Falcon).unwrap(), 0);
        assert_eq!(fresh.add(Kind::Falcon).unwrap(), 1);
        assert!(fresh.add(Kind::Fc3).is_err());
        let profile = crate::rotator::Profile::default();
        std::fs::write(
            path.with_extension("rotator.json"),
            serde_json::to_vec(&profile).unwrap(),
        )
        .unwrap();
        let slots = Slots::new_rotators(Some(path.clone())).unwrap();
        assert_eq!(slots.get(0).unwrap().unique_id, profile.unique_id);
        assert_eq!(
            slots.profile_path(&slots.get(0).unwrap()).unwrap(),
            path.with_extension("rotator.json")
        );
        assert_eq!(slots.add(Kind::Falcon).unwrap(), 1);
        assert_eq!(slots.add(Kind::Falcon).unwrap(), 2);
        let ids: Vec<_> = slots.all().iter().map(|s| s.unique_id.clone()).collect();
        let reopened = Slots::new_rotators(Some(path)).unwrap();
        assert_eq!(
            reopened
                .all()
                .iter()
                .map(|s| s.unique_id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_ne!(ids[1], ids[2]);
    }
    #[test]
    fn same_model_slots_persist_without_renumbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let slots = Slots::new(Some(path.clone())).unwrap();
        assert!(slots.all().is_empty());
        assert_eq!(slots.add(Kind::Eta).unwrap(), 0);
        assert_eq!(slots.add(Kind::Fc3).unwrap(), 1);
        assert_eq!(slots.add(Kind::Fc3).unwrap(), 2);
        let before = slots.all();
        let reopened = Slots::new(Some(path)).unwrap();
        for (left, right) in before.iter().zip(reopened.all()) {
            assert_eq!(left.number, right.number);
            assert_eq!(left.kind, right.kind);
            assert_eq!(left.unique_id, right.unique_id);
        }
        assert_ne!(before[1].unique_id, before[2].unique_id);
        assert_ne!(
            reopened.profile_path(&before[1]),
            reopened.profile_path(&before[2])
        );
        assert_eq!(reopened.add(Kind::Eaf).unwrap(), 3);
    }
    #[test]
    fn legacy_migration_keeps_urls_profiles_and_ids_and_runs_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let profile = crate::accessory::Profile {
            serial: Some("COM3".into()),
            ..Default::default()
        };
        std::fs::write(
            path.with_extension("eta.json"),
            serde_json::to_vec(&profile).unwrap(),
        )
        .unwrap();
        let slots = Slots::new(Some(path.clone())).unwrap();
        let eta = slots.get(2).unwrap();
        assert_eq!(eta.unique_id, profile.unique_id);
        assert_eq!(
            slots.profile_path(&eta).unwrap(),
            path.with_extension("eta.json")
        );
        assert!(slots.get(0).is_none());
        assert_eq!(slots.add(Kind::Eta).unwrap(), 3);
        std::fs::write(
            path.with_extension("eaf.json"),
            serde_json::to_vec(&crate::accessory::Profile::default()).unwrap(),
        )
        .unwrap();
        let reopened = Slots::new(Some(path.clone())).unwrap();
        assert_eq!(reopened.all().len(), 2);
        let mut broken = reopened.all();
        broken[1].number = 2;
        std::fs::write(
            path.with_extension("focusers.json"),
            serde_json::to_vec(&broken).unwrap(),
        )
        .unwrap();
        assert!(Slots::new(Some(path)).is_err());
    }
}
