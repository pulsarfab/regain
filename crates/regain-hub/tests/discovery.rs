use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    response::Response,
    routing::any,
};
use regain_hub::{
    alpaca::discovery::{MAX_DEVICES, discover},
    config::{ConfigStore, DeviceType, HubConfig},
    credentials::{CredentialStore, SecretAuthorization},
    runtime::HubRuntime,
    safety::MonotonicClock,
    service::HubService,
    source::{ErrorKind, SourceRegistry},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<(String, String, bool)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Clone)]
struct Replies {
    status: u16,
    body: String,
    requests: Arc<Mutex<Vec<(String, String, bool)>>>,
    gate: Option<Arc<Semaphore>>,
    entered: Arc<AtomicUsize>,
}
impl Server {
    async fn open(
        status: u16,
        body: String,
        gate: Option<Arc<Semaphore>>,
    ) -> (Self, Arc<AtomicUsize>) {
        let requests = Arc::new(Mutex::new(vec![]));
        let entered = Arc::new(AtomicUsize::new(0));
        let state = Replies {
            status,
            body,
            requests: requests.clone(),
            gate,
            entered: entered.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(any(reply)).with_state(state),
            )
            .await
            .unwrap();
        });
        (
            Self {
                url: format!("http://{address}/prefix/"),
                requests,
                task,
            },
            entered,
        )
    }
}
async fn reply(State(state): State<Replies>, request: Request) -> Response {
    state.requests.lock().unwrap().push((
        request.method().to_string(),
        request.uri().to_string(),
        request
            .headers()
            .get("authorization")
            .is_some_and(|h| h == "Bearer fixture-secret"),
    ));
    state.entered.fetch_add(1, SeqCst);
    if let Some(gate) = state.gate {
        gate.acquire().await.unwrap().forget();
    }
    Response::builder()
        .status(state.status)
        .header("Location", "/redirected")
        .header("Retry-After", "7")
        .body(Body::from_stream(futures_util::stream::iter([Ok::<
            _,
            std::convert::Infallible,
        >(
            state.body,
        )])))
        .unwrap()
}
fn device(kind: &str, number: u32, identity: &str) -> Value {
    json!({"DeviceName":"[SIMULATION] café", "DeviceType":kind, "DeviceNumber":number, "UniqueID":identity})
}
fn body(devices: Value) -> String {
    json!({"Value":devices,"ErrorNumber":0,"ClientTransactionID":1,"ServerTransactionID":9})
        .to_string()
}
fn runtime(config: HubConfig) -> Arc<HubRuntime> {
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |_| {
        unreachable!("Discovery must never create a source")
    })
    .unwrap();
    HubRuntime::from_registry(config, Arc::new(registry), clock).unwrap()
}
async fn wait_count(entered: &AtomicUsize, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while entered.load(SeqCst) != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn explicit_catalog_preserves_string_ids_sparse_numbers_and_unsupported_classes_without_device_calls()
 {
    let (server, _) = Server::open(
        200,
        body(json!([
            device("Camera", u32::MAX, "camera unit 42"),
            device("Telescope", 9, "mount-99")
        ])),
        None,
    )
    .await;
    let revision = Uuid::new_v4();
    let report = discover(
        &server.url,
        Some("Bearer fixture-secret".parse().unwrap()),
        revision,
    )
    .await
    .unwrap();
    assert_eq!(report.configuration_revision, revision);
    assert_eq!(report.base_url, server.url.trim_end_matches('/'));
    assert_eq!(report.devices[0].unique_id, "camera unit 42");
    assert_eq!(report.devices[0].number, u32::MAX);
    assert_eq!(
        report.devices[0].supported_device_type,
        Some(DeviceType::Camera)
    );
    assert_eq!(report.devices[1].supported_device_type, None);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "GET");
    assert!(requests[0].2);
    assert!(
        requests[0]
            .1
            .starts_with("/prefix/management/v1/configureddevices?")
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("fixture-secret")
    );
}
#[tokio::test]
async fn catalog_rejects_ambiguous_or_malformed_identity_fields_and_bounds_counts() {
    let good = device("Focuser", 5, "focus-123");
    let mut cases = vec![json!({}), Value::Null, json!([good.clone(), good.clone()])];
    let mut wrong = good.clone();
    wrong["DeviceNumber"] = json!(-1);
    cases.push(json!([wrong]));
    let mut wrong = good.clone();
    wrong["DeviceNumber"] = json!(4294967296u64);
    cases.push(json!([wrong]));
    let mut wrong = good.clone();
    wrong["UniqueID"] = Value::Null;
    cases.push(json!([wrong]));
    let mut wrong = good.clone();
    wrong["UniqueID"] = json!("\nsecret");
    cases.push(json!([wrong]));
    let mut wrong = good.clone();
    wrong["DeviceName"] = json!("x".repeat(257));
    cases.push(json!([wrong]));
    let mut alias = good.clone();
    alias["DeviceNumber"] = json!(6);
    cases.push(json!([good.clone(), alias]));
    cases.push(json!(
        (0..=MAX_DEVICES)
            .map(|i| device("Camera", i as u32, &format!("id-{i}")))
            .collect::<Vec<_>>()
    ));
    for value in cases {
        let (server, _) = Server::open(200, body(value), None).await;
        assert_eq!(
            discover(&server.url, None, Uuid::new_v4())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Permanent
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
    let duplicate = r#"{"Value":[{"DeviceName":"camera","DeviceType":"Camera","DeviceNumber":0,"UniqueID":"first","UniqueID":"last"}],"ErrorNumber":0}"#;
    let (server, _) = Server::open(200, duplicate.into(), None).await;
    assert_eq!(
        discover(&server.url, None, Uuid::new_v4())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Permanent
    );
}
#[tokio::test]
async fn catalog_never_follows_redirects_retries_errors_or_echoes_upstream_secrets() {
    for (status, payload, expected) in [
        (302, "upstream-secret".into(), ErrorKind::Permanent),
        (503, "upstream-secret".into(), ErrorKind::Transient),
        (
            200,
            "x".repeat(regain_hub::alpaca::MAX_RESPONSE_BYTES + 1),
            ErrorKind::Permanent,
        ),
        (
            200,
            r#"{"ErrorNumber":1024,"ErrorMessage":"upstream-secret"}"#.into(),
            ErrorKind::Unsupported,
        ),
        (
            200,
            r#"{"ErrorNumber":0,"ClientTransactionID":2,"Value":[]}"#.into(),
            ErrorKind::Permanent,
        ),
    ] {
        let (server, _) = Server::open(status, payload, None).await;
        let error = discover(&server.url, None, Uuid::new_v4())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("upstream-secret")
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        if status == 503 {
            assert_eq!(error.retry_after, Some(Duration::from_secs(7)));
        }
    }
    for url in [
        "file:///secret",
        "http://user:secret@localhost/",
        "http://localhost/?secret",
        "http://localhost/#secret",
    ] {
        let error = discover(url, None, Uuid::new_v4()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidValue);
        assert!(!error.message.contains("secret"));
    }
}
#[tokio::test]
async fn discovery_is_bounded_and_revision_fenced_without_blocking_configuration_or_creating_sources()
 {
    let config = HubConfig::empty();
    let revision = config.revision;
    let service = HubService::persistent(
        ConfigStore::new(None, config.clone()).unwrap(),
        Arc::new(|config| Ok(runtime(config))),
    )
    .unwrap();
    let gate = Arc::new(Semaphore::new(0));
    let (server, entered) = Server::open(200, body(json!([])), Some(gate.clone())).await;
    assert_eq!(
        service.search_alpaca(Uuid::nil()).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(
        service
            .discover_alpaca(server.url.clone(), None, Uuid::new_v4())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert!(server.requests.lock().unwrap().is_empty());
    let mut tasks = vec![];
    for _ in 0..4 {
        let service = service.clone();
        let url = server.url.clone();
        tasks.push(tokio::spawn(async move {
            service.discover_alpaca(url, None, revision).await
        }));
    }
    wait_count(&entered, 4).await;
    // UDP and HTTP discovery share admission. This rejects before interface
    // enumeration or any network broadcast; the fixture only uses loopback HTTP.
    assert_eq!(
        service.search_alpaca(revision).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        service
            .discover_alpaca(server.url.clone(), None, revision)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    let updated = service.apply(revision, config).await.unwrap();
    assert_ne!(updated.configuration_revision, revision);
    gate.add_permits(4);
    for task in tasks {
        assert_eq!(
            task.await.unwrap().unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    gate.add_permits(1);
    let catalog = service
        .discover_alpaca(server.url.clone(), None, updated.configuration_revision)
        .await
        .unwrap();
    assert!(catalog.devices.is_empty());
    assert!(service.configuration().sources.is_empty());
    assert!(service.configuration().outputs.is_empty());
    assert_eq!(
        service
            .discover_alpaca(
                server.url.clone(),
                Some("unavailable-reference".into()),
                updated.configuration_revision
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(server.requests.lock().unwrap().len(), 5);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn credential_references_are_resolved_without_exporting_secrets_or_opening_sources() {
    let directory = tempfile::tempdir().unwrap();
    let credentials =
        Arc::new(CredentialStore::at_directory(directory.path(), &"a".repeat(64)).unwrap());
    let reference = credentials
        .create(SecretAuthorization::new("Bearer fixture-secret".into()))
        .unwrap()
        .reference;
    let config = HubConfig::empty();
    let revision = config.revision;
    let service = HubService::persistent_with_credentials(
        ConfigStore::new(None, config).unwrap(),
        Arc::new(|config| Ok(runtime(config))),
        credentials,
    )
    .unwrap();
    let (server, _) = Server::open(200, body(json!([])), None).await;
    let catalog = service
        .discover_alpaca(server.url.clone(), Some(reference), revision)
        .await
        .unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(server.requests.lock().unwrap()[0].2);
    assert!(
        !serde_json::to_string(&catalog)
            .unwrap()
            .contains("fixture-secret")
    );
    let error = service
        .discover_alpaca(
            server.url.clone(),
            Some("invalid-secret-reference".into()),
            revision,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(
        !serde_json::to_string(&error)
            .unwrap()
            .contains("invalid-secret-reference")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stalled_catalog_expires_without_retry_or_a_source_connection() {
    let gate = Arc::new(Semaphore::new(0));
    let (server, _) = Server::open(200, body(json!([])), Some(gate)).await;
    let error = tokio::time::timeout(
        Duration::from_secs(6),
        discover(&server.url, None, Uuid::new_v4()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
