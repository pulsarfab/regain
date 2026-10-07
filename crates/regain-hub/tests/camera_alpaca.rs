//! Private loopback camera; never discovers hardware or starts vendor drivers.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
    routing::any,
};
use futures_util::StreamExt;
use regain_hub::{
    activity::ActivityCounter,
    alpaca::AlpacaBackend,
    camera::{
        acquisition::{
            AcquisitionPhase, AcquisitionTiming, CameraSupervisor, ExposureRequest, GuideRequest,
        },
        image::{ElementType, ImageBudget, ImageDescriptor, ImageOrder},
    },
    config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{Backend, ErrorKind, SourceHandle},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
struct Reply {
    body: Vec<u8>,
    content_type: &'static str,
    status: u16,
    echo_transaction: bool,
    delay: Duration,
}
impl Reply {
    fn image() -> Self {
        let descriptor = ImageDescriptor::new(
            3,
            2,
            None,
            ElementType::Int32,
            ElementType::UInt16,
            ImageOrder::Ascom,
        )
        .unwrap();
        let mut body = descriptor.imagebytes_header(0, 9).to_vec();
        for value in [0u16, 65535, 50000, 32768, 42, 4096] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        Self {
            body,
            content_type: "application/imagebytes; charset=binary",
            status: 200,
            echo_transaction: true,
            delay: Duration::ZERO,
        }
    }
    fn json(value: Value) -> Self {
        Self {
            body: serde_json::to_vec(&value).unwrap(),
            content_type: "application/json",
            echo_transaction: false,
            ..Self::image()
        }
    }
}
#[derive(Default)]
struct StateData {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<(String, String, String, String)>>,
    started: Mutex<bool>,
    guiding: Mutex<bool>,
    guide_parameters: Mutex<Vec<regain_hub::source::Values>>,
}
struct Server {
    data: Arc<StateData>,
    config: SourceConfig,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        let data = Arc::new(StateData::default());
        data.replies.lock().unwrap().extend(replies);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(any(handler))
            .with_state(data.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = SourceConfig {
            id: Uuid::new_v4(),
            label: "Private camera".into(),
            polling: PollPolicy::default(),
            backend: SourceBackend::Alpaca {
                unique_id: None,
                base_url: format!("http://{address}/prefix/"),
                device_type: DeviceType::Camera,
                device_number: 19,
                connection_policy: ConnectionPolicy::ExternallyManaged,
                credential_reference: None,
            },
        };
        Self { data, config, task }
    }
    fn backend(&self) -> AlpacaBackend {
        AlpacaBackend::new(&self.config, vec![], None).unwrap()
    }
    fn image_requests(&self) -> usize {
        self.data
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.1.split('?').next().unwrap().ends_with("/imagearray"))
            .count()
    }
}
async fn handler(State(data): State<Arc<StateData>>, request: Request) -> Response {
    let uri = request.uri().to_string();
    let url = url::Url::parse(&format!("http://fixture{uri}")).unwrap();
    let mut transaction = url
        .query_pairs()
        .find(|(key, _)| key == "ClientTransactionID")
        .map(|(_, value)| value.parse::<u32>().unwrap())
        .unwrap_or(0);
    let member = url.path_segments().unwrap().next_back().unwrap();
    data.requests.lock().unwrap().push((
        request.method().to_string(),
        uri.clone(),
        request
            .headers()
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .into(),
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .into(),
    ));
    let body = to_bytes(request.into_body(), 65536).await.unwrap();
    if !body.is_empty() {
        transaction = url::form_urlencoded::parse(&body)
            .find(|(key, _)| key == "ClientTransactionID")
            .map(|(_, value)| value.parse::<u32>().unwrap())
            .unwrap();
    }
    if member != "imagearray" {
        let value = match member {
            "interfaceversion" => json!(3),
            "connected" => json!(true),
            "camerastate" => json!(0),
            "imageready" => json!(*data.started.lock().unwrap()),
            "exposuremin" => json!(0.001),
            "exposuremax" => json!(600.0),
            "numx" | "cameraxsize" => json!(3),
            "numy" | "cameraysize" => json!(2),
            "binx" | "biny" | "maxbinx" | "maxbiny" => json!(1),
            "startx" | "starty" => json!(0),
            "canasymmetricbin" => json!(false),
            "canpulseguide" => json!(true),
            "ispulseguiding" => json!(*data.guiding.lock().unwrap()),
            "pulseguide" => {
                let args: std::collections::BTreeMap<_, _> =
                    url::form_urlencoded::parse(&body).into_owned().collect();
                let direction = args["Direction"].parse::<i32>().unwrap();
                let duration = args["Duration"].parse::<i32>().unwrap();
                assert!((0..4).contains(&direction) && duration >= 0);
                data.guide_parameters
                    .lock()
                    .unwrap()
                    .push(regain_hub::source::Values::from([
                        ("Direction".into(), json!(direction)),
                        ("Duration".into(), json!(duration)),
                    ]));
                *data.guiding.lock().unwrap() = duration != 0;
                Value::Null
            }
            "lastexposureduration" => json!(0.01),
            "lastexposurestarttime" => json!("2026-10-07T00:00:01.125"),
            "startexposure" => {
                *data.started.lock().unwrap() = true;
                Value::Null
            }
            _ => Value::Null,
        };
        return Response::builder().header("Content-Type", "application/json").body(Body::from(serde_json::to_vec(&json!({"Value":value,"ErrorNumber":0,"ClientTransactionID":transaction,"ServerTransactionID":1})).unwrap())).unwrap();
    }
    let mut reply = data
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("Unexpected repeated download");
    if reply.echo_transaction {
        reply.body[8..12].copy_from_slice(&transaction.to_le_bytes());
    }
    let split = reply.body.len().min(44);
    let tail = reply.body.split_off(split);
    let parts = [(Duration::ZERO, reply.body), (reply.delay, tail)];
    let stream = futures_util::stream::iter(parts).then(|(delay, part)| async move {
        tokio::time::sleep(delay).await;
        Ok::<_, Infallible>(part)
    });
    Response::builder()
        .status(reply.status)
        .header("Content-Type", reply.content_type)
        .header("Retry-After", "2")
        .header("Location", "/redirected")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::test]
async fn alpaca_guide_preserves_integer_parameters_and_exposure_ownership_without_replay() {
    let server = Server::new(vec![Reply::image()]).await;
    let source = SourceHandle::spawn(
        server.config.id,
        Uuid::new_v4(),
        PollPolicy::default(),
        Box::new(server.backend()),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let activity = ActivityCounter::default();
    let supervisor = CameraSupervisor::new(
        source.clone(),
        ImageBudget::new(12).unwrap(),
        AcquisitionTiming::default(),
        activity.clone(),
    )
    .unwrap();
    let owner = supervisor.connect().await.unwrap();
    let observer = supervisor.connect().await.unwrap();
    owner
        .pulse_guide(GuideRequest {
            direction: 3,
            duration_milliseconds: 0,
        })
        .await
        .unwrap();
    assert!(owner.status().guiding.is_none());
    owner
        .pulse_guide(GuideRequest {
            direction: 2,
            duration_milliseconds: i32::MAX,
        })
        .await
        .unwrap();
    owner
        .start(ExposureRequest {
            duration_seconds: 0.01,
            light: true,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !observer.status().image_ready {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(observer.status().guiding.is_some());
    assert_eq!(
        observer
            .pulse_guide(GuideRequest {
                direction: 0,
                duration_milliseconds: 0
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    drop(owner);
    *server.data.guiding.lock().unwrap() = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while observer.status().guiding.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(activity.active(), 0);
    assert_eq!(
        *server.data.guide_parameters.lock().unwrap(),
        vec![
            regain_hub::source::Values::from([
                ("Direction".into(), json!(3)),
                ("Duration".into(), json!(0))
            ]),
            regain_hub::source::Values::from([
                ("Direction".into(), json!(2)),
                ("Duration".into(), json!(i32::MAX))
            ]),
        ]
    );
    assert_eq!(server.image_requests(), 1);
    assert!(
        !server
            .data
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.0 == "PUT"
                && (request.1.ends_with("/abortexposure") || request.1.ends_with("/stopexposure")))
    );
    drop(observer);
    source.shutdown().await.unwrap();
}

#[tokio::test]
async fn negotiated_image_stream_uses_separate_deadline_credentials_and_transactions() {
    let mut reply = Reply::image();
    reply.delay = Duration::from_millis(1200);
    let mut server = Server::new(vec![reply]).await;
    if let SourceBackend::Alpaca {
        credential_reference,
        ..
    } = &mut server.config.backend
    {
        *credential_reference = Some("private-test".into());
    }
    let mut backend = AlpacaBackend::new(
        &server.config,
        vec![],
        Some(reqwest::header::HeaderValue::from_static(
            "Bearer private-camera",
        )),
    )
    .unwrap();
    let budget = ImageBudget::new(12).unwrap();
    let image = tokio::time::timeout(Duration::from_secs(5), backend.camera_image(budget.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(image.descriptor().width(), 3);
    assert_eq!(image.descriptor().height(), 2);
    assert_eq!(
        image
            .bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_le_bytes(*v))
            .collect::<Vec<_>>(),
        vec![0, 65535, 50000, 32768, 42, 4096]
    );
    assert_eq!(budget.used_bytes(), 12);
    let requests = server.data.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "GET");
    assert!(
        requests[0]
            .1
            .starts_with("/prefix/api/v1/camera/19/imagearray?")
    );
    assert_eq!(requests[0].2, "application/imagebytes");
    assert_eq!(requests[0].3, "Bearer private-camera");
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn malformed_or_replaced_http_images_never_publish_or_retry() {
    let base = Reply::image();
    let mut wrong_transaction = base.clone();
    wrong_transaction.echo_transaction = false;
    let mut bad_header = base.clone();
    bad_header.body[0..4].copy_from_slice(&2u32.to_le_bytes());
    let mut truncated = base.clone();
    truncated.body.pop();
    let mut trailing = base.clone();
    trailing.body.push(1);
    let mut wrong_content = base.clone();
    wrong_content.content_type = "text/html";
    for reply in [
        wrong_transaction,
        bad_header,
        truncated,
        trailing,
        wrong_content,
    ] {
        let server = Server::new(vec![reply]).await;
        let budget = ImageBudget::new(12).unwrap();
        let error = match server.backend().camera_image(budget.clone()).await {
            Err(e) => e,
            Ok(_) => panic!("Invalid image published"),
        };
        assert_eq!(error.kind, ErrorKind::Permanent);
        assert_eq!(budget.used_bytes(), 0);
        assert_eq!(server.image_requests(), 1);
    }
}

#[tokio::test]
async fn binary_json_and_http_errors_keep_codes_but_redact_upstream_text() {
    for code in [0x400u32, 0x401, 0x402, 0x407, 0x501] {
        let mut binary = Reply::image();
        binary.body.truncate(44);
        binary.body[4..8].copy_from_slice(&code.to_le_bytes());
        binary.body.extend_from_slice(b"private upstream error");
        let json = Reply::json(json!({"ErrorNumber":code,"ErrorMessage":"private upstream error"}));
        for reply in [binary, json] {
            let server = Server::new(vec![reply]).await;
            let error = match server
                .backend()
                .camera_image(ImageBudget::new(12).unwrap())
                .await
            {
                Err(e) => e,
                Ok(_) => panic!("Error published"),
            };
            assert_eq!(error.upstream_code, Some(code as i32));
            assert_eq!(error.transport_lost, code == 0x407);
            assert!(!error.to_string().contains("private upstream"));
            assert_eq!(server.image_requests(), 1);
        }
    }
    for status in [302, 503] {
        let server = Server::new(vec![Reply {
            status,
            ..Reply::image()
        }])
        .await;
        let error = match server
            .backend()
            .camera_image(ImageBudget::new(12).unwrap())
            .await
        {
            Err(e) => e,
            Ok(_) => panic!("HTTP error published"),
        };
        assert_eq!(error.upstream_code, Some(i32::from(status)));
        assert_eq!(error.retry_after, Some(Duration::from_secs(2)));
        assert_eq!(
            error.kind,
            if status == 503 {
                ErrorKind::Transient
            } else {
                ErrorKind::Permanent
            }
        );
        assert_eq!(server.data.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancelled_http_copy_releases_partial_allocation_without_another_request() {
    let server = Server::new(vec![Reply {
        delay: Duration::from_secs(10),
        ..Reply::image()
    }])
    .await;
    let budget = ImageBudget::new(12).unwrap();
    let mut backend = server.backend();
    let capture_budget = budget.clone();
    let task = tokio::spawn(async move { backend.camera_image(capture_budget).await });
    // Observe the real reservation instead of assuming a request has arrived
    // after a fixed sleep on a loaded CI host.
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.used_bytes() != 12 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(server.image_requests(), 1);
}

#[tokio::test]
async fn shared_budget_exhaustion_and_non_camera_reads_have_no_fallback_or_retry() {
    let mut server = Server::new(vec![Reply::image()]).await;
    let budget = ImageBudget::new(1).unwrap();
    let error = match server.backend().camera_image(budget.clone()).await {
        Err(e) => e,
        Ok(_) => panic!("Budget bypassed"),
    };
    assert_eq!(error.kind, ErrorKind::Busy);
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(server.image_requests(), 1);
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::Focuser;
    }
    let error = match server.backend().camera_image(budget).await {
        Err(e) => e,
        Ok(_) => panic!("Non-camera image"),
    };
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(server.image_requests(), 1);
}

#[tokio::test]
async fn real_alpaca_source_and_supervisor_share_one_owned_download() {
    for (reply, kind) in [
        (Reply::image(), ElementType::Int32),
        (
            Reply::json(
                json!({"ErrorNumber":0,"Type":3,"Rank":2,"Value":[[-1.25,0.0],[65535.5,3.0],[4.0,5.0]]}),
            ),
            ElementType::Double,
        ),
    ] {
        let server = Server::new(vec![reply]).await;
        let source = SourceHandle::spawn(
            server.config.id,
            Uuid::new_v4(),
            server.config.polling.clone(),
            Box::new(server.backend()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let activity = ActivityCounter::default();
        let supervisor = CameraSupervisor::new(
            source.clone(),
            ImageBudget::new(1024).unwrap(),
            AcquisitionTiming {
                poll_interval: Duration::from_millis(10),
                ..AcquisitionTiming::default()
            },
            activity.clone(),
        )
        .unwrap();
        let owner = supervisor.connect().await.unwrap();
        let observer = supervisor.connect().await.unwrap();
        let id = owner
            .start(ExposureRequest {
                duration_seconds: 0.01,
                light: true,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !supervisor.status().image_ready {
                assert_ne!(
                    supervisor.status().phase,
                    AcquisitionPhase::Uncertain,
                    "{:?}",
                    supervisor.status()
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let a = owner.image().unwrap();
        let b = observer.image().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.identity.acquisition, id);
        assert_eq!(a.image.descriptor().element_type(), kind);
        assert_eq!(server.image_requests(), 1);
        assert_eq!(activity.active(), 0);
        let starts = server
            .data
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.0 == "PUT" && r.1.ends_with("/startexposure"))
            .count();
        assert_eq!(starts, 1);
        drop(owner);
        drop(observer);
        source.shutdown().await.unwrap();
        assert_eq!(a.image.bytes(), b.image.bytes());
    }
}

#[tokio::test]
async fn successful_json_images_use_the_same_download_and_preserve_type_and_order() {
    let server = Server::new(vec![Reply::json(json!({
        "ErrorNumber":0, "Type":2, "Rank":2, "Value":[[1,2],[3,4],[5,6]]
    }))])
    .await;
    let budget = ImageBudget::new(1024).unwrap();
    let image = server.backend().camera_image(budget.clone()).await.unwrap();
    assert_eq!(image.descriptor().element_type(), ElementType::Int32);
    assert_eq!(image.descriptor().order(), ImageOrder::Ascom);
    assert_eq!(
        image
            .bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| i32::from_le_bytes(*v))
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert_eq!(budget.used_bytes(), 24);
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(server.image_requests(), 1);
}
