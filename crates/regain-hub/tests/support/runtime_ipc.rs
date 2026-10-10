use super::*;
use regain_hub::ipc::{
    Limits, MAX_FRAME_BYTES, MAX_IN_FLIGHT, ProtocolError, read_frame, serve_stream,
};
use tokio::{
    io::{AsyncWriteExt, DuplexStream},
    task::JoinHandle,
};

struct TrackedStream {
    inner: DuplexStream,
    dropped: Arc<AtomicBool>,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for TrackedStream {
    fn drop(&mut self) {
        self.dropped.store(true, SeqCst);
    }
}
impl tokio::io::AsyncRead for TrackedStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl tokio::io::AsyncWrite for TrackedStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn ipc_protocol_failure_drops_both_stream_halves_before_returning() {
    let f = fixture();
    let (mut client, server) = tokio::io::duplex(128);
    let dropped = Arc::new(AtomicBool::new(false));
    let stream = TrackedStream {
        inner: server,
        dropped: dropped.clone(),
        entered: None,
    };
    // Keep the peer open: the reader is waiting for another frame when the
    // dispatcher rejects this one. Do not yield after serving completes.
    let send = async {
        client.write_all(&1u32.to_le_bytes()).await.unwrap();
        client.write_all(b"{").await.unwrap();
    };
    let (result, ()) = tokio::join!(
        serve_stream(stream, f.runtime.clone(), Limits::default()),
        send
    );
    assert!(matches!(result, Err(ProtocolError::Malformed)));
    assert!(
        dropped.load(SeqCst),
        "Returned with a reader still owning the stream"
    );
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn ipc_future_cancellation_drops_both_stream_halves_synchronously() {
    let f = fixture();
    let (_client, server) = tokio::io::duplex(128);
    let dropped = Arc::new(AtomicBool::new(false));
    let (entered, reading) = tokio::sync::oneshot::channel();
    let stream = TrackedStream {
        inner: server,
        dropped: dropped.clone(),
        entered: Some(entered),
    };
    let mut serving = Box::pin(serve_stream(stream, f.runtime.clone(), Limits::default()));
    tokio::select! {
        result = &mut serving => panic!("Silent stream finished before cancellation: {result:?}"),
        result = reading => result.unwrap(),
    }
    assert!(!dropped.load(SeqCst));
    drop(serving);
    assert!(
        dropped.load(SeqCst),
        "Cancellation left a reader owning the stream"
    );
    f.runtime.shutdown().await.unwrap();
}

struct Peer {
    stream: DuplexStream,
    task: JoinHandle<Result<(), ProtocolError>>,
    next_id: u64,
    hello: Value,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Peer {
    async fn start(runtime: Arc<HubRuntime>, limits: Limits) -> Self {
        Self::with_capacity(runtime, limits, MAX_FRAME_BYTES + 4).await
    }
    async fn with_capacity(runtime: Arc<HubRuntime>, limits: Limits, capacity: usize) -> Self {
        let (stream, server) = tokio::io::duplex(capacity);
        let task = tokio::spawn(serve_stream(server, runtime, limits));
        let mut peer = Self {
            stream,
            task,
            next_id: 1,
            hello: Value::Null,
        };
        let hello = peer.call(json!({"op":"hello"})).await;
        assert_eq!(hello["version"], 1);
        peer.hello = hello["result"].clone();
        peer
    }
    async fn raw(&mut self, bytes: &[u8]) {
        self.stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        self.stream.write_all(bytes).await.unwrap();
    }
    async fn send(&mut self, command: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.raw(&serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap())
            .await;
        id
    }
    async fn receive(&mut self) -> Value {
        let frame = tokio::time::timeout(
            Duration::from_secs(3),
            read_frame(&mut self.stream, Duration::from_secs(1)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        serde_json::from_slice(&frame).unwrap()
    }
    async fn call(&mut self, command: Value) -> Value {
        let id = self.send(command).await;
        let response = self.receive().await;
        assert_eq!(response["id"], id);
        response
    }
}
fn get(output: Uuid, property: Value) -> Value {
    json!({"op":"get","output":output,"property":property})
}
fn put(output: Uuid, property: Value) -> Value {
    json!({"op":"put","output":output,"property":property})
}
fn connect(output: Uuid) -> Value {
    json!({"op":"connect","output":output})
}

#[tokio::test(start_paused = true)]
async fn ipc_resume_keeps_scalar_clients_available_while_new_sources_are_stalled() {
    let f = fixture();
    let mut first = Peer::start(f.runtime.clone(), Limits::default()).await;
    let mut second = Peer::start(f.runtime.clone(), Limits::default()).await;
    for output in [f.safety, f.weather, f.switch] {
        assert!(first.call(connect(output)).await.get("error").is_none());
    }
    assert!(second.call(connect(f.safety)).await.get("error").is_none());
    settle().await;
    assert_eq!(
        first.call(get(f.safety, json!({"member":"isSafe"}))).await["result"],
        true
    );
    assert_eq!(
        first
            .call(get(
                f.weather,
                json!({"member":"measurement","metric":"temperature"})
            ))
            .await["result"]["value"],
        20.0
    );
    for device in &f.devices {
        device.hang_connect.store(true, SeqCst);
    }
    f.clock.notify_resume();
    settle().await;
    for peer in [&mut first, &mut second] {
        assert_eq!(
            peer.call(get(f.safety, json!({"member":"connected"})))
                .await["result"],
            true
        );
        assert_eq!(
            peer.call(get(f.safety, json!({"member":"isSafe"}))).await["result"],
            false
        );
    }
    assert!(
        first
            .call(get(
                f.weather,
                json!({"member":"measurement","metric":"temperature"})
            ))
            .await
            .get("error")
            .is_some()
    );
    assert!(
        first
            .call(get(f.switch, json!({"member":"getSwitchValue","id":0})))
            .await
            .get("error")
            .is_some()
    );
    assert!(
        f.devices
            .iter()
            .all(|device| device.writes.load(SeqCst) == 0)
    );
    for device in &f.devices {
        device.hang_connect.store(false, SeqCst);
    }
    drop((first, second));
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn ipc_transfer_never_opens_sources_or_persists_an_import() {
    let f = fixture();
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    for operation in ["exportConfig", "prepareImport"] {
        assert!(
            peer.hello["operations"]
                .as_array()
                .unwrap()
                .contains(&json!(operation))
        );
    }
    let exported = peer
        .call(json!({"op":"exportConfig","expectedRevision":f.config.revision}))
        .await;
    let document = exported["result"].to_string();
    let prepared = peer.call(json!({"op":"prepareImport","expectedRevision":f.config.revision,"mode":"copy","document":document})).await;
    assert_eq!(prepared["result"]["mode"], "copy");
    assert!(
        !prepared["result"]["remappedIds"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        peer.call(json!({"op":"getConfig"})).await["result"],
        serde_json::to_value(&f.config).unwrap()
    );
    let duplicate = format!("{{\"formatVersion\":1,{}", &document[1..]);
    let rejected = peer.call(json!({"op":"prepareImport","expectedRevision":f.config.revision,"mode":"restore","document":duplicate})).await;
    assert_eq!(rejected["error"]["code"], "invalidConfig");
    assert_eq!(
        peer.call(json!({"op":"exportConfig","expectedRevision":Uuid::new_v4()}))
            .await["error"]["code"],
        "revisionConflict"
    );
    assert_eq!(f.runtime.active_connections(), 0);
    assert!(f.devices.iter().all(|d| d.connects.load(SeqCst) == 0));
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn ipc_output_diagnostics_negotiate_read_only_revision_fenced_pages() {
    let f = fixture();
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    assert!(
        peer.hello["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("outputStatus"))
    );
    let command = json!({"op":"outputStatus","output":f.switch,"expectedRevision":f.config.revision,"start":0,"limit":1});
    let observed = peer.call(command.clone()).await;
    assert_eq!(observed["result"]["purpose"], "cachedDiagnostics");
    assert_eq!(observed["result"]["nextStart"], 1);
    let mut stale = command.clone();
    stale["expectedRevision"] = json!(Uuid::new_v4());
    assert_eq!(peer.call(stale).await["error"]["code"], "revisionConflict");
    let mut invalid = command.clone();
    invalid["limit"] = json!(33);
    assert_eq!(peer.call(invalid).await["error"]["code"], "invalidValue");
    let mut missing = command;
    missing.as_object_mut().unwrap().remove("expectedRevision");
    assert!(serde_json::from_value::<regain_hub::ipc::Command>(missing).is_err());
    assert_eq!(f.runtime.active_connections(), 0);
    assert!(
        f.runtime
            .source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0)
    );
    assert!(
        f.devices
            .iter()
            .all(|device| device.connects.load(SeqCst) == 0
                && device.reads.load(SeqCst) == 0
                && device.writes.load(SeqCst) == 0
                && device.polls.load(SeqCst) == 0)
    );
    drop(peer);
}

#[tokio::test(start_paused = true)]
async fn ipc_async_connection_uses_shared_progress_and_failure_without_replaying() {
    let f = fixture();
    f.devices[0].hang_connect.store(true, SeqCst);
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    assert!(
        peer.hello["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("changeConnection"))
    );
    let accepted = peer
        .call(
            json!({"op":"changeConnection","output":f.switch,"connected":true,"asynchronous":true}),
        )
        .await;
    assert!(accepted.get("result").is_some(), "{accepted}");
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connecting"})))
            .await["result"],
        true
    );
    assert_eq!(peer.call(connect(f.switch)).await["error"]["code"], "busy");
    assert_eq!(
        peer.call(json!({"op":"disconnect","output":f.switch}))
            .await["error"]["code"],
        "busy"
    );
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    let failure = peer
        .call(get(f.switch, json!({"member":"connecting"})))
        .await;
    assert!(failure.get("error").is_some(), "{failure}");
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connected"})))
            .await["result"],
        false
    );
    let attempts = f.devices[0].connects.load(SeqCst);
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connecting"})))
            .await["error"]["code"],
        failure["error"]["code"]
    );
    assert_eq!(f.devices[0].connects.load(SeqCst), attempts);
    assert!(
        peer.call(
            json!({"op":"changeConnection","output":f.switch,"connected":false,"asynchronous":true})
        )
        .await
        .get("result")
        .is_some()
    );
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connecting"})))
            .await["result"],
        false
    );
    drop(peer);
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn device_state_is_cached_and_failed_samples_do_not_refresh_safety_or_weather() {
    let f = fixture();
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    assert!(
        peer.hello["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("scalarDeviceState"))
    );
    assert_eq!(
        peer.call(get(f.weather, json!({"member":"deviceState"})))
            .await["error"]["code"],
        "disconnected"
    );
    for output in [f.switch, f.safety, f.weather] {
        peer.call(connect(output)).await;
    }
    settle().await;
    // Source read calls are deliberately stalled. The bundle must read the
    // shared cache/policy and never probe individual capabilities or sensors.
    for device in &f.devices {
        device.hang_read.store(true, SeqCst);
    }
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"deviceState"})))
            .await["result"],
        json!([
            {"Name":"GetSwitch0","Value":true}, {"Name":"GetSwitchValue0","Value":1.0},
            {"Name":"GetSwitch1","Value":true}, {"Name":"GetSwitchValue1","Value":20.0}
        ])
    );
    assert_eq!(
        peer.call(get(f.weather, json!({"member":"deviceState"})))
            .await["result"],
        json!([
            {"Name":"Temperature","Value":20.0}
        ])
    );
    assert_eq!(
        peer.call(get(f.safety, json!({"member":"deviceState"})))
            .await["result"],
        json!([
            {"Name":"IsSafe","Value":true}
        ])
    );
    assert!(
        f.devices
            .iter()
            .all(|device| device.reads.load(SeqCst) == 0)
    );
    for device in &f.devices {
        device.hang_poll.store(true, SeqCst);
    }
    tokio::time::advance(Duration::from_secs(15)).await;
    settle().await;
    for _ in 0..3 {
        assert_eq!(
            peer.call(get(f.safety, json!({"member":"deviceState"})))
                .await["result"],
            json!([
                {"Name":"IsSafe","Value":false}
            ])
        );
        assert_eq!(
            peer.call(get(f.switch, json!({"member":"deviceState"})))
                .await["result"],
            json!([])
        );
    }
    tokio::time::advance(Duration::from_secs(61)).await;
    settle().await;
    assert_eq!(
        peer.call(get(f.weather, json!({"member":"deviceState"})))
            .await["result"],
        json!([])
    );
    assert!(
        f.devices
            .iter()
            .all(|device| device.reads.load(SeqCst) == 0)
    );
    drop(peer);
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn setup_inspection_is_advertised_and_available_without_connecting_an_output() {
    let f = fixture();
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    assert!(
        peer.hello["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("inspectSource"))
    );
    let source = f.config.sources[0].id;
    let metadata = peer.call(json!({"op":"describeConfig"})).await;
    assert_eq!(
        metadata["result"]["capabilityInspection"]["parameters"]["limit"]["maximum"],
        regain_hub::capabilities::MAX_CHANNEL_PAGE
    );
    let response = peer
        .call(json!({"op":"inspectSource","source":source,"start":0,"limit":1}))
        .await;
    assert_eq!(response["result"]["source"], source.to_string());
    assert_eq!(response["result"]["capabilities"]["kind"], "switch");
    assert_eq!(
        response["result"]["capabilities"]["channels"][0]["canWrite"]["value"],
        true
    );
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connected"})))
            .await["result"],
        false
    );
    assert_eq!(f.runtime.active_connections(), 0);
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn protocol_routes_shared_switch_safety_weather_and_configuration() {
    let f = fixture();
    let mut a = Peer::start(f.runtime.clone(), Limits::default()).await;
    let mut b = Peer::start(f.runtime.clone(), Limits::default()).await;
    assert_ne!(a.hello["clientId"], b.hello["clientId"]);
    assert_eq!(a.hello["hostInstance"], f.runtime.runtime_id().to_string());
    assert_eq!(
        a.hello["configurationRevision"],
        f.config.revision.to_string()
    );
    assert!(
        !a.hello["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("applyConfig"))
    );
    assert_eq!(
        a.call(get(f.safety, json!({"member":"connected"}))).await["result"],
        false
    );
    for output in [f.switch, f.safety, f.weather] {
        assert!(a.call(connect(output)).await.get("result").is_some());
    }
    for output in [f.safety, f.weather] {
        b.call(connect(output)).await;
    }
    settle().await;
    assert_eq!(
        a.call(get(f.safety, json!({"member":"isSafe"}))).await["result"],
        true
    );
    assert_eq!(
        a.call(get(f.switch, json!({"member":"maxSwitch"}))).await["result"],
        2
    );
    assert_eq!(
        a.call(get(f.switch, json!({"member":"getSwitchValue","id":1})))
            .await["result"],
        20.0
    );
    assert_eq!(
        a.call(get(f.switch, json!({"member":"canWrite","id":1})))
            .await["result"],
        false
    );
    assert_eq!(
        a.call(get(f.switch, json!({"member":"minSwitchValue","id":0})))
            .await["result"],
        0.0
    );
    assert_eq!(
        a.call(get(f.switch, json!({"member":"isSafe"}))).await["error"]["code"],
        "unsupported"
    );
    assert_eq!(
        a.call(put(
            f.switch,
            json!({"member":"setSwitchValue","id":0,"value":0.0})
        ))
        .await["result"],
        Value::Null
    );
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    a.call(put(
        f.weather,
        json!({"member":"averagePeriod","hours":0.01}),
    ))
    .await;
    assert_eq!(
        b.call(get(f.weather, json!({"member":"averagePeriod"})))
            .await["result"],
        0.01
    );
    assert_eq!(
        b.call(get(
            f.weather,
            json!({"member":"measurement","metric":"temperature"})
        ))
        .await["result"]["value"],
        20.0
    );
    assert_eq!(
        a.call(json!({"op":"getConfig"})).await["result"]["revision"],
        f.config.revision.to_string()
    );
    assert_eq!(
        a.call(json!({"op":"describeConfig"})).await["result"]["contractVersion"],
        1
    );
    assert_eq!(
        a.call(json!({"op":"listDevices"})).await["result"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let mut invalid = f.config.clone();
    invalid.revision = Uuid::nil();
    assert_eq!(
        a.call(json!({"op":"validateConfig","candidate":invalid}))
            .await["result"]["valid"],
        false
    );
    assert_eq!(
        a.call(get(Uuid::new_v4(), json!({"member":"connected"})))
            .await["error"]["code"],
        "invalidValue"
    );
    a.stream.shutdown().await.unwrap();
    (&mut a.task).await.unwrap().unwrap();
    settle().await;
    assert_eq!(
        b.call(get(f.safety, json!({"member":"isSafe"}))).await["result"],
        true
    );
    assert_eq!(f.devices[1].disconnects.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn cached_safety_reply_can_overtake_a_stalled_capability_request() {
    let f = fixture();
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    peer.call(connect(f.switch)).await;
    peer.call(connect(f.safety)).await;
    settle().await;
    f.devices[0].hang_read.store(true, SeqCst);
    let slow = peer
        .send(get(f.switch, json!({"member":"canWrite","id":0})))
        .await;
    let fast = peer.send(get(f.safety, json!({"member":"isSafe"}))).await;
    let reply = peer.receive().await;
    assert_eq!(reply["id"], fast);
    assert_eq!(reply["result"], true);
    let reply = peer.receive().await;
    assert_eq!(reply["id"], slow);
    assert!(reply.get("error").is_some());
}

#[tokio::test(start_paused = true)]
async fn eof_and_server_task_cancellation_cleanup_a_pending_connect_promptly() {
    for cancel in [false, true] {
        let f = fixture();
        f.devices[0].hang_connect.store(true, SeqCst);
        let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
        peer.send(connect(f.switch)).await;
        settle().await;
        assert_eq!(f.runtime.active_connections(), 1);
        if cancel {
            peer.task.abort();
            assert!((&mut peer.task).await.unwrap_err().is_cancelled());
        } else {
            peer.stream.shutdown().await.unwrap();
            (&mut peer.task).await.unwrap().unwrap();
        }
        settle().await;
        assert_eq!(f.runtime.active_connections(), 0);
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        assert!(f.registry.snapshots().iter().all(|s| s.lease_count == 0));
    }
}

#[tokio::test(start_paused = true)]
async fn disconnect_sees_an_earlier_connect_reservation_without_waiting_for_io() {
    let f = fixture();
    f.devices[0].hang_connect.store(true, SeqCst);
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    let opening = peer.send(connect(f.switch)).await;
    let closing = peer
        .send(json!({"op":"disconnect","output":f.switch}))
        .await;
    let first = peer.receive().await;
    let second = peer.receive().await;
    let replies = BTreeMap::from([
        (first["id"].as_u64().unwrap(), first),
        (second["id"].as_u64().unwrap(), second),
    ]);
    assert!(replies[&closing].get("result").is_some());
    assert_eq!(replies[&opening]["error"]["code"], "disconnected");
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"connected"})))
            .await["result"],
        false
    );
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_put_is_uncertain_and_never_automatically_replayed() {
    let f = fixture();
    let mut peer = Peer::start(
        f.runtime.clone(),
        Limits {
            operation_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
    )
    .await;
    peer.call(connect(f.switch)).await;
    settle().await;
    f.devices[0].hang_write.store(true, SeqCst);
    let reply = peer
        .call(put(
            f.switch,
            json!({"member":"setSwitch","id":0,"state":false}),
        ))
        .await;
    assert_eq!(reply["error"]["code"], "uncertain");
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    assert_eq!(
        peer.call(get(f.switch, json!({"member":"maxSwitch"})))
            .await["result"],
        2
    );
}

#[tokio::test(start_paused = true)]
async fn malformed_unknown_and_impersonating_requests_close_without_dispatch() {
    let f = fixture();
    for bytes in [
        br#"{"version":1,"version":1,"id":2,"command":{"op":"listDevices"}}"#.as_slice(),
        br#"{"version":1,"id":2,"clientId":"spoof","command":{"op":"listDevices"}}"#,
        br#"{"version":1,"id":2,"command":{"op":"arbitraryUsbWrite"}}"#,
        br#"{"version":1,"id":2,"command":{"op":"listDevices","unknown":1}}"#,
        br#"{"version":1,"id":2,"command":{"op":"get","output":"00000000-0000-0000-0000-000000000001","property":{"member":"connected","clientId":"spoof"}}}"#,
        br#"{"version":1,"id":2,"command":{"op":"put","output":"00000000-0000-0000-0000-000000000001","property":{"member":"refresh","unknown":1}}}"#,
        br#"{"version":1,"id":2,"command":{"op":"put","output":"00000000-0000-0000-0000-000000000001","property":{"member":"setSwitch","id":0,"state":"true"}}}"#,
    ] {
        let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
        peer.raw(bytes).await;
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), &mut peer.task).await.expect("Malformed request was accepted").unwrap(), Err(ProtocolError::Malformed));
    }
    assert!(
        f.devices
            .iter()
            .all(|d| d.connects.load(SeqCst) == 0 && d.writes.load(SeqCst) == 0)
    );
}

#[tokio::test(start_paused = true)]
async fn version_handshake_and_request_order_are_enforced() {
    let f = fixture();
    for (request, expected) in [
        (
            json!({"version":2,"id":1,"command":{"op":"hello"}}),
            ProtocolError::Version,
        ),
        (
            json!({"version":1,"id":1,"command":{"op":"listDevices"}}),
            ProtocolError::Handshake,
        ),
    ] {
        let (mut stream, server) = tokio::io::duplex(1024);
        let task = tokio::spawn(serve_stream(server, f.runtime.clone(), Limits::default()));
        let bytes = serde_json::to_vec(&request).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
        assert_eq!(task.await.unwrap(), Err(expected));
    }
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    peer.call(connect(f.switch)).await;
    settle().await;
    let command = put(
        f.switch,
        json!({"member":"setSwitchValue","id":0,"value":0.0}),
    );
    peer.call(command.clone()).await;
    peer.raw(
        &serde_json::to_vec(&json!({"version":1,"id":peer.next_id-1,"command":command})).unwrap(),
    )
    .await;
    assert_eq!(
        (&mut peer.task).await.unwrap(),
        Err(ProtocolError::RequestOrder)
    );
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn frame_lengths_partial_messages_and_concurrency_are_bounded() {
    let f = fixture();
    for (size, expected) in [
        (0u32, ProtocolError::Malformed),
        (MAX_FRAME_BYTES as u32 + 1, ProtocolError::FrameTooLarge),
        (32, ProtocolError::Timeout),
    ] {
        let mut peer = Peer::start(
            f.runtime.clone(),
            Limits {
                frame_timeout: Duration::from_millis(100),
                ..Limits::default()
            },
        )
        .await;
        peer.stream.write_all(&size.to_le_bytes()).await.unwrap();
        assert_eq!((&mut peer.task).await.unwrap(), Err(expected));
    }
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    peer.call(connect(f.switch)).await;
    f.devices[0].hang_read.store(true, SeqCst);
    for _ in 0..=MAX_IN_FLIGHT {
        peer.send(get(f.switch, json!({"member":"canWrite","id":0})))
            .await;
    }
    assert_eq!(
        (&mut peer.task).await.unwrap(),
        Err(ProtocolError::Overloaded)
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(f.runtime.active_connections(), 0);
}

#[tokio::test(start_paused = true)]
async fn oversized_responses_return_a_bounded_error_and_keep_the_stream_usable() {
    let f = fixture();
    f.devices[1].large_sample.store(true, SeqCst);
    let mut peer = Peer::start(f.runtime.clone(), Limits::default()).await;
    peer.call(connect(f.weather)).await;
    settle().await;
    let reply = peer
        .call(json!({"op":"sourceStatus","source":f.config.sources[1].id}))
        .await;
    assert_eq!(reply["error"]["code"], "responseTooLarge");
    assert_eq!(
        peer.call(json!({"op":"listDevices"})).await["result"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test(start_paused = true)]
async fn a_client_that_stops_reading_is_closed_at_the_frame_deadline() {
    let f = fixture();
    let mut peer = Peer::with_capacity(
        f.runtime.clone(),
        Limits {
            frame_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
        256,
    )
    .await;
    peer.call(connect(f.safety)).await;
    settle().await;
    assert_eq!(f.runtime.active_connections(), 1);
    peer.send(json!({"op":"describeConfig"})).await;
    // The schema cannot fit in the stream's buffer; do not read the reply.
    assert_eq!((&mut peer.task).await.unwrap(), Err(ProtocolError::Timeout));
    settle().await;
    assert_eq!(f.runtime.active_connections(), 0);
    assert_eq!(
        f.registry
            .get(f.config.sources[2].id)
            .unwrap()
            .snapshot()
            .lease_count,
        0
    );
}
