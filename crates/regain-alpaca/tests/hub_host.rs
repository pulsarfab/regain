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

// This guard is only used for PIDs returned by this test's successful fresh
// attachment launch. Production frontends must not kill the shared host.
struct StartedHost(u32);
impl Drop for StartedHost {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &self.0.to_string()])
                .creation_flags(0x08000000)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &self.0.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
async fn attach(path: &Path) -> Value {
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        command()
            .arg("--hub-attach")
            .arg("--hub-config")
            .arg(path)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

#[tokio::test]
async fn frontend_attachment_launches_one_shared_host_and_explicit_reconnect_gets_a_new_session() {
    use regain_hub::{
        client::{Client, ClientError, ClientLimits},
        ipc::Command as Rpc,
    };
    let dir = tempfile::Builder::new()
        .prefix("regain hub \u{03bb} ")
        .tempdir()
        .unwrap();
    let path = dir.path().join("hub.json");
    let config = HubConfig::empty();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let first = attach(&path).await;
    let started = StartedHost(
        first["startedProcessId"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let endpoint = Endpoint::for_config(&path).unwrap();
    assert_eq!(first["address"], endpoint.address().to_str().unwrap());
    let a = Client::connect(
        &endpoint,
        config.instance_id,
        Duration::from_secs(5),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    let b = Client::connect(
        &endpoint,
        config.instance_id,
        Duration::from_secs(5),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(first["hostInstance"], a.hello().host_instance.to_string());
    assert_eq!(a.hello().host_instance, b.hello().host_instance);
    assert_ne!(a.hello().client_id, b.hello().client_id);
    let next = attach(&path).await;
    assert!(next["startedProcessId"].is_null());
    assert_eq!(first["hostInstance"], next["hostInstance"]);
    a.close();
    drop(a);
    assert_eq!({ b.request(Rpc::ListDevices {}) }.await.unwrap(), json!([]));
    drop(started);
    tokio::time::timeout(Duration::from_secs(5), b.closed())
        .await
        .unwrap();
    // Unix closes clients before the graceful source drain releases ownership.
    // A replacement may start only after that owner has actually exited.
    tokio::time::timeout(Duration::from_secs(5), async {
        while endpoint.try_lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        { b.request(Rpc::ListDevices {}) }.await,
        Err(ClientError::Disconnected)
    ));
    let restarted = attach(&path).await;
    let _started = StartedHost(
        restarted["startedProcessId"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_ne!(first["hostInstance"], restarted["hostInstance"]);
    let c = Client::connect(
        &endpoint,
        config.instance_id,
        Duration::from_secs(5),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    assert_ne!(b.hello().host_instance, c.hello().host_instance);
    assert_ne!(b.hello().client_id, c.hello().client_id);
    assert_eq!(c.request(Rpc::ListDevices {}).await.unwrap(), json!([]));
    c.close();
}

#[tokio::test]
async fn attachment_waits_for_a_held_owner_instead_of_starting_a_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    std::fs::write(&path, serde_json::to_vec(&HubConfig::empty()).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let owner = endpoint.try_lock().unwrap().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        command()
            .arg("--hub-attach")
            .arg("--hub-config")
            .arg(&path)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("no automatic restart was attempted"));
    assert!(endpoint.try_lock().unwrap().is_none());
    drop(owner);
    assert!(endpoint.try_lock().unwrap().is_some());
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
async fn ordinary_http_executable_attaches_to_existing_host_without_taking_ownership() {
    use tokio::io::AsyncReadExt;
    async fn http(port: u16, method: &str, path: &str, body: &str) -> Value {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            stream.take(1024 * 1024).read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        let split = response.windows(4).position(|v| v == b"\r\n\r\n").unwrap() + 4;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let value: Value = serde_json::from_slice(&response[split..]).unwrap();
        assert_eq!(value["ErrorNumber"], 0, "{value}");
        value["Value"].clone()
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let config: HubConfig = serde_json::from_str(include_str!(
        "../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = host(&path).spawn().unwrap();
    let first = probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut http_server = command()
        .args(["--hub-config"])
        .arg(&path)
        .arg("--profiles")
        .arg(dir.path().join("profiles.json"))
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-discovery")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(http_server.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(ready.contains("Alpaca listening"), "{ready}");
    let devices = http(port, "GET", "/management/v1/configureddevices", "").await;
    assert_eq!(devices.as_array().unwrap().len(), 3);
    http(
        port,
        "PUT",
        "/api/v1/safetymonitor/0/connected",
        "ClientID=8&Connected=true",
    )
    .await;
    assert_eq!(
        http(port, "GET", "/api/v1/safetymonitor/0/issafe?ClientID=8", "").await,
        false
    );
    http_server.kill().await.unwrap();
    let after = probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(first.host_instance, after.host_instance);
    // HTTP process death closes only its leases, not the shared host.
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    let mut id = 2;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = request(
                &mut stream,
                id,
                json!({"op":"sourceStatus","source":config.sources[1].id}),
            )
            .await;
            id += 1;
            if status["leaseCount"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(stream);
    owner.kill().await.unwrap();
}

#[tokio::test]
async fn actual_hub_executable_shares_simulation_controls_and_restarts_safety_unsafe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("simulated.json");
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    let output = uuid::Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({"id":source,"label":"Simulation fixture",
        "backend":{"kind":"simulated","deviceType":"safetymonitor"},"polling":{"pollSeconds":0.1}}))
        .unwrap(),
    );
    config.outputs.push(serde_json::from_value(json!({"id":output,"number":0,"label":"Simulation safety",
        "device":{"kind":"safety","members":[{"source":source,"enabled":true,
            "policy":{"safeReadingsToSafe":1,"returnToSafeHoldSeconds":0,"confirmationSeconds":0.1}}]}})).unwrap());
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = host(&path).spawn().unwrap();
    probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    let metadata = request(&mut stream, 2, json!({"op":"describeConfig"})).await;
    assert!(
        metadata["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("simulation"))
    );
    assert_eq!(
        metadata["simulationControl"]["schema"]["properties"]["safe"]["description"],
        "Simulated raw safety reading. Starts false on each new runtime."
    );
    assert_eq!(
        request(&mut stream, 3, json!({"op":"listDevices"})).await[0]["simulated"],
        true
    );
    request(&mut stream, 4, json!({"op":"connect","output":output})).await;
    request(
        &mut stream,
        5,
        json!({"op":"updateSimulation","source":source,"update":{"safe":true}}),
    )
    .await;
    let mut id = 6;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let safe = request(
                &mut stream,
                id,
                json!({"op":"get","output":output,"property":{"member":"isSafe"}}),
            )
            .await;
            id += 1;
            if safe == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let status = request(
        &mut stream,
        id,
        json!({"op":"sourceStatus","source":source}),
    )
    .await;
    assert_eq!(status["simulated"], true);
    assert_eq!(status["simulation"]["safe"], true);
    drop(stream);
    owner.kill().await.unwrap();
    let mut restarted = host(&path).spawn().unwrap();
    probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    assert_eq!(
        request(&mut stream, 2, json!({"op":"sourceStatus","source":source})).await["simulation"]["safe"],
        false
    );
    request(&mut stream, 3, json!({"op":"connect","output":output})).await;
    assert_eq!(
        request(
            &mut stream,
            4,
            json!({"op":"get","output":output,"property":{"member":"isSafe"}})
        )
        .await,
        false
    );
    drop(stream);
    restarted.kill().await.unwrap();
}

#[tokio::test]
async fn actual_hub_executable_composes_outputs_and_applies_after_nested_lease_cleanup() {
    use uuid::Uuid;
    let mut config = HubConfig::empty();
    let leaf = Uuid::new_v4();
    let virtual_source = Uuid::new_v4();
    let base = Uuid::new_v4();
    let output = Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({"id":leaf,"label":"Simulation switch",
        "backend":{"kind":"simulated","deviceType":"switch"},"polling":{"pollSeconds":0.1}}))
        .unwrap(),
    );
    config.sources.push(
        serde_json::from_value(json!({"id":virtual_source,"label":"Local switch output",
        "backend":{"kind":"virtual","output":base},"polling":{"pollSeconds":0.1}}))
        .unwrap(),
    );
    for (id, number, source, channel) in [(base, 0, leaf, 1), (output, 1, virtual_source, 0)] {
        config.outputs.push(
            serde_json::from_value(json!({"id":id,"number":number,"label":"Composed switch",
            "device":{"kind":"switch","channels":[{"id":Uuid::new_v4(),"number":0,"label":"Level",
                "readout":{"kind":"channel","source":source,"channel":channel},"writable":true,
                "minimum":0,"maximum":100,"step":1,"units":"%"}]}}))
            .unwrap(),
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = host(&path).spawn().unwrap();
    probe(&endpoint, config.instance_id, Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    request(&mut stream, 1, json!({"op":"hello"})).await;
    let metadata = request(&mut stream, 2, json!({"op":"describeConfig"})).await;
    assert!(
        metadata["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("virtualSources"))
    );
    let mut candidate = request(&mut stream, 3, json!({"op":"getConfig"})).await;
    let revision = candidate["revision"].clone();
    candidate["outputs"][1]["label"] = json!("Renamed nested output");
    request(&mut stream, 4, json!({"op":"connect","output":output})).await;
    request(&mut stream, 5, json!({"op":"put","output":output,"property":{"member":"setSwitchValue","id":0,"value":42}})).await;
    assert_eq!(
        request(&mut stream, 6, json!({"op":"sourceStatus","source":leaf})).await["simulation"]["switchValues"]
            ["1"],
        42.0
    );
    assert_eq!(
        reply(
            &mut stream,
            7,
            json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})
        )
        .await["error"]["code"],
        "connected"
    );
    request(&mut stream, 8, json!({"op":"disconnect","output":output})).await;
    let mut id = 9;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let a = request(&mut stream, id, json!({"op":"sourceStatus","source":leaf})).await;
            id += 1;
            let b = request(
                &mut stream,
                id,
                json!({"op":"sourceStatus","source":virtual_source}),
            )
            .await;
            id += 1;
            if a["leaseCount"] == 0 && b["leaseCount"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let applied = request(
        &mut stream,
        id,
        json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate}),
    )
    .await;
    assert_eq!(applied["ready"], true);
    id += 1;
    request(&mut stream, id, json!({"op":"connect","output":output})).await;
    id += 1;
    request(&mut stream, id, json!({"op":"put","output":output,"property":{"member":"setSwitchValue","id":0,"value":27}})).await;
    id += 1;
    assert_eq!(
        request(&mut stream, id, json!({"op":"sourceStatus","source":leaf})).await["simulation"]["switchValues"]
            ["1"],
        27.0
    );
    drop(stream);
    owner.kill().await.unwrap();
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
        vec!["--hub-attach"],
        vec![
            "--hub-attach",
            "--hub-host",
            "--hub-config",
            "relative.json",
        ],
        vec![
            "--hub-attach",
            "--simulate",
            "--hub-config",
            "relative.json",
        ],
        vec!["--hub-attach", "--stdio", "--hub-config", "relative.json"],
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
