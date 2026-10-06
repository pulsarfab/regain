//! Exercise the production executable in private hub mode; no hardware I/O.
use regain_hub::{config::HubConfig, endpoint::Endpoint, host::probe};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_regain-alpaca"));
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}

async fn request(stream: &mut regain_hub::endpoint::LocalStream, id: u64, command: Value) -> Value {
    let reply = reply(stream, id, command).await;
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"].clone()
}
async fn reply(stream: &mut regain_hub::endpoint::LocalStream, id: u64, command: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        regain_hub::ipc::read_frame(stream, Duration::from_secs(5)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let reply: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["id"], id);
    reply
}

#[tokio::test]
async fn actual_hub_executable_serves_network_safety_and_observes_unsafe_changes() {
    use axum::{
        Json, Router,
        extract::{Path as RoutePath, State},
        routing::get,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    };
    async fn upstream(
        State((safe, requests)): State<(Arc<AtomicBool>, Arc<AtomicUsize>)>,
        RoutePath(member): RoutePath<String>,
        headers: axum::http::HeaderMap,
    ) -> Json<Value> {
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer production-fixture-only"
        );
        requests.fetch_add(1, SeqCst);
        let value = match member.as_str() {
            "interfaceversion" => json!(3),
            "connected" => json!(true),
            "issafe" => json!(safe.load(SeqCst)),
            _ => panic!("Unexpected source member: {member}"),
        };
        Json(json!({"ErrorNumber":0,"ErrorMessage":"","Value":value}))
    }
    let safe = Arc::new(AtomicBool::new(true));
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/api/v1/safetymonitor/0/{member}", get(upstream))
        .with_state((safe.clone(), requests.clone()));
    let upstream = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({
            "id":source, "label":"Loopback safety fixture",
            "backend":{"kind":"alpaca","baseUrl":format!("http://{address}/"),
                "deviceType":"safetymonitor","deviceNumber":0}
        }))
        .unwrap(),
    );
    config.outputs.push(
        serde_json::from_value(json!({
            "id":uuid::Uuid::new_v4(),"number":0,"label":"Shared safety fixture",
            "device":{"kind":"safety","members":[{"source":source,"enabled":true}]}
        }))
        .unwrap(),
    );
    config.sources[0].polling.poll_seconds = 0.1;
    config.sources[0].polling.request_timeout_seconds = 0.2;
    if let regain_hub::config::VirtualDevice::Safety { members } = &mut config.outputs[0].device {
        members.truncate(1);
        members[0].policy.safe_readings_to_safe = 1;
        members[0].policy.return_to_safe_hold_seconds = 0.0;
        members[0].policy.confirmation_seconds = 0.1;
    }
    assert!(config.validate().is_empty());
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = host(&path).spawn().unwrap();
    probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    let credential = request(
        &mut stream,
        2,
        json!({"op":"createCredential", "authorization":"Bearer production-fixture-only"}),
    )
    .await;
    let reference = credential["reference"].as_str().unwrap().to_string();
    // Clean only the fake record created by this test, including panic paths.
    struct Cleanup(regain_hub::credentials::CredentialStore, String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.delete(&self.1);
        }
    }
    let _cleanup = Cleanup(
        regain_hub::credentials::CredentialStore::for_endpoint(&endpoint).unwrap(),
        reference.clone(),
    );
    let mut authenticated = request(&mut stream, 3, json!({"op":"getConfig"})).await;
    authenticated["sources"][0]["backend"]["credentialReference"] = json!(reference);
    request(
        &mut stream,
        4,
        json!({"op":"applyConfig", "expectedRevision":config.revision, "candidate":authenticated}),
    )
    .await;
    assert_eq!(
        reply(
            &mut stream,
            5,
            json!({"op":"deleteCredential", "reference":reference})
        )
        .await["error"]["code"],
        "inUse"
    );
    let output = config.outputs[0].id;
    request(&mut stream, 6, json!({"op":"connect","output":output})).await;
    let mut id = 7;
    for expected in [true, false] {
        safe.store(expected, SeqCst);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = request(
                    &mut stream,
                    id,
                    json!({"op":"get","output":output,"property":{"member":"isSafe"}}),
                )
                .await;
                id += 1;
                if value == json!(expected) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
    }
    let mut candidate = request(&mut stream, id, json!({"op":"getConfig"})).await;
    assert!(!candidate.to_string().contains("production-fixture-only"));
    id += 1;
    let old_revision = candidate["revision"].clone();
    candidate["outputs"][0]["label"] = json!("Renamed safety output");
    let connected = reply(
        &mut stream,
        id,
        json!({"op":"applyConfig", "expectedRevision":old_revision, "candidate":candidate}),
    )
    .await;
    id += 1;
    assert_eq!(connected["error"]["code"], "connected");
    request(&mut stream, id, json!({"op":"disconnect", "output":output})).await;
    id += 1;
    let applied = request(
        &mut stream,
        id,
        json!({"op":"applyConfig", "expectedRevision":old_revision, "candidate":candidate}),
    )
    .await;
    id += 1;
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["ready"], true);
    let current = request(&mut stream, id, json!({"op":"getConfig"})).await;
    id += 1;
    assert_ne!(current["revision"], old_revision);
    assert_eq!(current["revision"], applied["configurationRevision"]);
    assert_eq!(current["outputs"][0]["label"], "Renamed safety output");
    let listed = request(&mut stream, id, json!({"op":"listDevices"})).await;
    id += 1;
    assert_eq!(listed[0]["label"], "Renamed safety output");
    let stale = reply(
        &mut stream,
        id,
        json!({"op":"applyConfig", "expectedRevision":old_revision, "candidate":candidate}),
    )
    .await;
    id += 1;
    assert_eq!(stale["error"]["code"], "revisionConflict");
    request(&mut stream, id, json!({"op":"connect", "output":output})).await;
    id += 1;
    assert_eq!(
        request(
            &mut stream,
            id,
            json!({"op":"get", "output":output,"property":{"member":"isSafe"}})
        )
        .await,
        false
    );
    drop(stream);
    owner.kill().await.unwrap();
    let saved = regain_hub::config::ConfigStore::load(&path)
        .unwrap()
        .snapshot();
    assert_eq!(
        serde_json::to_value(saved.revision).unwrap(),
        current["revision"]
    );
    let mut restarted = host(&path).spawn().unwrap();
    let hello = probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(hello.configuration_revision, saved.revision);
    assert!(
        hello
            .operations
            .iter()
            .any(|operation| operation == "applyConfig")
    );
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    let status = request(
        &mut stream,
        2,
        json!({"op":"credentialStatus", "reference":reference}),
    )
    .await;
    assert_eq!(status["present"], true);
    let description = request(&mut stream, 3, json!({"op":"describeConfig"})).await;
    assert_eq!(
        description["credentialStorage"]["input"]["authorization"]["writeOnly"],
        true
    );
    for response in [status, description] {
        assert!(!response.to_string().contains("production-fixture-only"));
    }
    // Reopened storage must authenticate a newly constructed source after restart.
    let before = requests.load(SeqCst);
    request(&mut stream, 4, json!({"op":"connect", "output":output})).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while requests.load(SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    request(&mut stream, 5, json!({"op":"disconnect", "output":output})).await;
    let mut candidate = request(&mut stream, 6, json!({"op":"getConfig"})).await;
    let revision = candidate["revision"].clone();
    candidate["sources"][0]["backend"]["credentialReference"] = Value::Null;
    request(
        &mut stream,
        7,
        json!({"op":"applyConfig", "expectedRevision":revision, "candidate":candidate}),
    )
    .await;
    assert_eq!(
        request(
            &mut stream,
            8,
            json!({"op":"deleteCredential", "reference":reference})
        )
        .await["removed"],
        true
    );
    assert_eq!(
        request(
            &mut stream,
            9,
            json!({"op":"credentialStatus", "reference":reference})
        )
        .await["present"],
        false
    );
    drop(stream);
    restarted.kill().await.unwrap();
    upstream.abort();
    let _ = upstream.await;
}
fn host(path: &Path) -> Command {
    let mut command = command();
    command
        .arg("--hub-host")
        .arg("--hub-config")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

#[tokio::test]
async fn actual_hub_executable_probes_an_existing_owner_and_recovers_after_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let config = HubConfig::empty();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = host(&path).spawn().unwrap();
    let mut stdout = BufReader::new(owner.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(line.contains("local IPC only"), "{line}");
    let first = probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(15), host(&path).output())
        .await
        .unwrap()
        .unwrap();
    assert!(second.status.success());
    assert!(
        String::from_utf8(second.stdout)
            .unwrap()
            .contains(&first.host_instance.to_string())
    );
    assert!(owner.try_wait().unwrap().is_none());
    assert!(endpoint.try_lock().unwrap().is_none());
    owner.kill().await.unwrap();
    assert!(endpoint.try_lock().unwrap().is_some());
    let mut restarted = host(&path).spawn().unwrap();
    let next = probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    assert_ne!(first.host_instance, next.host_instance);
    restarted.kill().await.unwrap();
}

#[tokio::test]
async fn hub_mode_rejects_ambiguous_options_and_invalid_configuration() {
    for args in [
        vec!["--hub-host"],
        vec!["--hub-config", "relative.json"],
        vec!["--hub-host", "--hub-config", "relative.json"],
        vec!["--hub-host", "--hub-config", "relative.json", "--stdio"],
        vec![
            "--hub-host",
            "--hub-config",
            "relative.json",
            "--port",
            "11111",
        ],
    ] {
        let output = tokio::time::timeout(Duration::from_secs(5), command().args(args).output())
            .await
            .unwrap()
            .unwrap();
        assert!(!output.status.success());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    std::fs::write(&path, br#"{"secret":"must not appear in diagnostics"}"#).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        host(&path).stderr(Stdio::piped()).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("must not appear")
    );
    assert!(
        Endpoint::for_config(&path)
            .unwrap()
            .try_lock()
            .unwrap()
            .is_some()
    );
}
