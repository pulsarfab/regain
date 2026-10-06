use super::*;
use regain_core::CancellationToken;
use regain_hub::{
    endpoint::{Endpoint, LocalStream},
    host::{MAX_CLIENTS, probe, serve},
    ipc::{Limits, read_frame},
};
use tokio::io::AsyncWriteExt;

fn endpoint(f: &Fixture) -> (tempfile::TempDir, Endpoint) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    std::fs::write(&path, serde_json::to_vec(&f.config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    (dir, endpoint)
}
async fn hello(stream: &mut LocalStream) {
    let bytes = br#"{"version":1,"id":1,"command":{"op":"hello"}}"#;
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(bytes).await.unwrap();
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame(stream, Duration::from_secs(2)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let reply: Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["version"], 1);
    assert_eq!(reply["id"], 1);
    assert!(reply.get("result").is_some());
}

#[tokio::test]
async fn host_bounds_clients_and_one_bad_client_does_not_stop_the_service() {
    let f = fixture();
    let (_dir, endpoint) = endpoint(&f);
    let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(serve(
        listener,
        f.runtime.clone(),
        Limits::default(),
        stop.clone(),
    ));
    let mut clients = Vec::new();
    for _ in 0..MAX_CLIENTS {
        let mut stream = endpoint.connect(Duration::from_secs(2)).await.unwrap();
        hello(&mut stream).await;
        clients.push(stream);
    }
    let blocked = probe(&endpoint, f.config.instance_id, Duration::from_millis(100)).await;
    assert!(blocked.is_err());
    // Retiring a malformed client frees a slot while the others remain usable.
    clients[0].write_all(&1u32.to_le_bytes()).await.unwrap();
    clients[0].write_all(b"{").await.unwrap();
    let resumed = probe(&endpoint, f.config.instance_id, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(resumed.host_instance, f.runtime.runtime_id());
    assert!(endpoint.try_lock().unwrap().is_none());
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(endpoint.try_lock().unwrap().is_some());
    assert!(f.devices.iter().all(|d| d.connects.load(SeqCst) == 0));
}

#[tokio::test(start_paused = true)]
async fn cancelled_host_waiter_keeps_ownership_until_uncertain_cleanup_finishes() {
    let f = fixture();
    let (_dir, endpoint) = endpoint(&f);
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    settle().await;
    let held = client.connection(f.safety).unwrap();
    let mut safety = held.safety().unwrap().subscribe();
    assert!(safety.borrow_and_update().is_safe);
    f.devices[2].hang_disconnect.store(true, SeqCst);
    let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let service = serve(
        listener,
        f.runtime.clone(),
        Limits::default(),
        CancellationToken::new(),
    );
    // The supervisor already exists even if its waiter was never polled.
    drop(service);
    settle().await;
    assert!(!safety.borrow().is_safe);
    assert!(endpoint.try_lock().unwrap().is_none());
    assert_eq!(f.devices[2].disconnects.load(SeqCst), 1);
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert!(endpoint.try_lock().unwrap().is_some());
    let errors = f.runtime.shutdown().await.unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].1.kind, ErrorKind::Uncertain);
    assert_eq!(f.devices[2].disconnects.load(SeqCst), 1);
    assert!(f.runtime.client().connect(f.safety).await.is_err());
}

#[tokio::test]
async fn readiness_rejects_a_wrong_hub_identity_and_a_silent_endpoint() {
    let f = fixture();
    let (_dir, endpoint) = endpoint(&f);
    let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(serve(
        listener,
        f.runtime.clone(),
        Limits::default(),
        stop.clone(),
    ));
    assert_eq!(
        probe(&endpoint, Uuid::new_v4(), Duration::from_secs(2))
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    let hello = probe(&endpoint, f.config.instance_id, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(hello.configuration_revision, f.config.revision);
    stop.cancel();
    task.await.unwrap().unwrap();
    let mut listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
    let silent = tokio::spawn(async move {
        let _stream = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    assert_eq!(
        probe(&endpoint, f.config.instance_id, Duration::from_millis(100))
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    silent.abort();
    let _ = silent.await;
}
