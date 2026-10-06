use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
    routing::any,
};
use regain_hub::{
    alpaca::{AlpacaBackend, MAX_RESPONSE_BYTES, SampleRequest},
    config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
    parameters::PollPolicy,
    source::{Backend, ErrorKind, Values},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use uuid::Uuid;

struct Reply {
    status: u16,
    body: String,
    retry_after: Option<String>,
    delay: Duration,
}
impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            retry_after: None,
            delay: Duration::ZERO,
        }
    }
    fn value(value: Value) -> Self {
        Self::json(json!({"ErrorNumber":0, "Value":value}))
    }
}
#[derive(Default)]
struct Fixture {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<(String, String, String)>>,
}
struct Server {
    fixture: Arc<Fixture>,
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
        let fixture = Arc::new(Fixture::default());
        fixture.replies.lock().unwrap().extend(replies);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(any(handler))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = SourceConfig {
            id: Uuid::new_v4(),
            label: "HTTP fixture".into(),
            polling: PollPolicy::default(),
            backend: SourceBackend::Alpaca {
                base_url: format!("http://{address}/prefix/"),
                device_type: DeviceType::SafetyMonitor,
                device_number: 7,
                connection_policy: ConnectionPolicy::ExternallyManaged,
                credential_reference: None,
            },
        };
        Self {
            fixture,
            config,
            task,
        }
    }
    fn backend(&self) -> AlpacaBackend {
        AlpacaBackend::new(&self.config, vec![SampleRequest::safety()], None).unwrap()
    }
    fn push(&self, reply: Reply) {
        self.fixture.replies.lock().unwrap().push_back(reply);
    }
    fn managed(&mut self) {
        if let SourceBackend::Alpaca {
            connection_policy, ..
        } = &mut self.config.backend
        {
            *connection_policy = ConnectionPolicy::Managed;
        }
    }
}
async fn handler(State(fixture): State<Arc<Fixture>>, request: Request) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let body = to_bytes(request.into_body(), 65536).await.unwrap();
    fixture
        .requests
        .lock()
        .unwrap()
        .push((method, uri, String::from_utf8(body.to_vec()).unwrap()));
    let reply = fixture
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| Reply::value(json!(true)));
    tokio::time::sleep(reply.delay).await;
    let mut response = Response::builder().status(reply.status);
    if let Some(delay) = reply.retry_after {
        response = response.header("Retry-After", delay);
    }
    if reply.status == 302 {
        response = response.header("Location", "/redirected");
    }
    // Use chunked transfer so oversized tests exercise the streaming limit,
    // not just rejection of an advertised Content-Length.
    response
        .body(Body::from_stream(futures_util::stream::iter([Ok::<
            _,
            std::convert::Infallible,
        >(
            reply.body,
        )])))
        .unwrap()
}

#[tokio::test]
async fn external_connection_never_writes_and_form_parameters_preserve_identity() {
    let server = Server::new(vec![
        Reply::value(json!(true)),
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":0})),
    ])
    .await;
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    assert_eq!(backend.poll().await.unwrap()["issafe"], false);
    backend
        .write(
            "action".into(),
            Values::from([("Action".into(), json!("a b&c"))]),
        )
        .await
        .unwrap();
    backend.disconnect().await.unwrap();
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .1
            .starts_with("/prefix/api/v1/safetymonitor/7/connected?")
    );
    assert!(requests[1].1.contains("ClientTransactionID=2"));
    assert_eq!(requests[2].0, "PUT");
    let form = url::form_urlencoded::parse(requests[2].2.as_bytes())
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(form["Action"], "a b&c");
    let query = url::form_urlencoded::parse(requests[0].1.split('?').nth(1).unwrap().as_bytes())
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(form["ClientID"], query["ClientID"]);
}

#[tokio::test]
async fn only_acknowledged_managed_connections_are_disconnected() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":0})),
        Reply::json(json!({"ErrorNumber":0})),
    ])
    .await;
    server.managed();
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    backend.reset();
    backend.disconnect().await.unwrap();
    {
        let requests = server.fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].2.contains("Connected=true"));
        assert!(requests[2].2.contains("Connected=false"));
    }
    // Finding a connected device does not give us ownership of its connection.
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    backend.disconnect().await.unwrap();
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn uncertain_connection_is_not_replayed_or_claimed_for_cleanup() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":null})),
    ])
    .await;
    server.managed();
    let mut backend = server.backend();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    backend.reset();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    backend.disconnect().await.unwrap();
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_envelopes_and_nonboolean_safety_never_become_safe() {
    let server = Server::new(vec![]).await;
    let mut backend = server.backend();
    for value in [
        json!({"Value":"true"}),
        json!({"Value":1}),
        json!({"ErrorNumber":null,"Value":true}),
        json!({"ErrorNumber":"0","Value":true}),
        json!({"ErrorNumber":0.5,"Value":true}),
        json!({"ErrorMessage":false,"Value":true}),
        json!({"ClientTransactionID":0,"Value":true}),
        json!({"ServerTransactionID":-1,"Value":true}),
        json!({"ErrorNumber":0}),
        json!([]),
    ] {
        server.push(Reply::json(value));
        assert_eq!(backend.poll().await.unwrap_err().kind, ErrorKind::Permanent);
    }
    // Explicit compatibility with Field Kit's missing-ErrorNumber behavior.
    server.push(Reply {
        status: 200,
        body: r#"{"Value":false,"Value":true}"#.into(),
        retry_after: None,
        delay: Duration::ZERO,
    });
    assert_eq!(backend.poll().await.unwrap_err().kind, ErrorKind::Permanent);
    server.push(Reply::json(json!({"Value":true})));
    assert_eq!(backend.poll().await.unwrap()["issafe"], true);
    server.push(Reply::json(
        json!({"ErrorNumber":1031, "ErrorMessage":"secret text"}),
    ));
    let error = backend.poll().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Disconnected);
    assert!(error.transport_lost);
    assert_eq!(error.upstream_code, Some(1031));
    assert!(!serde_json::to_string(&error).unwrap().contains("secret"));
}

#[tokio::test]
async fn http_errors_retry_after_and_redirects_are_bounded_without_automatic_retry() {
    let server = Server::new(vec![]).await;
    let mut backend = server.backend();
    for status in [408, 429, 500, 502, 503, 504, 401, 403, 404, 302] {
        server.push(Reply {
            status,
            body: "private upstream body".into(),
            retry_after: Some("172800".into()),
            delay: Duration::ZERO,
        });
        let error = backend.poll().await.unwrap_err();
        assert_eq!(
            error.kind,
            if matches!(status, 401 | 403 | 404 | 302) {
                ErrorKind::Permanent
            } else {
                ErrorKind::Transient
            }
        );
        assert_eq!(error.retry_after, Some(Duration::from_secs(172800)));
    }
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 10);
    server.push(Reply {
        status: 503,
        body: String::new(),
        retry_after: Some(httpdate::fmt_http_date(
            SystemTime::now() + Duration::from_secs(120),
        )),
        delay: Duration::ZERO,
    });
    let delay = backend.poll().await.unwrap_err().retry_after.unwrap();
    assert!((118..=120).contains(&delay.as_secs()));
}

#[tokio::test]
async fn stalled_and_oversized_responses_fail_and_writes_are_sent_once() {
    let mut server = Server::new(vec![]).await;
    server.config.polling.request_timeout_seconds = 0.05;
    let mut backend = server.backend();
    server.push(Reply {
        status: 200,
        body: " ".repeat(MAX_RESPONSE_BYTES + 1),
        retry_after: None,
        delay: Duration::ZERO,
    });
    assert_eq!(backend.poll().await.unwrap_err().kind, ErrorKind::Permanent);
    server.push(Reply {
        status: 200,
        body: "{}".into(),
        retry_after: None,
        delay: Duration::from_secs(5),
    });
    assert_eq!(
        backend
            .write("move".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn paths_identity_fields_and_connection_mutations_cannot_escape_the_adapter() {
    let server = Server::new(vec![]).await;
    let mut backend = server.backend();
    for member in ["../connected", "issafe?ClientID=3", "Connected", ""] {
        assert_eq!(
            backend
                .read(member.into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    for member in ["connected", "connect", "disconnect"] {
        assert_eq!(
            backend
                .write(member.into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(
        backend
            .read(
                "issafe".into(),
                Values::from([("clientID".into(), json!(3))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert!(server.fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actual_http_observations_drive_the_shared_safety_output() {
    use regain_hub::{
        config::{HubConfig, VirtualDevice},
        safety::MonotonicClock,
        safety_output::SafetyOutput,
        source::SourceRegistry,
    };
    let mut server = Server::new(vec![]).await;
    server.config.polling.poll_seconds = 0.1;
    server.config.polling.request_timeout_seconds = 0.2;
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    config.sources = vec![server.config.clone()];
    let VirtualDevice::Safety { members } = &mut config.outputs[0].device else {
        panic!("safety fixture");
    };
    members.truncate(1);
    members[0].source = server.config.id;
    members[0].policy.safe_readings_to_safe = 1;
    members[0].policy.return_to_safe_hold_seconds = 0.0;
    members[0].policy.confirmation_seconds = 0.1;
    members[0].policy.maximum_safe_age_seconds = 1.0;
    let members = members.clone();
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |source| {
        Ok(Box::new(AlpacaBackend::new(
            source,
            vec![SampleRequest::safety()],
            None,
        )?))
    })
    .unwrap();
    let output = SafetyOutput::new(&members, &registry, clock).unwrap();
    let mut status = output.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !status.borrow_and_update().is_safe {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    server.push(Reply::value(json!(false)));
    tokio::time::timeout(Duration::from_secs(3), async {
        while status.borrow_and_update().is_safe {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!output.snapshot().is_safe);
    assert!(
        server
            .fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    drop(output);
    assert!(!status.borrow().is_safe);
}

#[tokio::test]
async fn weather_sensor_ages_and_partial_errors_survive_the_http_source_and_output() {
    use regain_hub::{
        config::{HubConfig, Measurement, Readout, VirtualDevice, WeatherMetric},
        safety::MonotonicClock,
        source::SourceRegistry,
        weather::WeatherOutput,
    };
    let mut server = Server::new(vec![
        Reply::value(json!(true)),
        Reply::value(json!(5.0)),
        Reply::value(json!(12.5)),
        Reply::json(json!({"ErrorNumber":1024,"ErrorMessage":"pressure missing"})),
    ])
    .await;
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::ObservingConditions;
    }
    let temperature = Readout::Property {
        source: server.config.id,
        property: "temperature".into(),
        unit: None,
    };
    let pressure = Readout::Property {
        source: server.config.id,
        property: "pressure".into(),
        unit: None,
    };
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    config.sources = vec![server.config.clone()];
    config.outputs[0].device = VirtualDevice::Weather {
        measurements: std::collections::BTreeMap::from([
            (
                WeatherMetric::Temperature,
                Measurement {
                    sources: vec![temperature.clone()],
                    maximum_age_seconds: 60.0,
                    average_seconds: 0.0,
                },
            ),
            (
                WeatherMetric::Pressure,
                Measurement {
                    sources: vec![pressure.clone()],
                    maximum_age_seconds: 60.0,
                    average_seconds: 0.0,
                },
            ),
        ]),
    };
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&config, clock.clone(), |source| {
            Ok(Box::new(AlpacaBackend::new(
                source,
                vec![
                    SampleRequest::readout(&temperature, true),
                    SampleRequest::readout(&pressure, true),
                ],
                None,
            )?))
        })
        .unwrap(),
    );
    let output =
        WeatherOutput::new(&config, config.outputs[0].id, registry.clone(), clock).unwrap();
    let first = output.connect().await.unwrap();
    let second = output.connect().await.unwrap();
    let source = registry.get(server.config.id).unwrap();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while status.borrow_and_update().sequence == 0 {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let reading = first.read(WeatherMetric::Temperature).unwrap();
    assert_eq!(reading.value, 12.5);
    assert!(reading.age_seconds >= 5.0);
    assert_eq!(
        first.read(WeatherMetric::Pressure).unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        source.snapshot().sample_errors["pressure"].upstream_code,
        Some(1024)
    );
    assert_eq!(source.snapshot().lease_count, 2);
    drop(first);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(second.read(WeatherMetric::Temperature).unwrap().value, 12.5);
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].1.contains("SensorName=temperature"));
    assert!(requests[3].1.contains("SensorName=pressure"));
}
