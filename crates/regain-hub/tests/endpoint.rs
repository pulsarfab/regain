use regain_hub::{
    config::HubConfig,
    endpoint::Endpoint,
    factory::NoCredentials,
    ipc::{Limits, read_frame, serve_stream},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

fn config(path: &Path) -> HubConfig {
    let value = json!({"schemaVersion":1,"revision":Uuid::new_v4(),"instanceId":Uuid::new_v4(),"sources":[],"outputs":[]});
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    serde_json::from_value(value).unwrap()
}

#[test]
fn canonical_aliases_and_atomic_replacement_share_the_os_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    config(&path);
    let endpoint = Endpoint::for_config(&path).unwrap();
    let owner = endpoint.try_lock().unwrap().unwrap();
    let alias = Endpoint::for_config(&dir.path().join(".").join("hub.json")).unwrap();
    assert_eq!(alias.key(), endpoint.key());
    assert!(alias.try_lock().unwrap().is_none());
    #[cfg(windows)]
    {
        let uppercase = std::path::PathBuf::from(path.to_str().unwrap().to_uppercase());
        assert_eq!(
            Endpoint::for_config(&uppercase).unwrap().key(),
            endpoint.key()
        );
    }
    let replacement = dir.path().join("replacement.json");
    config(&replacement);
    fs::rename(replacement, &path).unwrap();
    assert_eq!(Endpoint::for_config(&path).unwrap().key(), endpoint.key());
    assert!(
        Endpoint::for_config(&path)
            .unwrap()
            .try_lock()
            .unwrap()
            .is_none()
    );
    drop(owner);
    assert!(endpoint.try_lock().unwrap().is_some());
}

#[tokio::test]
async fn local_streams_are_bidirectional_and_hold_ownership_through_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    config(&path);
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let (client, server) =
        tokio::join!(endpoint.connect(Duration::from_secs(2)), listener.accept());
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    client.write_all(b"hello").await.unwrap();
    let mut bytes = [0u8; 5];
    server.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"hello");
    server.write_all(b"world").await.unwrap();
    client.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"world");
    drop(listener);
    assert!(endpoint.try_lock().unwrap().is_none());
    drop(server);
    drop(client);
    assert!(endpoint.try_lock().unwrap().is_some());
}

#[tokio::test]
async fn real_endpoint_serves_versioned_ipc_and_waits_only_for_bounded_startup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let config = config(&path);
    let endpoint = Endpoint::for_config(&path).unwrap();
    assert_eq!(
        endpoint
            .connect(Duration::from_millis(50))
            .await
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    let runtime = HubRuntime::build(
        config.clone(),
        &NativeRuntime {
            cameras: None,
            directory: dir.path().into(),
            simulate: false,
            references: None,
        },
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let mut listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        serve_stream(stream, runtime, Limits::default())
            .await
            .unwrap();
    });
    let mut client = endpoint.connect(Duration::from_secs(2)).await.unwrap();
    let request =
        serde_json::to_vec(&json!({"version":1,"id":1,"command":{"op":"hello"}})).unwrap();
    client
        .write_all(&(request.len() as u32).to_le_bytes())
        .await
        .unwrap();
    client.write_all(&request).await.unwrap();
    let response = read_frame(&mut client, Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(
        response["result"]["instanceId"],
        config.instance_id.to_string()
    );
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn endpoint_requires_an_existing_absolute_file() {
    assert!(Endpoint::for_config(Path::new("relative.json")).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(Endpoint::for_config(dir.path()).is_err());
    assert!(Endpoint::for_config(&dir.path().join("missing.json")).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_is_private_and_regular_files_are_never_removed_as_stale_sockets() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    config(&path);
    let endpoint = Endpoint::for_config(&path).unwrap();
    let address = endpoint.address();
    let owner = endpoint.try_lock().unwrap().unwrap();
    fs::write(&address, b"do not remove").unwrap();
    assert!(owner.bind().is_err());
    assert_eq!(fs::read(&address).unwrap(), b"do not remove");
    fs::remove_file(&address).unwrap();
    let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    assert_eq!(fs::metadata(&address).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(address.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(listener);
    assert!(!address.exists());

    let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    fs::remove_file(&address).unwrap();
    fs::write(&address, b"replacement belongs to someone else").unwrap();
    drop(listener);
    assert_eq!(
        fs::read(&address).unwrap(),
        b"replacement belongs to someone else"
    );
    fs::remove_file(&address).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_pipe_denies_anonymous_access() {
    use windows_sys::Win32::{
        Security::{ImpersonateAnonymousToken, RevertToSelf},
        System::Threading::GetCurrentThread,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    config(&path);
    let endpoint = Endpoint::for_config(&path).unwrap();
    let _listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let path = endpoint.address();
    std::thread::spawn(move || {
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let _revert = Revert;
        let result = fs::OpenOptions::new().read(true).write(true).open(path);
        assert_eq!(
            result.err().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    })
    .join()
    .unwrap();
}
