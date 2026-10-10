//! User-scoped local endpoints and process ownership. The lock file is separate
//! from the atomically replaced configuration and is never unlinked on release.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(unix)]
#[path = "endpoint/unix.rs"]
pub(crate) mod platform;
#[cfg(windows)]
#[path = "endpoint/windows.rs"]
pub(crate) mod platform;

#[derive(Clone)]
pub struct Endpoint {
    config: PathBuf,
    key: String,
    root: PathBuf,
    user: String,
}
impl Endpoint {
    pub fn for_config(config: &Path) -> io::Result<Self> {
        if !config.is_absolute() {
            return Err(invalid("Hub configuration path must be absolute"));
        }
        let config = config.canonicalize()?;
        if !config.is_file() {
            return Err(invalid("Hub configuration must be a regular file"));
        }
        let user = platform::user()?;
        let mut hash = Sha256::new();
        hash.update(user.as_bytes());
        hash.update([0]);
        hash.update(platform::path_bytes(&config)?);
        let key = format!("{:x}", hash.finalize());
        let root = platform::root(&user)?;
        platform::prepare_root(&root, &user)?;
        Ok(Self {
            config,
            key,
            root,
            user,
        })
    }
    pub fn config_path(&self) -> &Path {
        &self.config
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn address(&self) -> PathBuf {
        platform::address(self)
    }
    /// An OS-held lock, not file contents, establishes ownership. No PID files
    /// or stale-file deletion can grant a second host authority.
    pub fn try_lock(&self) -> io::Result<Option<HostLock>> {
        let file = platform::lock_file(&self.root.join(format!("{}.lock", self.key)), &self.user)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(HostLock {
                endpoint: self.clone(),
                _file: file,
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
    /// Wait for a local endpoint to appear, checking its owner before returning.
    /// Callers must also complete the versioned IPC hello before using it.
    pub async fn connect(&self, deadline: Duration) -> io::Result<LocalStream> {
        tokio::time::timeout(deadline, async {
            loop {
                match platform::connect(self).await {
                    Ok(inner) => {
                        return Ok(LocalStream {
                            inner,
                            _owner: None,
                        });
                    }
                    Err(error) if platform::not_ready(&error) => {
                        tokio::time::sleep(Duration::from_millis(25)).await
                    }
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Hub endpoint did not become ready",
            ))
        })
    }
}

pub struct HostLock {
    endpoint: Endpoint,
    _file: File,
}
impl Drop for HostLock {
    fn drop(&mut self) {
        // Last listener/accepted-stream owner has drained. Closing this file
        // alone can leave a Unix flock held by a concurrent fork's inherited
        // descriptor until that child execs; explicitly retire our authority.
        let _ = self._file.unlock();
    }
}
impl HostLock {
    pub fn bind(self) -> io::Result<Listener> {
        let inner = platform::bind(&self.endpoint)?;
        Ok(Listener {
            inner,
            owner: Arc::new(self),
        })
    }
}
pub struct Listener {
    inner: platform::Listener,
    owner: Arc<HostLock>,
}
impl Listener {
    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        let inner = platform::accept(&mut self.inner, &self.owner.endpoint).await?;
        Ok(LocalStream {
            inner,
            _owner: Some(self.owner.clone()),
        })
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.owner.endpoint
    }
}

/// Accepted streams retain the owner lock even if the listening task is dropped.
/// The next host cannot open sources until the old connections are closed.
pub struct LocalStream {
    inner: platform::Stream,
    _owner: Option<Arc<HostLock>>,
}
impl AsyncRead for LocalStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for LocalStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Hub endpoint storage or owner is not private to this user",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn final_stream_release_unlocks_even_while_a_duplicated_descriptor_remains_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hub.json");
        std::fs::write(&path, b"{}").unwrap();
        let endpoint = Endpoint::for_config(&path).unwrap();
        let owner = endpoint.try_lock().unwrap().unwrap();
        // A duplicate has the same flock lifetime as a descriptor temporarily
        // inherited by another thread's fork before exec/close-on-exec.
        let duplicate = owner._file.try_clone().unwrap();
        let mut listener = owner.bind().unwrap();
        let (client, accepted) =
            tokio::join!(endpoint.connect(Duration::from_secs(2)), listener.accept());
        let client = client.unwrap();
        let accepted = accepted.unwrap();
        drop(listener);
        assert!(endpoint.try_lock().unwrap().is_none());
        drop(accepted);
        let successor = endpoint.try_lock().unwrap().unwrap();
        // Closing an old duplicate must not unlock the successor's distinct
        // open file description or bypass the surviving stream check above.
        drop(duplicate);
        assert!(endpoint.try_lock().unwrap().is_none());
        drop(client);
        drop(successor);
        assert!(endpoint.try_lock().unwrap().is_some());
    }
}
