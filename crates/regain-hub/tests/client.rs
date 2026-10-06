use regain_hub::{
    client::{Client, ClientError, ClientLimits},
    config::HubConfig,
    factory::NoCredentials,
    ipc::{Command, Get, Limits, MAX_FRAME_BYTES, Put, read_frame, serve_stream},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncWriteExt, DuplexStream};
use uuid::Uuid;

async fn receive(peer: &mut DuplexStream) -> Value {
    serde_json::from_slice(
        &read_frame(peer, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
async fn raw(peer: &mut DuplexStream, bytes: &[u8]) {
    peer.write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    peer.write_all(bytes).await.unwrap();
}
async fn send(peer: &mut DuplexStream, value: Value) {
    raw(peer, &serde_json::to_vec(&value).unwrap()).await;
}
async fn hello(peer: &mut DuplexStream, instance: Uuid, limit: usize) {
    assert_eq!(receive(peer).await["command"]["op"], "hello");
    send(peer, json!({"version":1,"id":1,"result":{
        "protocolVersion":1,"instanceId":instance,"hostInstance":Uuid::new_v4(),"configurationRevision":Uuid::new_v4(),
        "clientId":Uuid::new_v4(),"maxFrameBytes":limit,"maxInFlight":2,
        "operations":["get","put","listDevices","sourceStatus","connect","disconnect"],"capabilities":["switchOutputs"]
    }})).await;
}
async fn fake(limits: ClientLimits, frame: usize) -> (Client, DuplexStream) {
    let (stream, mut peer) = tokio::io::duplex(MAX_FRAME_BYTES * 2);
    let instance = Uuid::new_v4();
    let handshake = tokio::spawn(async move {
        hello(&mut peer, instance, frame).await;
        peer
    });
    let client = Client::from_stream(stream, instance, Duration::from_secs(1), limits)
        .await
        .unwrap();
    (client, handshake.await.unwrap())
}
fn get() -> Command {
    Command::Get {
        output: Uuid::new_v4(),
        property: Get::IsSafe {},
    }
}
fn put() -> Command {
    Command::Put {
        output: Uuid::new_v4(),
        property: Put::SetSwitch { id: 0, state: true },
    }
}
fn call(client: &Client, command: Command) -> tokio::task::JoinHandle<Result<Value, ClientError>> {
    let client = client.clone();
    tokio::spawn(async move { client.request(command).await })
}

#[tokio::test]
async fn out_of_order_replies_are_correlated_and_slow_write_does_not_block_safety() {
    let (client, mut peer) = fake(ClientLimits::default(), MAX_FRAME_BYTES).await;
    let a = call(&client, put());
    let first = receive(&mut peer).await;
    let b = call(&client, get());
    let second = receive(&mut peer).await;
    assert!(second["id"].as_u64() > first["id"].as_u64());
    send(
        &mut peer,
        json!({"version":1,"id":second["id"],"result":false}),
    )
    .await;
    assert_eq!(b.await.unwrap().unwrap(), false);
    assert!(!a.is_finished());
    send(
        &mut peer,
        json!({"version":1,"id":first["id"],"result":null}),
    )
    .await;
    assert_eq!(a.await.unwrap().unwrap(), Value::Null);
    assert!(client.is_connected());
    client.close();
    assert!(matches!(
        client.request(get()).await,
        Err(ClientError::Disconnected)
    ));
}

#[tokio::test]
async fn cancelled_dispatched_requests_keep_capacity_until_reply_and_commands_are_not_replayed() {
    let (client, mut peer) = fake(ClientLimits::default(), MAX_FRAME_BYTES).await;
    let a = call(&client, put());
    let first = receive(&mut peer).await;
    a.abort();
    let _ = a.await;
    let b = call(&client, get());
    let second = receive(&mut peer).await;
    assert!(matches!(
        client.request(get()).await,
        Err(ClientError::Busy)
    ));
    send(
        &mut peer,
        json!({"version":1,"id":first["id"],"result":null}),
    )
    .await;
    send(
        &mut peer,
        json!({"version":1,"id":second["id"],"result":true}),
    )
    .await;
    assert_eq!(b.await.unwrap().unwrap(), true);
    let c = call(&client, get());
    let third = receive(&mut peer).await;
    assert_eq!(
        third["id"].as_u64().unwrap(),
        first["id"].as_u64().unwrap() + 2
    );
    send(
        &mut peer,
        json!({"version":1,"id":third["id"],"result":false}),
    )
    .await;
    assert_eq!(c.await.unwrap().unwrap(), false);
}

#[tokio::test]
async fn connection_loss_and_deadlines_make_dispatched_mutations_uncertain() {
    for lose_peer in [false, true] {
        let (client, mut peer) = fake(
            ClientLimits {
                request_timeout: Duration::from_millis(50),
                ..Default::default()
            },
            MAX_FRAME_BYTES,
        )
        .await;
        let write = call(&client, put());
        receive(&mut peer).await;
        let read = call(&client, get());
        receive(&mut peer).await;
        if lose_peer {
            drop(peer);
        }
        assert!(matches!(write.await.unwrap(), Err(ClientError::Uncertain)));
        assert!(matches!(
            read.await.unwrap(),
            Err(ClientError::Disconnected | ClientError::Timeout)
        ));
        client.closed().await;
        assert!(!client.is_connected());
        assert!(matches!(
            client.request(put()).await,
            Err(ClientError::Disconnected)
        ));
    }
}

#[tokio::test]
async fn malformed_duplicate_and_uncorrelated_replies_close_the_entire_session() {
    for bytes in [
        br#"{"version":1,"id":99,"result":true}"#.as_slice(),
        br#"{"version":2,"id":2,"result":true}"#,
        br#"{"version":1,"id":2,"id":2,"result":true}"#,
        br#"{"version":1,"id":2,"result":true,"error":null}"#,
        br#"{"version":1,"id":2,"result":true,"extra":1}"#,
        br#"{"version":1,"id":2,"error":{"code":"retry","message":"bad age","retryAfterSeconds":-1}}"#,
    ] {
        let (client, mut peer) = fake(ClientLimits::default(), MAX_FRAME_BYTES).await;
        let request = call(&client, get()); receive(&mut peer).await;
        raw(&mut peer, bytes).await;
        assert!(matches!(request.await.unwrap(), Err(ClientError::Protocol)), "{bytes:?}");
        client.closed().await;
    }
}

#[tokio::test]
async fn bounded_encoding_rejects_large_requests_without_writing_and_remote_errors_remain_structured()
 {
    let (client, mut peer) = fake(ClientLimits::default(), 512).await;
    assert!(matches!(
        client
            .request(Command::Get {
                output: Uuid::new_v4(),
                property: Get::TimeSinceLastUpdate {
                    sensor: "x".repeat(2048)
                }
            })
            .await,
        Err(ClientError::InvalidRequest)
    ));
    assert!(matches!(
        client.request(Command::Hello {}).await,
        Err(ClientError::InvalidRequest)
    ));
    assert!(matches!(
        client.request(Command::DescribeConfig {}).await,
        Err(ClientError::InvalidRequest)
    ));
    let request = call(&client, get());
    let frame = receive(&mut peer).await;
    assert_eq!(
        frame["id"], 3,
        "oversized local request reserves an ID but writes no frame"
    );
    send(&mut peer, json!({"version":1,"id":3,"error":{"code":"transient","message":"Rate limited","upstreamCode":429,"retryAfterSeconds":12.5,"fields":[]}})).await;
    let Err(ClientError::Remote(error)) = request.await.unwrap() else {
        panic!("Expected remote error")
    };
    assert_eq!(error.retry_after_seconds, Some(12.5));
    assert_eq!(error.upstream_code, Some(429));
    assert!(client.is_connected());
}

#[tokio::test]
async fn dropping_last_client_closes_both_transport_halves_and_releases_server_leases() {
    let config: HubConfig =
        serde_json::from_str(include_str!("../examples/simulated-observatory.json")).unwrap();
    let output = config.outputs[0].id;
    let instance = config.instance_id;
    let source = config.sources[0].id;
    let runtime = HubRuntime::build(
        config,
        &NativeRuntime {
            directory: "unused".into(),
            simulate: false,
            references: None,
        },
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let (stream, server) = tokio::io::duplex(MAX_FRAME_BYTES * 2);
    let task = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    let client = Client::from_stream(
        stream,
        instance,
        Duration::from_secs(1),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    client.request(Command::Connect { output }).await.unwrap();
    assert_eq!(runtime.source_snapshot(source).unwrap().lease_count, 1);
    let other = client.clone();
    drop(client);
    assert!(other.is_connected());
    drop(other);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.source_snapshot(source).unwrap().lease_count != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_host_identity_and_silent_handshake_never_create_a_client() {
    let (stream, mut peer) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        hello(&mut peer, Uuid::new_v4(), MAX_FRAME_BYTES).await;
    });
    assert!(matches!(
        Client::from_stream(
            stream,
            Uuid::new_v4(),
            Duration::from_secs(1),
            ClientLimits::default()
        )
        .await,
        Err(ClientError::Protocol)
    ));
    task.await.unwrap();
    let (stream, _peer) = tokio::io::duplex(4096);
    assert!(matches!(
        Client::from_stream(
            stream,
            Uuid::new_v4(),
            Duration::from_millis(20),
            ClientLimits::default()
        )
        .await,
        Err(ClientError::Timeout)
    ));
}
