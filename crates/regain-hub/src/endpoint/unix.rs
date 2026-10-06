use super::{Endpoint, denied};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use tokio::net::{UnixListener, UnixStream};

pub type Stream = UnixStream;
pub struct Listener {
    socket: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}
pub fn user() -> io::Result<String> {
    Ok(unsafe { libc::geteuid() }.to_string())
}
pub fn path_bytes(path: &Path) -> io::Result<Vec<u8>> {
    Ok(path.as_os_str().as_bytes().to_vec())
}
pub fn root(user: &str) -> io::Result<PathBuf> {
    Ok(PathBuf::from(format!("/tmp/regain-hub-{user}")))
}
pub fn prepare_root(path: &Path, _: &str) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(denied());
    }
    Ok(())
}
pub fn lock_file(path: &Path, _: &str) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    verify_file(&file)?;
    Ok(file)
}
pub fn create_private(path: &Path, _: &str) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    verify_file(&file)?;
    Ok(file)
}
pub fn read_private(path: &Path, _: &str) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    verify_file(&file)?;
    Ok(file)
}
fn verify_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(denied());
    }
    Ok(())
}
pub fn address(endpoint: &Endpoint) -> PathBuf {
    endpoint.root.join(format!("{}.sock", endpoint.key))
}
pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    let path = address(endpoint);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(denied());
            }
            // The caller owns the OS lock; only a stale socket is removed.
            fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let socket = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(&path)?;
    Ok(Listener {
        socket,
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}
pub async fn accept(listener: &mut Listener, _: &Endpoint) -> io::Result<Stream> {
    loop {
        let stream = match listener.socket.accept().await {
            Ok((stream, _)) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::Interrupted
                ) =>
            {
                tokio::task::yield_now().await;
                continue;
            }
            Err(error) => return Err(error),
        };
        // A queued peer may have closed before admission. Failed credentials
        // reject that connection; they do not retire the listening service.
        if stream
            .peer_cred()
            .is_ok_and(|peer| peer.uid() == unsafe { libc::geteuid() })
        {
            return Ok(stream);
        }
        drop(stream);
        tokio::task::yield_now().await;
    }
}
pub async fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    let path = address(endpoint);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(denied());
    }
    let stream = UnixStream::connect(path).await?;
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(denied());
    }
    Ok(stream)
}
pub fn not_ready(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}
impl Drop for Listener {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn private_storage_rejects_links_and_broad_permissions_without_repairing_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("private");
        let user = user().unwrap();
        prepare_root(&root, &user).unwrap();
        let alias = dir.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(prepare_root(&alias, &user).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            prepare_root(&root, &user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o755);

        let path = dir.path().join("owner.lock");
        drop(lock_file(&path, &user).unwrap());
        let link = dir.path().join("link.lock");
        symlink(&path, &link).unwrap();
        assert!(lock_file(&link, &user).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(lock_file(&path, &user).is_err());
        fs::remove_file(&link).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            lock_file(&path, &user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
    }
}
