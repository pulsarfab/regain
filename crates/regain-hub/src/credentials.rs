//! Write-only credential references, isolated by user and canonical hub path.
//! Windows adds user DPAPI encryption; Unix uses private file permissions without
//! requiring a desktop secret service. Protection is reported, never implied.
use crate::{
    endpoint::{Endpoint, platform},
    factory::CredentialProvider,
    source::{ErrorKind, SourceError},
};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

#[cfg(windows)]
#[path = "credentials/windows.rs"]
mod crypto;
pub const MAX_AUTHORIZATION_BYTES: usize = 8192;
const MAX_RECORD_BYTES: usize = 65536;

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
pub struct SecretAuthorization(String);
impl SecretAuthorization {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    fn header(&self) -> Result<HeaderValue, CredentialError> {
        if self.0.trim().is_empty()
            || self.0.len() > MAX_AUTHORIZATION_BYTES
            || !self.0.is_ascii()
            || self.0.bytes().any(|b| b.is_ascii_control())
        {
            return Err(CredentialError::Invalid);
        }
        let mut value = HeaderValue::from_str(&self.0).map_err(|_| CredentialError::Invalid)?;
        value.set_sensitive(true);
        Ok(value)
    }
}
impl std::fmt::Debug for SecretAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted authorization]")
    }
}
impl Drop for SecretAuthorization {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Protection {
    WindowsDpapiUser,
    UserFilePermissions,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialError {
    Invalid,
    Missing,
    Unavailable,
    InUse,
    Busy,
    Unsupported,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub reference: String,
    pub present: bool,
    pub protection: Protection,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence_warning: Option<&'static str>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    scope: String,
    reference: String,
    authorization: SecretAuthorization,
}
pub struct CredentialStore {
    root: PathBuf,
    user: String,
    scope: String,
}
impl CredentialStore {
    pub fn for_endpoint(endpoint: &Endpoint) -> io::Result<Self> {
        #[cfg(windows)]
        let user = platform::user()?;
        #[cfg(windows)]
        let base = platform::root(&user)?.join("Credentials");
        #[cfg(unix)]
        let base = data_directory()?.join("regain").join("hub-credentials");
        Self::at_directory(&base, endpoint.key())
    }
    /// Host/test override. The base must be absolute; each hub gets a separate
    /// private subdirectory. No directories are created until a credential write.
    pub fn at_directory(base: &Path, scope: &str) -> io::Result<Self> {
        if !base.is_absolute() || scope.len() != 64 || !scope.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid credential storage location or scope",
            ));
        }
        Ok(Self {
            root: base.join(scope),
            user: platform::user()?,
            scope: scope.into(),
        })
    }
    pub fn protection(&self) -> Protection {
        if cfg!(windows) {
            Protection::WindowsDpapiUser
        } else {
            Protection::UserFilePermissions
        }
    }
    pub fn description(&self) -> serde_json::Value {
        serde_json::json!({"protection":self.protection(),"immutableReferences":true,
            "input":{"authorization":{"type":"string","label":"Authorization header",
                "description":"Complete upstream HTTP Authorization value, such as Bearer followed by a token. Saved separately from configuration; never returned by the host.",
                "maxLength":MAX_AUTHORIZATION_BYTES,"writeOnly":true,"sensitive":true}},
            "rotation":"Create a new reference, apply it to the source, then remove the unused reference."})
    }
    fn directory(&self, create: bool) -> Result<(), CredentialError> {
        if create {
            fs::create_dir_all(self.root.parent().unwrap()).map_err(storage_error)?;
        } else {
            fs::symlink_metadata(&self.root).map_err(storage_error)?;
        }
        platform::prepare_root(&self.root, &self.user).map_err(storage_error)
    }
    fn key(&self, reference: &str) -> Result<String, CredentialError> {
        if reference.trim().is_empty()
            || reference.chars().count() > 200
            || reference.chars().any(char::is_control)
        {
            return Err(CredentialError::Invalid);
        }
        let mut hash = Sha256::new();
        hash.update(self.scope.as_bytes());
        hash.update([0]);
        hash.update(reference.as_bytes());
        Ok(format!("{:x}", hash.finalize()))
    }
    fn path(&self, reference: &str) -> Result<PathBuf, CredentialError> {
        Ok(self.root.join(format!("{}.cred", self.key(reference)?)))
    }
    pub fn create(
        &self,
        authorization: SecretAuthorization,
    ) -> Result<CredentialStatus, CredentialError> {
        authorization.header()?;
        self.directory(true)?;
        let reference = format!("credential-{}", Uuid::new_v4());
        #[cfg(windows)]
        let key = self.key(&reference)?;
        let record = Record {
            version: 1,
            scope: self.scope.clone(),
            reference: reference.clone(),
            authorization,
        };
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&record).map_err(|_| CredentialError::Unavailable)?);
        #[cfg(windows)]
        let encoded = crypto::protect(&plaintext, key.as_bytes()).map_err(storage_error)?;
        #[cfg(unix)]
        let encoded = plaintext;
        let path = self.root.join(format!("{}.tmp", Uuid::new_v4()));
        let file = platform::create_private(&path, &self.user).map_err(storage_error)?;
        let temporary = tempfile::TempPath::try_from_path(path).map_err(storage_error)?;
        let mut file = tempfile::NamedTempFile::from_parts(file, temporary);
        file.write_all(self.prefix()).map_err(storage_error)?;
        file.write_all(&encoded).map_err(storage_error)?;
        file.as_file().sync_all().map_err(storage_error)?;
        let file = file
            .persist_noclobber(self.path(&reference)?)
            .map_err(|_| CredentialError::Unavailable)?;
        // A failed flush must not return a usable reference. The immutable orphan
        // remains private and cannot silently replace an existing credential.
        file.sync_all().map_err(storage_error)?;
        self.flush_directory().map_err(storage_error)?;
        Ok(CredentialStatus {
            reference,
            present: true,
            protection: self.protection(),
        })
    }
    fn prefix(&self) -> &'static [u8] {
        if cfg!(windows) {
            b"REGCRED1W\n"
        } else {
            b"REGCRED1P\n"
        }
    }
    fn read(&self, reference: &str) -> Result<HeaderValue, CredentialError> {
        let path = self.path(reference)?;
        self.directory(false)?;
        let mut bytes = Zeroizing::new(Vec::new());
        platform::read_private(&path, &self.user)
            .map_err(storage_error)?
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(storage_error)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(CredentialError::Unavailable);
        }
        let encoded = bytes
            .strip_prefix(self.prefix())
            .ok_or(CredentialError::Unavailable)?;
        #[cfg(windows)]
        let plaintext =
            crypto::unprotect(encoded, self.key(reference)?.as_bytes()).map_err(storage_error)?;
        #[cfg(unix)]
        let plaintext = Zeroizing::new(encoded.to_vec());
        let record: Record =
            serde_json::from_slice(&plaintext).map_err(|_| CredentialError::Unavailable)?;
        if record.version != 1 || record.scope != self.scope || record.reference != reference {
            return Err(CredentialError::Unavailable);
        }
        record
            .authorization
            .header()
            .map_err(|_| CredentialError::Unavailable)
    }
    pub fn status(&self, reference: &str) -> Result<CredentialStatus, CredentialError> {
        let present = match self.read(reference) {
            Ok(_) => true,
            Err(CredentialError::Missing) => false,
            Err(error) => return Err(error),
        };
        Ok(CredentialStatus {
            reference: reference.into(),
            present,
            protection: self.protection(),
        })
    }
    /// The service checks configuration references under its update gate first.
    pub fn delete(&self, reference: &str) -> Result<DeleteOutcome, CredentialError> {
        let path = self.path(reference)?;
        let file = self
            .directory(false)
            .and_then(|()| platform::read_private(&path, &self.user).map_err(storage_error));
        match file {
            Ok(file) => drop(file),
            Err(CredentialError::Missing) => {
                return Ok(DeleteOutcome {
                    removed: false,
                    persistence_warning: None,
                });
            }
            Err(error) => return Err(error),
        }
        fs::remove_file(path).map_err(storage_error)?;
        Ok(DeleteOutcome { removed:true, persistence_warning:self.flush_directory().err().map(|_| "Credential was removed, but final filesystem durability could not be confirmed") })
    }
    fn flush_directory(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            fs::File::open(&self.root)?.sync_all()
        }
        #[cfg(windows)]
        {
            Ok(())
        }
    }
}
impl CredentialProvider for CredentialStore {
    fn authorization(&self, reference: &str) -> Result<HeaderValue, SourceError> {
        self.read(reference).map_err(|_| SourceError::new(ErrorKind::Unavailable, "Credential reference is missing, invalid, or unavailable in this user's protected storage"))
    }
}
fn storage_error(error: io::Error) -> CredentialError {
    if error.kind() == io::ErrorKind::NotFound {
        CredentialError::Missing
    } else {
        CredentialError::Unavailable
    }
}
#[cfg(unix)]
fn data_directory() -> io::Result<PathBuf> {
    let path = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                #[cfg(target_os = "macos")]
                {
                    PathBuf::from(home).join("Library/Application Support")
                }
                #[cfg(not(target_os = "macos"))]
                {
                    PathBuf::from(home).join(".local/share")
                }
            })
        })
        .filter(|path| path.is_absolute());
    path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "No absolute user data directory is available for credential storage",
        )
    })
}

#[cfg(test)]
#[path = "credentials/tests.rs"]
mod tests;
