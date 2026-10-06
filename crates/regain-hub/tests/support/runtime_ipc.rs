use super::*;
use regain_hub::ipc::{
    Limits, MAX_FRAME_BYTES, MAX_IN_FLIGHT, ProtocolError, read_frame, serve_stream,
};
use tokio::{
    io::{AsyncWriteExt, DuplexStream},
    task::JoinHandle,
};

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
