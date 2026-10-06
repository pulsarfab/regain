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

#[derive(Clone)]
struct Reply {
    status: u16,
    body: String,
    retry_after: Option<String>,
    delay: Duration,
}
impl Reply {
    fn delayed(value: Value, millis: u64) -> Self {
        Self {
            delay: Duration::from_millis(millis),
            ..Self::value(value)
        }
    }
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

#[tokio::test]
async fn polling_budget_is_per_request_and_commands_interleave_without_rejuvenating_samples() {
    use regain_hub::{config::Readout, safety::MonotonicClock, source::SourceHandle};
    let mut server = Server::new(vec![Reply::value(json!(true))]).await;
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::Switch;
    }
    server.config.polling.request_timeout_seconds = 0.5;
    for _ in 0..5 {
        server.push(Reply::delayed(json!(42), 200));
    }
    let samples = (0..4)
        .map(|channel| {
            SampleRequest::readout(
                &Readout::Channel {
                    source: server.config.id,
                    channel,
                    unit: None,
                },
                false,
            )
        })
        .collect();
    let backend = AlpacaBackend::new(&server.config, samples, None).unwrap();
    let source = SourceHandle::spawn(
        server.config.id,
        Uuid::new_v4(),
        server.config.polling.clone(),
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !status.borrow_and_update().values.contains_key("channel/0") {
            status.changed().await.unwrap();
        }
        let first = source.snapshot();
        assert_eq!(
            source
                .read(lease, "maxswitch", Values::new())
                .await
                .unwrap(),
            42
        );
        while status.borrow_and_update().completed_passes == 0 {
            status.changed().await.unwrap();
        }
        let done = source.snapshot();
        assert_eq!(done.values.len(), 4);
        assert_eq!(done.generation, first.generation);
        assert_eq!(
            done.sample_started_seconds["channel/0"],
            first.sample_started_seconds["channel/0"]
        );
        assert_eq!(done.sample_sequences["channel/0"], 1);
        assert!(
            done.sample_started_seconds["channel/3"] - done.sample_started_seconds["channel/0"]
                > 0.5
        );
        assert!(done.error.is_none());
    })
    .await
    .unwrap();
    let requests = server.fixture.requests.lock().unwrap();
    let command = requests
        .iter()
        .position(|(_, uri, _)| uri.contains("/maxswitch?"))
        .unwrap();
    let last = requests
        .iter()
        .position(|(_, uri, _)| uri.contains("Id=3"))
        .unwrap();
    assert!(
        command < last,
        "A large polling pass must not monopolize the actor"
    );
}

#[tokio::test]
async fn weather_refresh_triggers_upstream_without_waiting_for_polling_and_respects_retry_after() {
    use regain_hub::{safety::MonotonicClock, source::SourceHandle};
    let mut server = Server::new(vec![Reply::value(json!(true))]).await;
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::ObservingConditions;
    }
    // An empty poll plan isolates the refresh request from normal sampling.
    let backend = AlpacaBackend::new(&server.config, vec![], None).unwrap();
    let source = SourceHandle::spawn(
        server.config.id,
        Uuid::new_v4(),
        server.config.polling.clone(),
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !status.borrow_and_update().transport_connected {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    server.push(Reply::json(json!({"ErrorNumber":0})));
    source.refresh(lease).await.unwrap();
    assert_eq!(
        server.fixture.requests.lock().unwrap().last().unwrap().0,
        "PUT"
    );
    assert!(
        server
            .fixture
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("/refresh")
    );
    server.push(Reply {
        status: 429,
        retry_after: Some("120".into()),
        ..Reply::value(Value::Null)
    });
    assert!(source.refresh(lease).await.is_err());
    let requests = server.fixture.requests.lock().unwrap().len();
    let error = source.refresh(lease).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.retry_after.unwrap() > Duration::from_secs(119));
    assert_eq!(server.fixture.requests.lock().unwrap().len(), requests);
}

#[tokio::test]
async fn transient_partial_sample_retries_same_key_before_advancing() {
    use regain_hub::config::Readout;
    let mut server = Server::new(vec![
        Reply::value(json!(true)),
        Reply {
            status: 503,
            retry_after: Some("5".into()),
            ..Reply::value(Value::Null)
        },
        Reply::value(json!(10)),
        Reply::value(json!(20)),
    ])
    .await;
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::Switch;
    }
    let samples = (0..2)
        .map(|channel| {
            SampleRequest::readout(
                &Readout::Channel {
                    source: server.config.id,
                    channel,
                    unit: None,
                },
                false,
            )
        })
        .collect();
    let mut backend = AlpacaBackend::new(&server.config, samples, None).unwrap();
    backend.connect().await.unwrap();
    let failed = backend.sample().await.unwrap();
    assert_eq!(
        failed.errors["channel/0"].retry_after,
        Some(Duration::from_secs(5))
    );
    assert!(failed.more);
    let retry = backend.sample().await.unwrap();
    assert_eq!(retry.values["channel/0"], 10);
    assert!(retry.more);
    let last = backend.sample().await.unwrap();
    assert_eq!(last.values["channel/1"], 20);
    assert!(!last.more);
}

#[tokio::test]
async fn partial_poll_retry_after_blocks_refresh_and_resumes_the_failed_sample() {
    use regain_hub::{config::Readout, safety::MonotonicClock, source::SourceHandle};
    let mut server = Server::new(vec![
        Reply::value(json!(true)),
        Reply {
            status: 503,
            retry_after: Some("1".into()),
            ..Reply::value(Value::Null)
        },
        Reply::value(json!(10)),
        Reply::value(json!(20)),
    ])
    .await;
    if let SourceBackend::Alpaca { device_type, .. } = &mut server.config.backend {
        *device_type = DeviceType::Switch;
    }
    let samples = (0..2)
        .map(|channel| {
            SampleRequest::readout(
                &Readout::Channel {
                    source: server.config.id,
                    channel,
                    unit: None,
                },
                false,
            )
        })
        .collect();
    let backend = AlpacaBackend::new(&server.config, samples, None).unwrap();
    let source = SourceHandle::spawn(
        server.config.id,
        Uuid::new_v4(),
        server.config.polling.clone(),
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !status
            .borrow_and_update()
            .sample_errors
            .contains_key("channel/0")
        {
            status.changed().await.unwrap();
        }
        let before = tokio::time::Instant::now();
        let error = source.refresh(lease).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        let remaining = error.retry_after.unwrap();
        assert!(remaining > Duration::from_millis(900));
        while status.borrow_and_update().completed_passes == 0 {
            status.changed().await.unwrap();
        }
        assert!(before.elapsed() >= remaining);
    })
    .await
    .unwrap();
    assert_eq!(source.snapshot().values["channel/0"], 10);
    assert_eq!(source.snapshot().values["channel/1"], 20);
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].1.contains("Id=0"));
    assert!(requests[2].1.contains("Id=0"));
    assert!(requests[3].1.contains("Id=1"));
}

#[tokio::test]
async fn mixed_native_and_http_switch_shares_native_temperature_with_weather() {
    use regain_hub::{
        config::{HubConfig, Measurement, OutputConfig, Readout, VirtualDevice, WeatherMetric},
        factory::{NoCredentials, build_sources},
        native::NativeRuntime,
        safety::MonotonicClock,
        switch::SwitchOutput,
        weather::WeatherOutput,
    };
    let Some(directory) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!("Mixed native source test requires REGAIN_TEST_WORKERS");
        return;
    };
    let runtime = NativeRuntime {
        directory: directory.into(),
        simulate: true,
    };
    let server = Server::new(vec![Reply::value(json!(true)), Reply::value(json!(1))]).await;
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/mixed-switch.json")).unwrap();
    let SourceBackend::Alpaca { base_url, .. } = &server.config.backend else {
        unreachable!()
    };
    let SourceBackend::Alpaca {
        base_url: target, ..
    } = &mut config.sources[0].backend
    else {
        unreachable!()
    };
    *target = base_url.clone();
    let SourceBackend::Native { identity, .. } = &mut config.sources[1].backend else {
        unreachable!()
    };
    *identity = "00:00:00:00:00:03".into();
    let native_source = config.sources[1].id;
    config.outputs.push(OutputConfig {
        id: Uuid::new_v4(),
        number: 0,
        label: "Shared native temperature".into(),
        device: VirtualDevice::Weather {
            measurements: std::collections::BTreeMap::from([(
                WeatherMetric::Temperature,
                Measurement {
                    sources: vec![Readout::Property {
                        source: native_source,
                        property: "temperature".into(),
                        unit: None,
                    }],
                    maximum_age_seconds: 60.0,
                    average_seconds: 0.0,
                },
            )]),
        },
    });
    let clock = Arc::new(MonotonicClock::default());
    let registry = build_sources(&config, &runtime, &NoCredentials, clock.clone()).unwrap();
    let switches = SwitchOutput::new(
        &config,
        config.outputs[0].id,
        registry.clone(),
        clock.clone(),
    )
    .unwrap();
    let weather =
        WeatherOutput::new(&config, config.outputs[1].id, registry.clone(), clock).unwrap();
    let first = switches.connect().await.unwrap();
    let second = switches.connect().await.unwrap();
    let weather = weather.connect().await.unwrap();
    let native = registry.get(native_source).unwrap();
    let generation = native.snapshot().generation;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if first.value(0).is_ok()
                && first.value(1).is_ok()
                && weather.read(WeatherMetric::Temperature).is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(first.value(0).unwrap(), 1.0);
    assert_eq!(
        first.value(1).unwrap(),
        weather.read(WeatherMetric::Temperature).unwrap().value
    );
    assert!(!first.can_write(1).await.unwrap());
    assert_eq!(native.snapshot().lease_count, 3);
    for value in [
        json!(true),
        json!(0),
        json!(1),
        json!(1),
        Value::Null,
        json!(0),
    ] {
        server.push(Reply::value(value));
    }
    first.set_value(0, 0.0).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while first.value(0).ok() != Some(0.0) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(first);
    drop(second);
    tokio::time::timeout(Duration::from_secs(3), async {
        while native.snapshot().lease_count != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(native.snapshot().generation, generation);
    assert!(weather.read(WeatherMetric::Temperature).is_ok());
    let requests = server.fixture.requests.lock().unwrap();
    let writes: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method == "PUT")
        .collect();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].1.contains("/setswitchvalue"));
}
#[derive(Default)]
struct Fixture {
    replies: Mutex<VecDeque<Reply>>,
    interface_reply: Mutex<Option<Reply>>,
    negotiation_requests: Mutex<Vec<(String, String, String)>>,
    authenticated_requests: std::sync::atomic::AtomicUsize,
    // Data/connection operations; version discovery is recorded separately so
    // transport assertions do not depend on metadata negotiation order.
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
    if request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        == Some("Bearer fixture-secret")
    {
        fixture
            .authenticated_requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let body = to_bytes(request.into_body(), 65536).await.unwrap();
    let metadata = uri
        .split('?')
        .next()
        .unwrap()
        .ends_with("/interfaceversion");
    let requests = if metadata {
        &fixture.negotiation_requests
    } else {
        &fixture.requests
    };
    requests.lock().unwrap().push((
        method,
        uri.clone(),
        String::from_utf8(body.to_vec()).unwrap(),
    ));
    let reply = if metadata {
        fixture
            .interface_reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Reply::value(json!(if uri.contains("/observingconditions/") {
                    1
                } else {
                    2
                }))
            })
    } else {
        fixture
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Reply::value(json!(true)))
    };
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
    assert!(requests[1].1.contains("ClientTransactionID=3"));
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
async fn configured_factory_resolves_one_credential_and_keeps_secrets_out_of_source_status() {
    use regain_hub::{
        config::{HubConfig, SafetyMember, VirtualDevice},
        factory::{CredentialProvider, build_sources},
        native::NativeRuntime,
        parameters::SafetyPolicy,
        readout::SourceLease,
        safety::MonotonicClock,
        source::SourceError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Credentials(AtomicUsize);
    impl CredentialProvider for Credentials {
        fn authorization(
            &self,
            reference: &str,
        ) -> Result<reqwest::header::HeaderValue, SourceError> {
            assert_eq!(reference, "fixture-reference");
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(reqwest::header::HeaderValue::from_static(
                "Bearer fixture-secret",
            ))
        }
    }
    let mut server = Server::new(vec![Reply::value(json!(true)), Reply::value(json!(true))]).await;
    let SourceBackend::Alpaca {
        credential_reference,
        ..
    } = &mut server.config.backend
    else {
        unreachable!()
    };
    *credential_reference = Some("fixture-reference".into());
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    config.sources = vec![server.config.clone()];
    config.outputs[0].device = VirtualDevice::Safety {
        members: vec![SafetyMember {
            source: server.config.id,
            enabled: true,
            policy: SafetyPolicy::default(),
        }],
    };
    let credentials = Credentials(AtomicUsize::new(0));
    let registry = build_sources(
        &config,
        &NativeRuntime {
            directory: ".".into(),
            simulate: false,
        },
        &credentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert_eq!(credentials.0.load(Ordering::Relaxed), 1);
    assert!(server.fixture.requests.lock().unwrap().is_empty());
    assert!(
        server
            .fixture
            .negotiation_requests
            .lock()
            .unwrap()
            .is_empty()
    );
    let source = registry.get(server.config.id).unwrap();
    let first = SourceLease::acquire(source.clone()).await.unwrap();
    let second = SourceLease::acquire(source.clone()).await.unwrap();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while status.borrow_and_update().values.get("issafe") != Some(&json!(true)) {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(credentials.0.load(Ordering::Relaxed), 1);
    assert_eq!(
        server
            .fixture
            .authenticated_requests
            .load(Ordering::Relaxed),
        3
    );
    let diagnostic = serde_json::to_string(&source.snapshot()).unwrap();
    assert!(!diagnostic.contains("fixture-secret"));
    assert!(!diagnostic.contains("fixture-reference"));
    drop(first);
    drop(second);
}

#[tokio::test]
async fn modern_managed_connection_claims_its_client_and_waits_for_completion() {
    use regain_hub::source::ConnectionMethod;
    let mut server = Server::new(vec![
        Reply::value(json!(true)),
        Reply::json(json!({"ErrorNumber":0})),
        Reply::value(json!(true)),
        Reply::value(json!(false)),
        Reply::value(json!(true)),
        Reply::json(json!({"ErrorNumber":0})),
    ])
    .await;
    server.managed();
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    let info = backend.connection_info().unwrap();
    assert_eq!(info.interface_version, Some(3));
    assert_eq!(info.method, ConnectionMethod::Async);
    assert!(info.owns_connection);
    backend.disconnect().await.unwrap();
    assert!(!backend.connection_info().unwrap().owns_connection);
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(requests[1].1.contains("/connect"));
    assert!(requests[2].1.contains("/connecting"));
    assert!(requests[3].1.contains("/connecting"));
    assert!(requests[4].1.contains("/connected"));
    assert!(requests[5].1.contains("/disconnect"));
    let forms: Vec<_> = [1, 5]
        .into_iter()
        .map(|index| {
            url::form_urlencoded::parse(requests[index].2.as_bytes())
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .collect();
    assert_eq!(forms[0]["ClientID"], forms[1]["ClientID"]);
    assert!(!forms[0].contains_key("Connected"));
}

#[tokio::test]
async fn modern_external_connection_does_not_claim_or_disconnect_the_device() {
    let server = Server::new(vec![Reply::value(json!(true))]).await;
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    backend.disconnect().await.unwrap();
    assert!(!backend.connection_info().unwrap().owns_connection);
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "GET");
}

#[tokio::test]
async fn metadata_fallback_is_limited_to_explicitly_missing_interface_version() {
    for reply in [
        Reply::json(json!({"ErrorNumber":1024})),
        Reply {
            status: 404,
            ..Reply::value(Value::Null)
        },
    ] {
        let server = Server::new(vec![Reply::value(json!(true))]).await;
        *server.fixture.interface_reply.lock().unwrap() = Some(reply);
        let mut backend = server.backend();
        backend.connect().await.unwrap();
        assert_eq!(backend.connection_info().unwrap().interface_version, None);
    }
    for reply in [
        Reply::value(json!(true)),
        Reply::value(json!(0)),
        Reply::value(json!(1.5)),
        // An ASCOM error number is not an HTTP status code.
        Reply::json(json!({"ErrorNumber":404})),
        Reply {
            status: 403,
            ..Reply::value(Value::Null)
        },
    ] {
        let server = Server::new(vec![]).await;
        *server.fixture.interface_reply.lock().unwrap() = Some(reply);
        assert_eq!(
            server.backend().connect().await.unwrap_err().kind,
            ErrorKind::Permanent
        );
        assert!(server.fixture.requests.lock().unwrap().is_empty());
    }
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":1024})),
    ])
    .await;
    server.managed();
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    assert_eq!(
        server.backend().connect().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(
        server
            .fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| method == "PUT")
            .all(|(_, uri, _)| uri.split('?').next().unwrap().ends_with("/connect"))
    );
}

#[tokio::test]
async fn asynchronous_connection_has_an_overall_deadline_and_never_replays_connect() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":0})),
    ])
    .await;
    server.managed();
    server.config.polling.connection_timeout_seconds = 1.0;
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    let mut backend = server.backend();
    let started = tokio::time::Instant::now();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let requests = server.fixture.requests.lock().unwrap().len();
    backend.reset();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert_eq!(server.fixture.requests.lock().unwrap().len(), requests);
    assert!(backend.connection_info().unwrap().owns_connection);
    backend.disconnect().await.unwrap();
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(method, uri, _)| method == "PUT"
                && uri.split('?').next().unwrap().ends_with("/connect"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(method, uri, _)| method == "PUT"
                && uri.split('?').next().unwrap().ends_with("/disconnect"))
            .count(),
        1
    );
}

#[tokio::test]
async fn uncertain_disconnect_is_not_replayed_during_reset_or_another_cleanup() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::json(json!({"ErrorNumber":0})),
        Reply::delayed(Value::Null, 500),
    ])
    .await;
    server.managed();
    server.config.polling.request_timeout_seconds = 0.1;
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    assert_eq!(
        backend.disconnect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    backend.reset();
    assert_eq!(
        backend.disconnect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cancelled_connection_write_is_not_replayed_or_disconnected_without_acknowledgement() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::delayed(Value::Null, 500),
    ])
    .await;
    server.managed();
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    let mut backend = server.backend();
    assert!(!backend.connect_step().await.unwrap());
    assert!(!backend.connect_step().await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), backend.connect_step())
            .await
            .is_err()
    );
    backend.reset();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    backend.disconnect().await.unwrap();
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
    assert!(backend.connection_info().unwrap().uncertain);
    assert!(!backend.connection_info().unwrap().owns_connection);
}

#[tokio::test]
async fn overall_connection_deadline_also_bounds_a_single_slow_metadata_request() {
    let mut server = Server::new(vec![]).await;
    server.config.polling.request_timeout_seconds = 5.0;
    server.config.polling.connection_timeout_seconds = 1.0;
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::delayed(json!(3), 3000));
    let began = tokio::time::Instant::now();
    assert_eq!(
        server.backend().connect().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(server.fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconnect_waits_for_previous_async_disconnect_before_claiming_a_new_lease() {
    let mut server = Server::new(vec![
        Reply::value(json!(false)),
        Reply::value(Value::Null),
        Reply::value(json!(false)),
        Reply::value(json!(true)),
        Reply::value(Value::Null),
        Reply::value(json!(true)),
        Reply::value(json!(false)),
        Reply::value(json!(true)),
        Reply::value(Value::Null),
        Reply::value(json!(false)),
        Reply::value(json!(true)),
    ])
    .await;
    server.managed();
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::value(json!(3)));
    let mut backend = server.backend();
    backend.connect().await.unwrap();
    backend.disconnect().await.unwrap();
    backend.connect().await.unwrap();
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 11);
    for (index, suffix) in [
        (4, "disconnect"),
        (5, "connecting"),
        (6, "connecting"),
        (7, "connected"),
        (8, "connect"),
    ] {
        assert!(
            requests[index]
                .1
                .split('?')
                .next()
                .unwrap()
                .ends_with(&format!("/{suffix}"))
        );
    }
    assert_eq!(server.fixture.negotiation_requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn actor_handshake_uses_separate_deadlines_and_does_not_count_pending_as_failed_safety() {
    use regain_hub::{safety::MonotonicClock, source::SourceHandle};
    let mut server = Server::new(vec![
        Reply::delayed(json!(false), 150),
        Reply::delayed(Value::Null, 150),
        Reply::delayed(json!(false), 150),
        Reply::delayed(json!(true), 150),
        Reply::delayed(json!(true), 150),
    ])
    .await;
    server.managed();
    server.config.polling.request_timeout_seconds = 0.3;
    *server.fixture.interface_reply.lock().unwrap() = Some(Reply::delayed(json!(3), 150));
    let source = SourceHandle::spawn(
        server.config.id,
        Uuid::new_v4(),
        server.config.polling.clone(),
        Box::new(server.backend()),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let mut events = source.subscribe();
    let lease = Uuid::new_v4();
    let pending = source.acquire(lease).await.unwrap();
    assert!(!pending.transport_connected);
    assert_eq!(pending.error.unwrap().kind, ErrorKind::Connecting);
    let first = tokio::time::timeout(Duration::from_secs(4), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(first.result.unwrap()["issafe"], true);
    assert!(source.snapshot().connection_info.unwrap().owns_connection);
    source.release(lease).await.unwrap();
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
        while status.borrow_and_update().completed_passes == 0 {
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
