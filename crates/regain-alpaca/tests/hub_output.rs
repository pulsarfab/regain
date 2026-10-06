//! Real private endpoint and production HTTP router, using explicit simulation.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use regain_alpaca::{
    hub_output::Publisher,
    profile::Profiles,
    server::{Log, Server},
};
use regain_core::{CancellationToken, Runtime};
use regain_hub::{
    config::{HubConfig, VirtualDevice},
    endpoint::Endpoint,
    factory::NoCredentials,
    host,
    ipc::Limits,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::SimulationUpdate,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    config: HubConfig,
    hub: Arc<HubRuntime>,
    server: Arc<Server>,
    router: Router,
    stop: CancellationToken,
    host: tokio::task::JoinHandle<Result<(), host::HostError>>,
}
impl Fixture {
    async fn new() -> Self {
        let mut config: HubConfig = serde_json::from_str(include_str!(
            "../../regain-hub/examples/simulated-observatory.json"
        ))
        .unwrap();
        // Non-contiguous output numbers must survive discovery and routing.
        config.outputs[0].number = 7;
        config.outputs[1].number = 3;
        config.outputs[2].number = 12;
        for source in &mut config.sources {
            source.polling.poll_seconds = 0.1;
        }
        if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
            members[0].policy.safe_readings_to_safe = 1;
            members[0].policy.return_to_safe_hold_seconds = 0.0;
        }
        Self::from_config(config).await
    }
    async fn from_config(config: HubConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hub.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let endpoint = Endpoint::for_config(&path).unwrap();
        let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
        let directory = dir.path().to_path_buf();
        let service = regain_hub::service::HubService::persistent(
            regain_hub::config::ConfigStore::load(&path).unwrap(),
            Arc::new(move |config| {
                HubRuntime::build(
                    config,
                    &NativeRuntime {
                        directory: directory.clone(),
                        simulate: false,
                    },
                    &NoCredentials,
                    Arc::new(MonotonicClock::default()),
                )
            }),
        )
        .unwrap();
        let hub = service.runtime().unwrap();
        let stop = CancellationToken::new();
        let host = tokio::spawn(host::serve_service(
            listener,
            service,
            Limits::default(),
            stop.clone(),
        ));
        let publisher = Publisher::connect(endpoint, config.instance_id)
            .await
            .unwrap();
        let server = Server::with_hub(
            Arc::new(Profiles::new(None).unwrap()),
            Runtime {
                directory: dir.path().into(),
                sdk: dir.path().join("unused"),
                simulate: false,
                sdk_simulation: None,
            },
            Log::new(None),
            Some(publisher),
        );
        let router = server.router();
        Self {
            _dir: dir,
            config,
            hub,
            server,
            router,
            stop,
            host,
        }
    }
    async fn finish(self) {
        self.server.shutdown().await;
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.host)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    async fn call(&self, method: &str, path: &str, data: &str) -> Value {
        let (status, body) = request(&self.router, method, path, data).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        body
    }
    async fn ok(&self, method: &str, path: &str, data: &str) -> Value {
        let body = self.call(method, path, data).await;
        assert_eq!(body["ErrorNumber"], 0, "{path}: {body}");
        body["Value"].clone()
    }
}
async fn request(router: &Router, method: &str, path: &str, data: &str) -> (StatusCode, Value) {
    let uri = if method == "GET" && !data.is_empty() {
        format!("{path}?{data}")
    } else {
        path.into()
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(Body::from(
            if method == "GET" { "" } else { data }.to_owned(),
        ))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn eventually(mut read: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !read().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn setup(
    router: &Router,
    command: Value,
    content_type: &str,
    origin: &str,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/setup/api/hub")
                .header("Content-Type", content_type)
                .header("Host", "127.0.0.1:11111")
                .header("Origin", origin)
                .body(Body::from(serde_json::to_vec(&command).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status != StatusCode::FORBIDDEN {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn setup_routes_share_metadata_validate_and_apply_with_revision_and_connection_guards() {
    let f = Fixture::new().await;
    let invoke = async |command| {
        setup(
            &f.router,
            command,
            "application/json",
            "http://127.0.0.1:11111",
        )
        .await
    };
    let (status, metadata) = invoke(json!({"op":"describeConfig"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metadata["result"]["contractVersion"], 1);
    assert_eq!(
        metadata["result"]["schema"]["$defs"]["SafetyMember"]["properties"]["source"]["x-regain"]["reference"],
        "source"
    );
    let (_, original) = invoke(json!({"op":"getConfig"})).await;
    let mut candidate = original["result"].clone();
    let revision = candidate["revision"].clone();
    candidate["outputs"][0]["label"] = json!("Updated hub switches");
    let (_, validated) = invoke(json!({"op":"validateConfig","candidate":candidate})).await;
    assert_eq!(validated["result"]["valid"], true);
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=true",
    )
    .await;
    let (status, _) =
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=false",
    )
    .await;
    eventually(async || f.hub.active_connections() == 0).await;
    let (status, applied) =
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["result"]["ready"], true);
    assert_ne!(applied["result"]["configurationRevision"], revision);
    assert!(
        f.ok("GET", "/api/v1/switch/7/name", "")
            .await
            .as_str()
            .unwrap()
            .contains("Updated hub switches")
    );
    assert_eq!(
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let persisted: HubConfig =
        serde_json::from_slice(&std::fs::read(f._dir.path().join("hub.json")).unwrap()).unwrap();
    assert_eq!(persisted.outputs[0].label, "Updated hub switches");
    assert_eq!(persisted.outputs[0].id, f.config.outputs[0].id);
    candidate["outputs"][0]["label"] = json!("");
    let (_, invalid) = invoke(json!({"op":"validateConfig","candidate":candidate})).await;
    assert_eq!(invalid["result"]["valid"], false);
    assert!(!invalid["result"]["errors"].as_array().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn setup_rejects_cross_origin_wrong_media_type_and_device_commands_without_side_effects() {
    let f = Fixture::new().await;
    for (media, origin) in [
        ("application/json", "http://untrusted.example"),
        ("text/plain", "http://127.0.0.1:11111"),
        ("application/jsonp", "http://127.0.0.1:11111"),
    ] {
        assert_eq!(
            setup(
                &f.router,
                json!({"op":"inspectSource","source":f.config.sources[0].id,"start":0,"limit":8}),
                media,
                origin
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        setup(
            &f.router,
            json!({"op":"connect","output":f.config.outputs[0].id}),
            "application/json",
            "http://127.0.0.1:11111"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.hub.active_connections(), 0);
    assert_eq!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count,
        0
    );
    let (status, inspected) = setup(
        &f.router,
        json!({"op":"inspectSource","source":f.config.sources[0].id,"start":0,"limit":8}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    eventually(async || f.hub.active_connections() == 0).await;
    for path in [
        "/setup/hub",
        "/hub.mjs",
        "/hub-config.mjs",
        "/hub-form.mjs",
        "/hub.css",
        "/setup/v1/switch/7/setup",
        "/setup/v1/safetymonitor/3/setup",
        "/setup/v1/observingconditions/12/setup",
    ] {
        assert_eq!(request(&f.router, "GET", path, "").await.0, StatusCode::OK);
    }
    for path in [
        "/setup/v1/switch/0/setup",
        "/setup/v1/safetymonitor/7/setup",
        "/setup/v1/unsupported/7/setup",
    ] {
        assert_eq!(
            request(&f.router, "GET", path, "").await.0,
            StatusCode::NOT_FOUND
        );
    }
    f.finish().await;
}

#[tokio::test]
async fn dynamic_discovery_scalar_mapping_and_independent_client_leases() {
    let f = Fixture::new().await;
    let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
    assert_eq!(devices.as_array().unwrap().len(), 3);
    for (actual, expected) in devices.as_array().unwrap().iter().zip(&f.config.outputs) {
        assert_eq!(actual["UniqueID"], json!(expected.id));
        assert_eq!(actual["DeviceNumber"], expected.number);
        assert!(
            actual["DeviceName"]
                .as_str()
                .unwrap()
                .contains("Simulation")
        );
    }
    assert_eq!(
        request(&f.router, "GET", "/api/v1/switch/0/name", "")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/interfaceversion", "").await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=10")
            .await,
        false
    );
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=10&Id=0")
            .await["ErrorNumber"],
        0x407
    );
    for id in [10, 20] {
        f.ok(
            "PUT",
            "/api/v1/switch/7/connected",
            &format!("ClientID={id}&Connected=true"),
        )
        .await;
    }
    assert_eq!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count,
        2
    );
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=10&Id=0")
            .await["ErrorNumber"]
            == 0
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/maxswitch", "ClientID=10")
            .await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/canwrite", "ClientID=10&Id=2")
            .await,
        false
    );
    let response = f
        .call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=10&ClientTransactionID=4294967295&Id=1&Value=37",
        )
        .await;
    assert_eq!(response["ErrorNumber"], 0);
    assert_eq!(response["ClientTransactionID"], u32::MAX);
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=20&Id=1")
            .await["Value"]
            == 37.0
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=10")
            .await,
        false
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=20")
            .await,
        true
    );
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 1
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=20&Connected=false",
    )
    .await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn device_state_preserves_slots_and_omits_failed_stale_or_unconfigured_values() {
    use regain_hub::config::{ConfigStore, WeatherMetric};
    let config: HubConfig = serde_json::from_str(include_str!(
        "../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    let store = ConfigStore::new(None, config).unwrap();
    let original = store.snapshot();
    let mut edited = original.clone();
    if let VirtualDevice::Switch { channels } = &mut edited.outputs[0].device {
        channels.remove(0);
    }
    if let VirtualDevice::Weather { measurements } = &mut edited.outputs[2].device {
        let mut metric = measurements[&WeatherMetric::Temperature].clone();
        if let regain_hub::config::Readout::Property { property, .. } = &mut metric.sources[0] {
            *property = "starfwhm".into();
        }
        measurements.insert(WeatherMetric::StarFwhm, metric);
    }
    store.apply(original.revision, edited, false).unwrap();
    let f = Fixture::from_config(store.snapshot()).await;
    for kind in ["switch", "safetymonitor", "observingconditions"] {
        assert_eq!(
            f.call(
                "GET",
                &format!("/api/v1/{kind}/0/devicestate"),
                "ClientID=7"
            )
            .await["ErrorNumber"],
            0x407
        );
        f.ok(
            "PUT",
            &format!("/api/v1/{kind}/0/connected"),
            "ClientID=7&Connected=true",
        )
        .await;
    }
    eventually(async || {
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await
            .as_array()
            .unwrap()
            .len()
            == 4
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/0/maxswitch", "ClientID=7")
            .await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await,
        json!([
            {"Name":"GetSwitch1","Value":false}, {"Name":"GetSwitchValue1","Value":0.0},
            {"Name":"GetSwitch2","Value":true}, {"Name":"GetSwitchValue2","Value":12.0}
        ])
    );
    eventually(async || {
        f.ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await
        .as_array()
        .unwrap()
        .len()
            == 3
    })
    .await;
    let weather = f
        .ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await;
    assert!(
        weather
            .as_array()
            .unwrap()
            .contains(&json!({"Name":"StarFWHM","Value":2.0}))
    );
    f.hub
        .update_simulation(
            f.config.sources[2].id,
            SimulationUpdate {
                weather: std::collections::BTreeMap::from([(WeatherMetric::Pressure, None)]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        let weather = f
            .ok(
                "GET",
                "/api/v1/observingconditions/0/devicestate",
                "ClientID=7",
            )
            .await;
        weather.as_array().unwrap().len() == 2
            && weather
                .as_array()
                .unwrap()
                .iter()
                .all(|state| state["Name"] != "Pressure")
    })
    .await;
    f.hub
        .update_simulation(
            f.config.sources[2].id,
            SimulationUpdate {
                sample_age_seconds: Some(120.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await
            == json!([])
    })
    .await;
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                sample_age_seconds: Some(120.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await
            == json!([])
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/0/devicestate", "ClientID=7")
            .await,
        json!([
            {"Name":"IsSafe","Value":false}
        ])
    );
    f.finish().await;
}

#[tokio::test]
async fn weather_and_safety_use_host_evidence_and_never_http_cached_permission() {
    let f = Fixture::new().await;
    for path in [
        "/api/v1/safetymonitor/3/connected",
        "/api/v1/observingconditions/12/connected",
    ] {
        f.ok("PUT", path, "ClientID=1&Connected=true").await;
    }
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await,
        false
    );
    f.hub
        .update_simulation(
            f.config.sources[1].id,
            SimulationUpdate {
                safe: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await
            == true
    })
    .await;
    eventually(async || {
        f.call(
            "GET",
            "/api/v1/observingconditions/12/temperature",
            "ClientID=1",
        )
        .await["ErrorNumber"]
            == 0
    })
    .await;
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/temperature",
            "ClientID=1"
        )
        .await
        .is_number()
    );
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/sensordescription",
            "ClientID=1&SensorName=Temperature"
        )
        .await
        .as_str()
        .unwrap()
        .contains("temperature")
    );
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/timesincelastupdate",
            "ClientID=1&SensorName=temperature"
        )
        .await
        .as_f64()
        .unwrap()
            >= 0.0
    );
    assert_eq!(
        f.call(
            "GET",
            "/api/v1/observingconditions/12/humidity",
            "ClientID=1"
        )
        .await["ErrorNumber"],
        0x400
    );
    f.ok(
        "PUT",
        "/api/v1/observingconditions/12/averageperiod",
        "ClientID=1&AveragePeriod=0.01",
    )
    .await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/averageperiod",
            "ClientID=1"
        )
        .await,
        0.01
    );
    f.ok(
        "PUT",
        "/api/v1/observingconditions/12/refresh",
        "ClientID=1",
    )
    .await;
    f.hub
        .update_simulation(
            f.config.sources[1].id,
            SimulationUpdate {
                safe: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await
            == false
    })
    .await;
    f.stop.cancel();
    eventually(async || {
        f.call("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await["ErrorNumber"]
            != 0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn invalid_requests_are_rejected_without_mutation_and_shutdown_releases_only_http_clients() {
    let f = Fixture::new().await;
    let native = f.hub.client();
    native.connect(f.config.outputs[0].id).await.unwrap();
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=42&Connected=true",
    )
    .await;
    for data in [
        "ClientID=42&Id=-1&Value=1",
        "ClientID=42&Id=1&Value=NaN",
        "ClientID=42&Id=1&Value=1&id=2",
        "ClientID=42&Id=1&Value=101",
    ] {
        assert_eq!(
            f.call("PUT", "/api/v1/switch/7/setswitchvalue", data).await["ErrorNumber"],
            0x401
        );
    }
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setswitchname",
            "ClientID=42&Id=1&Name=changed"
        )
        .await["ErrorNumber"],
        0x400
    );
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/issafe", "ClientID=42")
            .await["ErrorNumber"],
        0x400
    );
    f.server.shutdown().await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 1
    })
    .await;
    assert!(native.connection(f.config.outputs[0].id).is_ok());
    native.close();
    f.finish().await;
}

#[tokio::test]
async fn bounded_http_clients_reuse_capacity_and_uncertain_writes_are_not_replayed() {
    let f = Fixture::new().await;
    for id in 1..=24 {
        f.ok(
            "PUT",
            "/api/v1/switch/7/connected",
            &format!("ClientID={id}&Connected=true"),
        )
        .await;
    }
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/connected",
            "ClientID=25&Connected=true"
        )
        .await["ErrorNumber"],
        0x40b
    );
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=25&Connected=true",
    )
    .await;
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                fault: Some(regain_hub::simulated::Fault::UncertainWrite),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let result = f
        .call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=25&Id=1&Value=33",
        )
        .await;
    assert_eq!(result["ErrorNumber"], 0x500);
    assert!(
        result["ErrorMessage"]
            .as_str()
            .unwrap()
            .contains("uncertain")
    );
    assert!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .write_uncertain
    );
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                fault: Some(regain_hub::simulated::Fault::None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=2&Id=1&Value=44"
        )
        .await["ErrorNumber"],
        0x500
    );
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=2&Id=1")
            .await["Value"]
            == 33.0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn slow_upstream_connection_does_not_block_other_outputs_or_claim_valid_readings() {
    use axum::{Json, extract::Request as AxumRequest, routing::any};
    use regain_hub::config::{ConnectionPolicy, DeviceType, SourceBackend};
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new().fallback(any({
        let entered = entered.clone(); let release = release.clone();
        move |request: AxumRequest| {
            let entered = entered.clone(); let release = release.clone();
            async move {
                let path = request.uri().path().to_owned();
                let query = request.uri().query().unwrap_or("").to_owned();
                let put = request.method() == "PUT";
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let params = regain_alpaca::device::Params::parse(if put {
                    std::str::from_utf8(&bytes).unwrap()
                } else { &query }).unwrap();
                let transaction = params.optional_id("ClientTransactionID").unwrap();
                let value = if path.ends_with("/interfaceversion") { json!(2) }
                    else if put && path.ends_with("/connected") {
                        if String::from_utf8_lossy(&bytes).contains("Connected=true") {
                            entered.notify_one(); release.notified().await;
                        }
                        Value::Null
                    } else if path.ends_with("/connected") { json!(false) }
                    else { json!(0) };
                Json(json!({"Value":value,"ErrorNumber":0,"ErrorMessage":"","ClientTransactionID":transaction,"ServerTransactionID":1}))
            }
        }
    }));
    let serving = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let mut config: HubConfig = serde_json::from_str(include_str!(
        "../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    config.sources[0].backend = SourceBackend::Alpaca {
        base_url: format!("http://{address}"),
        device_type: DeviceType::Switch,
        device_number: 0,
        connection_policy: ConnectionPolicy::Managed,
        credential_reference: None,
    };
    let f = Fixture::from_config(config).await;
    let connecting = tokio::spawn({
        let router = f.router.clone();
        async move {
            request(
                &router,
                "PUT",
                "/api/v1/switch/0/connected",
                "ClientID=9&Connected=true",
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    // Connected is a lease on the virtual hub; it does not assert every upstream
    // device is online. Unavailable source reads must remain errors.
    assert_eq!(connecting.await.unwrap().1["ErrorNumber"], 0);
    assert_eq!(
        f.call("GET", "/api/v1/switch/0/getswitchvalue", "ClientID=9&Id=1")
            .await["ErrorNumber"],
        0x407
    );
    f.ok(
        "PUT",
        "/api/v1/safetymonitor/0/connected",
        "ClientID=10&Connected=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/0/issafe", "ClientID=10")
            .await,
        false
    );
    release.notify_one();
    eventually(async || {
        f.call("GET", "/api/v1/switch/0/connected", "ClientID=9")
            .await["Value"]
            == true
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/0/connected",
        "ClientID=9&Connected=false",
    )
    .await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 0
    })
    .await;
    f.finish().await;
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn modern_connection_methods_and_switch_async_contract_preserve_client_ownership() {
    let f = Fixture::new().await;
    for (kind, number, version) in [
        ("switch", 7, 3),
        ("safetymonitor", 3, 3),
        ("observingconditions", 12, 2),
    ] {
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/interfaceversion"),
                ""
            )
            .await,
            version
        );
        assert_eq!(
            f.ok("GET", &format!("/api/v1/{kind}/{number}/driverversion"), "")
                .await,
            "0.6"
        );
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/connecting"),
                "ClientID=1"
            )
            .await,
            false
        );
        f.ok(
            "PUT",
            &format!("/api/v1/{kind}/{number}/connect"),
            "ClientID=1",
        )
        .await;
        eventually(async || {
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/connecting"),
                "ClientID=1",
            )
            .await
                == false
                && f.ok(
                    "GET",
                    &format!("/api/v1/{kind}/{number}/connected"),
                    "ClientID=1",
                )
                .await
                    == true
        })
        .await;
    }
    f.ok("PUT", "/api/v1/switch/7/connect", "ClientID=2").await;
    eventually(async || {
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=2")
            .await
            == true
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/canasync", "ClientID=1&Id=1")
            .await,
        false
    );
    for (method, member, data) in [
        ("GET", "statechangecomplete", "ClientID=1&Id=1"),
        ("PUT", "setasync", "ClientID=1&Id=1&State=true"),
        ("PUT", "setasyncvalue", "ClientID=1&Id=1&Value=30"),
    ] {
        assert_eq!(
            f.call(method, &format!("/api/v1/switch/7/{member}"), data)
                .await["ErrorNumber"],
            0x400
        );
    }
    f.ok("PUT", "/api/v1/switch/7/cancelasync", "ClientID=1&Id=1")
        .await;
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/canasync", "ClientID=1&Id=999")
            .await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setasyncvalue",
            "ClientID=1&Id=1&Value=NaN"
        )
        .await["ErrorNumber"],
        0x401
    );
    f.ok("PUT", "/api/v1/switch/7/disconnect", "ClientID=1")
        .await;
    eventually(async || {
        f.ok("GET", "/api/v1/switch/7/connecting", "ClientID=1")
            .await
            == false
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=1")
            .await,
        false
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/3/connected", "ClientID=1")
            .await,
        true
    );
    f.finish().await;
}

#[tokio::test]
async fn asynchronous_open_failure_is_visible_until_explicit_disconnect_and_does_not_leak_capacity()
{
    use regain_hub::client::{Client, ClientLimits};
    let f = Fixture::new().await;
    let endpoint = Endpoint::for_config(&f._dir.path().join("hub.json")).unwrap();
    // Occupy the remaining host streams without output/source leases. Catalog
    // reads still work, but HTTP's new private session is rejected.
    let mut clients = Vec::new();
    for _ in 1..host::MAX_CLIENTS {
        clients.push(
            Client::connect(
                &endpoint,
                f.config.instance_id,
                Duration::from_secs(5),
                ClientLimits::default(),
            )
            .await
            .unwrap(),
        );
    }
    f.ok("PUT", "/api/v1/switch/7/connect", "ClientID=42").await;
    // The accepted attempt owns a real ten-second attachment deadline. A
    // crowded host leaves it pending until that deadline, rather than rejecting
    // its stream immediately. Poll that specific operation to completion.
    tokio::time::timeout(Duration::from_secs(15), async {
        while f
            .call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await["ErrorNumber"]
            == 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let failure = f
        .call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
        .await;
    assert_ne!(failure["ErrorNumber"], 0);
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=42")
            .await,
        false
    );
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await["ErrorNumber"],
        failure["ErrorNumber"]
    );
    f.ok("PUT", "/api/v1/switch/7/disconnect", "ClientID=42")
        .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await,
        false
    );
    drop(clients);
    for id in 1..=26 {
        f.ok("PUT", "/api/v1/switch/7/connect", &format!("ClientID={id}"))
            .await;
        eventually(async || {
            f.ok(
                "GET",
                "/api/v1/switch/7/connected",
                &format!("ClientID={id}"),
            )
            .await
                == true
                && f.ok(
                    "GET",
                    "/api/v1/switch/7/connecting",
                    &format!("ClientID={id}"),
                )
                .await
                    == false
        })
        .await;
        f.ok(
            "PUT",
            "/api/v1/switch/7/disconnect",
            &format!("ClientID={id}"),
        )
        .await;
        eventually(async || {
            f.ok(
                "GET",
                "/api/v1/switch/7/connecting",
                &format!("ClientID={id}"),
            )
            .await
                == false
        })
        .await;
    }
    eventually(async || f.hub.active_connections() == 0).await;
    f.finish().await;
}
