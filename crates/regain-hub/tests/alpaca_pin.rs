//! Private management/device peer; no hardware or installed driver activation.
use axum::{
    Json, Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::any,
};
use regain_hub::{
    alpaca::AlpacaBackend,
    camera::image::ImageBudget,
    config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
    parameters::PollPolicy,
    source::{Backend, ErrorKind, Values},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

struct Peer {
    fixture: Arc<Fixture>,
    config: SourceConfig,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Fixture {
    catalog: Mutex<Value>,
    requests: Mutex<Vec<(String, String, bool)>>,
    status: Mutex<u16>,
    delay: Mutex<Duration>,
}
fn catalog(id: &str) -> Value {
    json!([{"DeviceName":"[SIMULATION] pinned camera", "DeviceType":"Camera", "DeviceNumber":7, "UniqueID":id}])
}
impl Peer {
    async fn open(id: &str) -> Self {
        let fixture = Arc::new(Fixture {
            catalog: Mutex::new(catalog(id)),
            requests: Mutex::new(vec![]),
            status: Mutex::new(200),
            delay: Mutex::new(Duration::ZERO),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/prefix/", listener.local_addr().unwrap());
        let router = Router::new()
            .fallback(any(reply))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            fixture,
            task,
            config: SourceConfig {
                id: Uuid::new_v4(),
                label: "[SIMULATION] pinned source".into(),
                polling: PollPolicy {
                    request_timeout_seconds: 0.2,
                    connection_timeout_seconds: 2.0,
                    ..Default::default()
                },
                backend: SourceBackend::Alpaca {
                    scope_id: None,
                    base_url: url,
                    device_type: DeviceType::Camera,
                    device_number: 7,
                    unique_id: Some(id.into()),
                    connection_policy: ConnectionPolicy::Managed,
                    credential_reference: Some("private-reference".into()),
                },
            },
        }
    }
    fn backend(&self) -> AlpacaBackend {
        AlpacaBackend::new(
            &self.config,
            vec![],
            Some("Bearer fixture-secret".parse().unwrap()),
        )
        .unwrap()
    }
    fn device_requests(&self) -> usize {
        self.fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path, _)| !path.contains("/management/"))
            .count()
    }
    async fn connect(&self, backend: &mut AlpacaBackend) {
        while !backend.connect_step().await.unwrap() {}
    }
}
async fn reply(State(f): State<Arc<Fixture>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    let authenticated = request
        .headers()
        .get("authorization")
        .is_some_and(|h| h == "Bearer fixture-secret");
    f.requests
        .lock()
        .unwrap()
        .push((method.clone(), path.clone(), authenticated));
    if path == "/prefix/management/v1/configureddevices" {
        let delay = *f.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        let status = axum::http::StatusCode::from_u16(*f.status.lock().unwrap()).unwrap();
        return (
            status,
            Json(json!({"ErrorNumber":0,"Value":f.catalog.lock().unwrap().clone()})),
        )
            .into_response();
    }
    let value = if path.ends_with("/interfaceversion") {
        json!(2)
    } else if path.ends_with("/connected") {
        if method == "GET" {
            json!(false)
        } else {
            Value::Null
        }
    } else if path.ends_with("/imagearray") {
        json!([[42]])
    } else {
        json!(true)
    };
    Json(json!({"ErrorNumber":0,"Value":value,"Type":2,"Rank":2})).into_response()
}

#[tokio::test]
async fn matching_pin_guards_connect_reads_writes_images_and_cleanup_with_the_same_credentials() {
    let peer = Peer::open("camera unit 42").await;
    let mut backend = peer.backend();
    peer.connect(&mut backend).await;
    assert_eq!(
        backend
            .read("imageready".into(), Values::new())
            .await
            .unwrap(),
        true
    );
    backend
        .write("startexposure".into(), Values::new())
        .await
        .unwrap();
    let image = backend
        .camera_image(ImageBudget::new(1024).unwrap())
        .await
        .unwrap();
    assert_eq!(image.bytes(), 42i32.to_le_bytes());
    backend.disconnect().await.unwrap();
    assert!(!backend.connection_info().unwrap().owns_connection);
    let requests = peer.fixture.requests.lock().unwrap();
    assert!(requests.len() >= 12 && requests.len() % 2 == 0);
    for pair in requests.chunks_exact(2) {
        assert_eq!(pair[0].0, "GET");
        assert_eq!(pair[0].1, "/prefix/management/v1/configureddevices");
        assert!(!pair[1].1.contains("/management/"));
        assert!(pair[0].2 && pair[1].2);
    }
}

#[tokio::test]
async fn missing_moved_replaced_ambiguous_and_malformed_pins_reject_before_any_device_request() {
    for fault in [
        "missing",
        "replaced",
        "number",
        "type",
        "duplicate",
        "malformed",
        "unavailable",
    ] {
        let peer = Peer::open("camera unit 42").await;
        let mut wrong = catalog("camera unit 42");
        if fault == "missing" {
            wrong = json!([]);
        }
        if fault == "replaced" {
            wrong[0]["UniqueID"] = json!("different equipment");
        }
        if fault == "number" {
            wrong[0]["DeviceNumber"] = json!(8);
        }
        if fault == "type" {
            wrong[0]["DeviceType"] = json!("Focuser");
        }
        if fault == "duplicate" {
            let device = wrong[0].clone();
            wrong.as_array_mut().unwrap().push(device);
        }
        if fault == "malformed" {
            wrong[0].as_object_mut().unwrap().remove("UniqueID");
        }
        if fault == "unavailable" {
            *peer.fixture.status.lock().unwrap() = 503;
        }
        *peer.fixture.catalog.lock().unwrap() = wrong;
        let error = peer.backend().connect_step().await.unwrap_err();
        assert_eq!(
            error.kind,
            if fault == "unavailable" {
                ErrorKind::Transient
            } else {
                ErrorKind::Permanent
            },
            "{fault}"
        );
        assert_eq!(peer.device_requests(), 0, "{fault}");
    }
}

#[tokio::test]
async fn replacement_during_a_session_blocks_reads_writes_images_and_disconnect_of_the_replacement()
{
    let peer = Peer::open("camera unit 42").await;
    let mut backend = peer.backend();
    peer.connect(&mut backend).await;
    let before = peer.device_requests();
    *peer.fixture.catalog.lock().unwrap() = catalog("replacement");
    assert_eq!(
        backend
            .read("imageready".into(), Values::new())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Permanent
    );
    assert_eq!(
        backend
            .write("startexposure".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Permanent
    );
    assert_eq!(
        backend
            .camera_image(ImageBudget::new(1024).unwrap())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Permanent
    );
    assert_eq!(
        backend.disconnect().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert_eq!(peer.device_requests(), before);
    let info = backend.connection_info().unwrap();
    assert!(info.owns_connection && info.uncertain);
    backend.reset();
    assert_eq!(
        backend.connect_step().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(peer.device_requests(), before);
}

#[tokio::test]
async fn replacement_between_metadata_and_open_cannot_receive_a_connect_write() {
    let peer = Peer::open("camera unit 42").await;
    let mut backend = peer.backend();
    assert!(!backend.connect_step().await.unwrap()); // interface version
    assert!(!backend.connect_step().await.unwrap()); // disconnected
    *peer.fixture.catalog.lock().unwrap() = catalog("replacement");
    assert_eq!(
        backend.connect_step().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert_eq!(peer.device_requests(), 2);
    assert!(
        peer.fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    assert!(!backend.connection_info().unwrap().owns_connection);
}

#[tokio::test]
async fn uuid_forms_match_but_plain_string_ids_remain_case_sensitive() {
    let id = Uuid::new_v4();
    for (identity, matches) in [
        (id.simple().to_string().to_uppercase(), true),
        (format!("{{{id}}}"), true),
        (format!("urn:uuid:{id}"), true),
        (format!("{id} "), false),
        (format!("urn:uuid:{}", id.simple()), false),
        (format!("{{{}}}", id.simple()), false),
        (
            "{0x00112233,0x4455,0x6677,{0x88,0x99,0xaa,0xbb,0xcc,0xdd,0xee,0xff}}".into(),
            false,
        ),
    ] {
        let peer = Peer::open(&id.to_string()).await;
        *peer.fixture.catalog.lock().unwrap() = catalog(&identity);
        if matches {
            peer.connect(&mut peer.backend()).await;
        } else {
            assert_eq!(
                peer.backend().connect_step().await.unwrap_err().kind,
                ErrorKind::Permanent,
                "{identity}"
            );
            assert_eq!(peer.device_requests(), 0);
        }
    }
    let peer = Peer::open("camera unit 42").await;
    *peer.fixture.catalog.lock().unwrap() = catalog("Camera unit 42");
    assert_eq!(
        peer.backend().connect_step().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert_eq!(peer.device_requests(), 0);
}

#[tokio::test]
async fn stalled_identity_check_is_bounded_by_the_source_timeout_and_sends_no_device_request() {
    let peer = Peer::open("camera unit 42").await;
    *peer.fixture.delay.lock().unwrap() = Duration::from_secs(2);
    let error = tokio::time::timeout(Duration::from_secs(1), peer.backend().connect_step())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(peer.device_requests(), 0);
    assert_eq!(peer.fixture.requests.lock().unwrap().len(), 1);
}
