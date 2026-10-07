//! Credential management uses the same private IPC/configuration transaction.
use regain_hub::{
    config::{ConfigStore, HubConfig},
    credentials::{CredentialError, CredentialStore, SecretAuthorization},
    ipc::{Limits, read_frame, serve_service_stream},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    service::{HubService, RuntimeBuilder},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncWriteExt, DuplexStream};
const FAKE: &str = "Bearer private-ipc-fixture-value";

#[tokio::test]
async fn chosen_reference_survives_a_lost_reply_and_cannot_replace_a_secret() {
    use regain_hub::factory::CredentialProvider;
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialStore::at_directory(dir.path(), &"a".repeat(64)).unwrap());
    let service = HubService::persistent_with_credentials(
        ConfigStore::new(None, HubConfig::empty()).unwrap(),
        builder(credentials.clone()),
        credentials.clone(),
    )
    .unwrap();
    let id = uuid::Uuid::new_v4();
    let reference = format!("credential-{id}");
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_service_stream(
        server,
        service.clone(),
        Limits::default(),
    ));
    rpc(&mut stream, 1, json!({"op":"hello"})).await;
    let bytes = serde_json::to_vec(&json!({"version":1,"id":2,"command":{
        "op":"createCredential","referenceId":id,"authorization":FAKE}}))
    .unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
    // Wait for the actual commit, then abandon the connection without reading
    // its response. The new setup client already knows which reference to check.
    tokio::time::timeout(Duration::from_secs(5), async {
        // Observe the store without taking the service gate before the queued
        // create has been admitted. The exclusive Windows file handle can
        // briefly make direct reads unavailable during the flush.
        loop {
            match credentials.status(&reference) {
                Ok(status) if status.present => break,
                Ok(_) | Err(CredentialError::Unavailable) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("Unexpected status during credential commit: {error:?}"),
            }
        }
        while matches!(
            service.credential_status(reference.clone()).await,
            Err(CredentialError::Busy)
        ) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    drop(stream);
    let _ = task.await.unwrap(); // A dropped peer can fail the response write.
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_service_stream(
        server,
        service.clone(),
        Limits::default(),
    ));
    rpc(&mut stream, 1, json!({"op":"hello"})).await;
    let description = rpc(&mut stream, 2, json!({"op":"describeConfig"})).await;
    assert_eq!(
        description["result"]["credentialStorage"]["clientChosenReferences"],
        true
    );
    assert_eq!(
        rpc(
            &mut stream,
            3,
            json!({"op":"credentialStatus","reference":reference})
        )
        .await["result"]["present"],
        true
    );
    assert_eq!(
        rpc(
            &mut stream,
            4,
            json!({"op":"createCredential","referenceId":id,"authorization":"Bearer replacement"})
        )
        .await["error"]["code"],
        "unavailable"
    );
    assert_eq!(credentials.authorization(&reference).unwrap(), FAKE);
    assert_eq!(
        rpc(
            &mut stream,
            5,
            json!({"op":"createCredential","referenceId":uuid::Uuid::nil(),"authorization":FAKE})
        )
        .await["error"]["code"],
        "invalidValue"
    );
    assert_eq!(
        rpc(
            &mut stream,
            6,
            json!({"op":"deleteCredential","reference":reference})
        )
        .await["result"]["removed"],
        true
    );
    drop(stream);
    task.await.unwrap().unwrap();
    service.shutdown().await.unwrap();
}

fn builder(credentials: Arc<CredentialStore>) -> Arc<RuntimeBuilder> {
    Arc::new(move |config| {
        HubRuntime::build(
            config,
            &NativeRuntime {
                cameras: None,
                directory: "unused-fixture-directory".into(),
                simulate: false,
                references: None,
            },
            &*credentials,
            Arc::new(MonotonicClock::default()),
        )
    })
}
async fn rpc(stream: &mut DuplexStream, id: u64, command: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        read_frame(stream, Duration::from_secs(3)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["id"], id);
    assert!(
        !response.to_string().contains(FAKE),
        "Secret appeared in an IPC response"
    );
    response
}
fn add_source(config: &mut HubConfig, reference: &str) {
    config.sources.push(
        serde_json::from_value(json!({
            "id":uuid::Uuid::new_v4(),"label":"Authenticated fixture",
            "backend":{"kind":"alpaca","baseUrl":"http://127.0.0.1:1/","deviceType":"safetymonitor",
                "deviceNumber":0,"credentialReference":reference}
        }))
        .unwrap(),
    );
}

#[tokio::test]
async fn private_ipc_creates_rotates_and_removes_references_without_reading_values() {
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialStore::at_directory(dir.path(), &"c".repeat(64)).unwrap());
    let service = HubService::persistent_with_credentials(
        ConfigStore::new(Some(dir.path().join("hub.json")), HubConfig::empty()).unwrap(),
        builder(credentials.clone()),
        credentials,
    )
    .unwrap();
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_service_stream(
        server,
        service.clone(),
        Limits::default(),
    ));
    let hello = rpc(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("createCredential"))
    );
    let invalid = rpc(
        &mut stream,
        2,
        json!({"op":"createCredential","authorization":format!("{FAKE}\r\n")}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], "invalidValue");
    let first = rpc(
        &mut stream,
        3,
        json!({"op":"createCredential","authorization":FAKE}),
    )
    .await;
    let reference = first["result"]["reference"].as_str().unwrap();
    let mut config = service.configuration();
    add_source(&mut config, reference);
    let applied = rpc(
        &mut stream,
        4,
        json!({"op":"applyConfig","expectedRevision":config.revision,"candidate":config}),
    )
    .await;
    assert_eq!(applied["result"]["ready"], true);
    assert_eq!(
        rpc(
            &mut stream,
            5,
            json!({"op":"deleteCredential","reference":reference})
        )
        .await["error"]["code"],
        "inUse"
    );
    let second = rpc(
        &mut stream,
        6,
        json!({"op":"createCredential","authorization":"Bearer rotated-fixture-value"}),
    )
    .await;
    let replacement = second["result"]["reference"].as_str().unwrap();
    let mut current = rpc(&mut stream, 7, json!({"op":"getConfig"})).await["result"].clone();
    current["sources"][0]["backend"]["credentialReference"] = json!(replacement);
    assert_eq!(
        rpc(
            &mut stream,
            8,
            json!({"op":"applyConfig","expectedRevision":current["revision"],"candidate":current})
        )
        .await["result"]["ready"],
        true
    );
    assert_eq!(
        rpc(
            &mut stream,
            9,
            json!({"op":"deleteCredential","reference":reference})
        )
        .await["result"]["removed"],
        true
    );
    assert_eq!(
        rpc(
            &mut stream,
            10,
            json!({"op":"credentialStatus","reference":reference})
        )
        .await["result"]["present"],
        false
    );
    assert_eq!(
        rpc(
            &mut stream,
            11,
            json!({"op":"credentialStatus","reference":replacement})
        )
        .await["result"]["present"],
        true
    );
    let saved = std::fs::read_to_string(dir.path().join("hub.json")).unwrap();
    assert!(!saved.contains(FAKE));
    assert!(!saved.contains("Bearer rotated-fixture-value"));
    drop(stream);
    task.await.unwrap().unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .create_credential(SecretAuthorization::new(FAKE.into()))
            .await
            .unwrap_err(),
        CredentialError::Unavailable
    );
}

#[tokio::test]
async fn credential_deletion_cannot_race_an_accepted_configuration_update() {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialStore::at_directory(dir.path(), &"d".repeat(64)).unwrap());
    let reference = credentials
        .create(SecretAuthorization::new(FAKE.into()))
        .unwrap()
        .reference;
    let base = builder(credentials.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let proceed = Mutex::new(proceed);
    let gate = calls.clone();
    let builder: Arc<RuntimeBuilder> = Arc::new(move |config| {
        if gate.fetch_add(1, SeqCst) > 0 {
            entered.send(()).unwrap();
            proceed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        base(config)
    });
    let service = HubService::persistent_with_credentials(
        ConfigStore::new(None, HubConfig::empty()).unwrap(),
        builder,
        credentials.clone(),
    )
    .unwrap();
    let mut candidate = service.configuration();
    add_source(&mut candidate, &reference);
    let applying = tokio::spawn({
        let service = service.clone();
        async move { service.apply(candidate.revision, candidate).await }
    });
    tokio::time::timeout(Duration::from_secs(3), started.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        service.delete_credential(reference.clone()).await,
        Err(CredentialError::Busy)
    ));
    assert!(matches!(
        service.credential_status(reference.clone()).await,
        Err(CredentialError::Busy)
    ));
    // Dropping the apply waiter cannot release the transaction's gate early.
    applying.abort();
    let _ = applying.await;
    assert!(matches!(
        service.delete_credential(reference.clone()).await,
        Err(CredentialError::Busy)
    ));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match service.delete_credential(reference.clone()).await {
                Err(CredentialError::Busy) => tokio::task::yield_now().await,
                Err(CredentialError::InUse) => break,
                _ => panic!("Configured credential must remain protected from deletion"),
            }
        }
    })
    .await
    .unwrap();
    assert!(credentials.status(&reference).unwrap().present);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn hosts_without_storage_do_not_advertise_or_accept_credential_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialStore::at_directory(dir.path(), &"e".repeat(64)).unwrap());
    let service = HubService::persistent(
        ConfigStore::new(None, HubConfig::empty()).unwrap(),
        builder(credentials),
    )
    .unwrap();
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_service_stream(
        server,
        service.clone(),
        Limits::default(),
    ));
    let hello = rpc(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        !hello["result"]["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("createCredential"))
    );
    assert_eq!(
        rpc(
            &mut stream,
            2,
            json!({"op":"createCredential","authorization":FAKE})
        )
        .await["error"]["code"],
        "unsupported"
    );
    assert!(
        rpc(&mut stream, 3, json!({"op":"describeConfig"})).await["result"]["credentialStorage"]
            .is_null()
    );
    drop(stream);
    task.await.unwrap().unwrap();
    service.shutdown().await.unwrap();
}
