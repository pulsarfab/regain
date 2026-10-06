//! Durable native rotator references, separate from editable hub configuration.
//! Atomic revisions fence late filesystem completion after request cancellation.
use crate::{
    config::NativeDevice,
    endpoint::{Endpoint, platform},
    source::{ErrorKind, SourceError},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReferenceKey {
    pub source: Uuid,
    pub device: NativeDevice,
    pub identity: String,
    pub simulated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum ReferenceState {
    Known { offset: f64, reverse: bool },
    Uncertain,
}
impl ReferenceState {
    fn valid(&self) -> bool {
        match self {
            Self::Known { offset, .. } => offset.is_finite() && (0.0..360.0).contains(offset),
            Self::Uncertain => true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReferenceRecord {
    version: u32,
    scope: String,
    key: ReferenceKey,
    pub revision: Uuid,
    pub state: ReferenceState,
}
#[derive(Clone)]
pub struct NativeReferenceStore {
    root: PathBuf,
    user: String,
    scope: String,
    gate: Arc<Mutex<()>>,
}
impl NativeReferenceStore {
    pub fn for_endpoint(endpoint: &Endpoint) -> io::Result<Self> {
        #[cfg(windows)]
        let base = platform::root(&platform::user()?)?.join("NativeReferences");
        #[cfg(unix)]
        let base = crate::credentials::data_directory()?
            .join("regain")
            .join("hub-native-references");
        Self::at_directory(&base, endpoint.key())
    }
    /// Explicit host/test location. Construction performs no filesystem writes.
    pub fn at_directory(base: &Path, scope: &str) -> io::Result<Self> {
        if !base.is_absolute() || scope.len() != 64 || !scope.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid native reference storage scope",
            ));
        }
        Ok(Self {
            root: base.join(scope),
            user: platform::user()?,
            scope: scope.into(),
            gate: Arc::new(Mutex::new(())),
        })
    }
    fn filename(&self, key: &ReferenceKey) -> Result<String, SourceError> {
        if key.source.is_nil()
            || !matches!(key.device, NativeDevice::Caa | NativeDevice::Falcon)
            || key.identity.trim().is_empty()
            || key.identity.chars().count() > crate::config::MAX_LABEL_CHARS
            || key.identity.chars().any(char::is_control)
        {
            return Err(unavailable());
        }
        let mut hash = Sha256::new();
        hash.update(self.scope.as_bytes());
        hash.update(serde_json::to_vec(key).map_err(|_| unavailable())?);
        Ok(format!("{:x}", hash.finalize()))
    }
    fn read(&self, key: &ReferenceKey) -> Result<Option<ReferenceRecord>, SourceError> {
        let filename = self.filename(key)?;
        match fs::symlink_metadata(&self.root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(unavailable()),
            Ok(_) => platform::prepare_root(&self.root, &self.user).map_err(|_| unavailable())?,
        }
        let file =
            match platform::read_private(&self.root.join(format!("{filename}.json")), &self.user) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                result => result.map_err(|_| unavailable())?,
            };
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        if bytes.len() > 4096 {
            return Err(unavailable());
        }
        let record: ReferenceRecord = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        if record.version != 1
            || record.scope != self.scope
            || record.key != *key
            || record.revision.is_nil()
            || !record.state.valid()
        {
            return Err(unavailable());
        }
        Ok(Some(record))
    }
    pub(crate) async fn load(
        &self,
        key: ReferenceKey,
    ) -> Result<Option<ReferenceRecord>, SourceError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = store.gate.lock().map_err(|_| unavailable())?;
            store.read(&key)
        })
        .await
        .map_err(|_| unavailable())?
    }
    pub(crate) async fn replace(
        &self,
        key: ReferenceKey,
        expected: Option<Uuid>,
        state: ReferenceState,
    ) -> Result<ReferenceRecord, SourceError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save(key, expected, state))
            .await
            .map_err(|_| unavailable())?
    }
    fn save(
        &self,
        key: ReferenceKey,
        expected: Option<Uuid>,
        state: ReferenceState,
    ) -> Result<ReferenceRecord, SourceError> {
        if !state.valid() {
            return Err(unavailable());
        }
        let filename = self.filename(&key)?;
        let _gate = self.gate.lock().map_err(|_| unavailable())?;
        fs::create_dir_all(self.root.parent().unwrap()).map_err(|_| unavailable())?;
        platform::prepare_root(&self.root, &self.user).map_err(|_| unavailable())?;
        // Separate OS lock also fences another store/process. Never unlink it.
        let lock = platform::lock_file(&self.root.join(format!("{filename}.lock")), &self.user)
            .map_err(|_| unavailable())?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                SourceError::new(ErrorKind::Busy, "Native reference storage is busy")
            }
            std::fs::TryLockError::Error(_) => unavailable(),
        })?;
        if self.read(&key)?.as_ref().map(|record| record.revision) != expected {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Native reference changed; reconnect before writing",
            ));
        }
        let record = ReferenceRecord {
            version: 1,
            scope: self.scope.clone(),
            key,
            revision: Uuid::new_v4(),
            state,
        };
        let path = self.root.join(format!("{}.tmp", Uuid::new_v4()));
        let file = platform::create_private(&path, &self.user).map_err(|_| unavailable())?;
        let temporary = tempfile::TempPath::try_from_path(path).map_err(|_| unavailable())?;
        let mut file = tempfile::NamedTempFile::from_parts(file, temporary);
        file.write_all(&serde_json::to_vec(&record).map_err(|_| unavailable())?)
            .map_err(|_| unavailable())?;
        file.as_file().sync_all().map_err(|_| unavailable())?;
        let file = file
            .persist(self.root.join(format!("{filename}.json")))
            .map_err(|_| unavailable())?;
        // Replacement has happened. A durability failure is an unknown outcome,
        // never a license to replace/replay this operation using an old revision.
        file.sync_all().map_err(|_| SourceError::uncertain())?;
        #[cfg(unix)]
        fs::File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|_| SourceError::uncertain())?;
        Ok(record)
    }
}
fn unavailable() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Native reference storage is missing, invalid or unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> ReferenceKey {
        ReferenceKey {
            source: Uuid::new_v4(),
            device: NativeDevice::Caa,
            identity: "0102030405060708".into(),
            simulated: true,
        }
    }
    fn store(directory: &tempfile::TempDir) -> NativeReferenceStore {
        NativeReferenceStore::at_directory(&directory.path().join("references"), &"a".repeat(64))
            .unwrap()
    }
    #[tokio::test]
    async fn missing_records_do_not_create_directories_and_identity_scopes_are_independent() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        assert!(store.load(key.clone()).await.unwrap().is_none());
        assert!(!directory.path().join("references").exists());
        let record = store
            .replace(
                key.clone(),
                None,
                ReferenceState::Known {
                    offset: 42.5,
                    reverse: false,
                },
            )
            .await
            .unwrap();
        let fresh = NativeReferenceStore::at_directory(
            &directory.path().join("references"),
            &"a".repeat(64),
        )
        .unwrap();
        assert_eq!(
            fresh.load(key.clone()).await.unwrap().unwrap().revision,
            record.revision
        );
        for other in [
            ReferenceKey {
                source: Uuid::new_v4(),
                ..key.clone()
            },
            ReferenceKey {
                device: NativeDevice::Falcon,
                ..key.clone()
            },
            ReferenceKey {
                identity: "other".into(),
                ..key.clone()
            },
            ReferenceKey {
                simulated: false,
                ..key.clone()
            },
        ] {
            assert!(fresh.load(other).await.unwrap().is_none());
        }
        let scope = NativeReferenceStore::at_directory(
            &directory.path().join("references"),
            &"b".repeat(64),
        )
        .unwrap();
        assert!(scope.load(key).await.unwrap().is_none());
    }
    #[tokio::test]
    async fn stale_or_abandoned_commit_cannot_clear_a_newer_uncertain_marker() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        let marker = store
            .replace(key.clone(), None, ReferenceState::Uncertain)
            .await
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let old = tokio::spawn({
            let store = store.clone();
            let key = key.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    started_tx.send(()).unwrap();
                    proceed_rx.recv().unwrap();
                    let result = store.save(
                        key,
                        Some(marker.revision),
                        ReferenceState::Known {
                            offset: 42.0,
                            reverse: false,
                        },
                    );
                    result_tx.send(result).unwrap();
                })
                .await
                .unwrap();
            }
        });
        started_rx.await.unwrap();
        old.abort();
        assert!(old.await.unwrap_err().is_cancelled());
        let newer = store
            .replace(
                key.clone(),
                Some(marker.revision),
                ReferenceState::Uncertain,
            )
            .await
            .unwrap();
        proceed_tx.send(()).unwrap();
        assert_eq!(
            result_rx.await.unwrap().unwrap_err().kind,
            ErrorKind::Unavailable
        );
        let record = store.load(key).await.unwrap().unwrap();
        assert_eq!(record.revision, newer.revision);
        assert_eq!(record.state, ReferenceState::Uncertain);
    }
    #[tokio::test]
    async fn separate_store_instances_have_one_atomic_revision_winner() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        let other = NativeReferenceStore::at_directory(
            &directory.path().join("references"),
            &"a".repeat(64),
        )
        .unwrap();
        let (a, b) = tokio::join!(
            store.replace(key.clone(), None, ReferenceState::Uncertain),
            other.replace(
                key.clone(),
                None,
                ReferenceState::Known {
                    offset: 25.0,
                    reverse: false
                }
            )
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(matches!(
            a.err().or(b.err()).unwrap().kind,
            ErrorKind::Busy | ErrorKind::Unavailable
        ));
        assert!(store.load(key).await.unwrap().is_some());
    }
    #[tokio::test]
    async fn malformed_oversized_and_wrong_identity_records_never_become_zero_reference() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        let record = store
            .replace(key.clone(), None, ReferenceState::Uncertain)
            .await
            .unwrap();
        let path = store
            .root
            .join(format!("{}.json", store.filename(&key).unwrap()));
        let mut wrong = serde_json::to_value(&record).unwrap();
        wrong["key"]["identity"] = serde_json::json!("other");
        let mut invalid = serde_json::to_value(&record).unwrap();
        invalid["state"] = serde_json::json!({"kind":"known", "offset":360.0, "reverse":false});
        for bytes in [
            b"{".to_vec(),
            vec![b' '; 4097],
            serde_json::to_vec(&wrong).unwrap(),
            serde_json::to_vec(&invalid).unwrap(),
        ] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                store.load(key.clone()).await.unwrap_err().kind,
                ErrorKind::Unavailable
            );
            assert_eq!(
                store
                    .replace(
                        key.clone(),
                        Some(record.revision),
                        ReferenceState::Uncertain
                    )
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Unavailable
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[tokio::test]
    async fn invalid_new_state_and_keys_are_rejected_before_file_creation() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        for offset in [-1.0, 360.0, f64::NAN, f64::INFINITY] {
            assert!(
                store
                    .replace(
                        key.clone(),
                        None,
                        ReferenceState::Known {
                            offset,
                            reverse: false
                        }
                    )
                    .await
                    .is_err()
            );
        }
        assert!(
            store
                .load(ReferenceKey {
                    source: Uuid::nil(),
                    ..key
                })
                .await
                .is_err()
        );
        assert!(!directory.path().join("references").exists());
        assert!(
            NativeReferenceStore::at_directory(Path::new("relative"), &"a".repeat(64)).is_err()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn public_permissions_and_symlinks_cannot_supply_a_saved_reference() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let key = key();
        store
            .replace(key.clone(), None, ReferenceState::Uncertain)
            .await
            .unwrap();
        let path = store
            .root
            .join(format!("{}.json", store.filename(&key).unwrap()));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load(key.clone()).await.is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let target = store.root.join("copied.json");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(store.load(key.clone()).await.is_err());
        assert!(
            store
                .replace(key, None, ReferenceState::Uncertain)
                .await
                .is_err()
        );
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
