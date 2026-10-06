use super::{Endpoint, denied, invalid};
use std::{
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    pin::Pin,
    ptr::null_mut,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl, GetTokenInformation,
        OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, READ_CONTROL,
        SECURITY_IDENTIFICATION,
    },
    System::{
        Com::CoTaskMemFree,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
};

struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
unsafe fn sid_text(sid: *mut c_void) -> io::Result<String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _owned = Local(text.cast());
    let mut len = 0;
    while len < 256 && unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    if len == 256 {
        return Err(denied());
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) }).map_err(|_| denied())
}
pub fn user() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 65536 {
        return Err(denied());
    }
    // TOKEN_USER contains pointers: allocate word-aligned storage.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    unsafe { sid_text(user.User.Sid) }
}
fn descriptor(user: &str) -> io::Result<Local> {
    let text = wide(std::ffi::OsStr::new(&format!(
        "O:{user}D:P(A;OICI;GA;;;{user})"
    )));
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(descriptor))
}
fn attributes(sd: &Local) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    }
}
fn verify_private(handle: HANDLE, user: &str) -> io::Result<()> {
    let mut owner = null_mut();
    let mut acl: *mut ACL = null_mut();
    let mut sd = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut sd,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _sd = Local(sd);
    if owner.is_null()
        || acl.is_null()
        || unsafe { sid_text(owner)? } != user
        || unsafe { (*acl).AceCount } != 1
    {
        return Err(denied());
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(denied());
    }
    let mut ace = null_mut();
    if unsafe { GetAce(acl, 0, &mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if header.AceType != 0 || (header.AceSize as usize) < size_of::<ACCESS_ALLOWED_ACE>() {
        return Err(denied());
    }
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
    if unsafe { sid_text(sid)? } != user {
        return Err(denied());
    }
    Ok(())
}
pub fn path_bytes(path: &Path) -> io::Result<Vec<u8>> {
    // Canonical Win32 paths may differ in case; share their ownership identity.
    Ok(path
        .to_str()
        .ok_or_else(|| invalid("Hub configuration path must be valid Unicode"))?
        .to_lowercase()
        .into_bytes())
}
pub fn root(_: &str) -> io::Result<PathBuf> {
    let mut path = null_mut();
    if unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, null_mut(), &mut path) } < 0 {
        return Err(denied());
    }
    let mut len = 0;
    while unsafe { *path.add(len) } != 0 {
        len += 1;
    }
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(path, len) });
    unsafe {
        CoTaskMemFree(path.cast());
    }
    Ok(PathBuf::from(value.map_err(|_| denied())?)
        .join("Regain")
        .join("Hub"))
}
pub fn prepare_root(path: &Path, user: &str) -> io::Result<()> {
    fs::create_dir_all(path.parent().ok_or_else(denied)?)?;
    let descriptor = descriptor(user)?;
    let attributes = attributes(&descriptor);
    if unsafe { CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied());
    }
    verify_private(file.as_raw_handle(), user)
}
pub fn lock_file(path: &Path, user: &str) -> io::Result<File> {
    let descriptor = descriptor(user)?;
    let attributes = attributes(&descriptor);
    let handle = unsafe {
        CreateFileW(
            wide(path.as_os_str()).as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_ALWAYS,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied());
    }
    verify_private(file.as_raw_handle(), user)?;
    Ok(file)
}
pub fn address(endpoint: &Endpoint) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\PulsarFab.Regain.Hub.{}", endpoint.key))
}
pub type Listener = NamedPipeServer;
pub enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}
fn create(endpoint: &Endpoint, first: bool) -> io::Result<NamedPipeServer> {
    let descriptor = descriptor(&endpoint.user)?;
    let mut attributes = attributes(&descriptor);
    let pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(33)
            .create_with_security_attributes_raw(
                address(endpoint),
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )?
    };
    verify_private(pipe.as_raw_handle(), &endpoint.user)?;
    Ok(pipe)
}
pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    create(endpoint, true)
}
pub async fn accept(listener: &mut Listener, endpoint: &Endpoint) -> io::Result<Stream> {
    listener.connect().await?;
    let replacement = create(endpoint, false)?;
    Ok(Stream::Server(std::mem::replace(listener, replacement)))
}
pub async fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    let pipe = ClientOptions::new()
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(address(endpoint))?;
    verify_private(pipe.as_raw_handle(), &endpoint.user)?;
    Ok(Stream::Client(pipe))
}
pub fn not_ready(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Client(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Client(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_flush(cx),
            Self::Client(stream) => Pin::new(stream).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Client(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_storage_rejects_an_existing_everyone_acl_without_repairing_it() {
        let dir = tempfile::tempdir().unwrap();
        let user = user().unwrap();
        let text = wide(std::ffi::OsStr::new(&format!(
            "O:{user}D:P(A;OICI;GA;;;WD)"
        )));
        let mut sd = null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    null_mut(),
                )
            },
            0
        );
        let sd = Local(sd);
        let attributes = attributes(&sd);
        let path = dir.path().join("public");
        assert_ne!(
            unsafe { CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &attributes) },
            0
        );
        assert_eq!(
            prepare_root(&path, &user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // A second call still rejects the original descriptor: no implicit ACL repair.
        assert_eq!(
            prepare_root(&path, &user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let lock = dir.path().join("public.lock");
        let handle = unsafe {
            CreateFileW(
                wide(lock.as_os_str()).as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                OPEN_ALWAYS,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        drop(Handle(handle));
        assert_eq!(
            lock_file(&lock, &user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
