//! Bounded local service lifetime. Ownership outlives client tasks and device drain.
use crate::{
    endpoint::{Endpoint, Listener},
    ipc::{
        Command, Limits, MAX_FRAME_BYTES, MAX_IN_FLIGHT, ProtocolError, Request, VERSION,
        read_frame, serve_service_stream,
    },
    runtime::HubRuntime,
    service::HubService,
    source::SourceError,
};
use regain_core::CancellationToken;
use serde::Deserialize;
use std::{future::Future, io, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, task::JoinSet};
use uuid::Uuid;

pub const MAX_CLIENTS: usize = 32;

#[derive(Debug)]
pub struct HostError {
    pub listener: Option<io::Error>,
    pub cleanup: Vec<(Uuid, SourceError)>,
    pub supervisor: Option<tokio::task::JoinError>,
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hub host stopped with a listener, source cleanup, or supervisor error")
    }
}
impl std::error::Error for HostError {}

/// Starts the supervisor immediately. Dropping the returned future requests
/// shutdown; it does not abort the supervisor or release the OS lock early.
/// The Tokio runtime must remain alive until source cleanup has completed.
pub fn serve(
    listener: Listener,
    runtime: Arc<HubRuntime>,
    limits: Limits,
    stop: CancellationToken,
) -> impl Future<Output = Result<(), HostError>> + Send {
    serve_service(listener, HubService::read_only(runtime), limits, stop)
}
pub fn serve_service(
    listener: Listener,
    service: Arc<HubService>,
    limits: Limits,
    stop: CancellationToken,
) -> impl Future<Output = Result<(), HostError>> + Send {
    let stop = stop.child_token();
    let guard = stop.clone().drop_guard();
    let task = tokio::spawn(supervise(listener, service, limits, stop));
    async move {
        let _guard = guard;
        task.await.map_err(|error| HostError {
            listener: None,
            cleanup: Vec::new(),
            supervisor: Some(error),
        })?
    }
}

async fn supervise(
    mut listener: Listener,
    service: Arc<HubService>,
    limits: Limits,
    stop: CancellationToken,
) -> Result<(), HostError> {
    let mut clients = JoinSet::new();
    let listener_error = loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break None,
            // Reap completed clients before admitting more, bounding both live
            // tasks and their retained results. A protocol failure is per client.
            _ = clients.join_next(), if !clients.is_empty() => {},
            result = listener.accept(), if clients.len() < MAX_CLIENTS => {
                match result {
                    Ok(stream) => { clients.spawn(serve_service_stream(stream, service.clone(), limits)); }
                    Err(error) => break Some(error),
                }
            }
        }
    };
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let cleanup = service.shutdown().await.err().unwrap_or_default();
    // Keep ownership even after all IPC streams have gone away: source actors
    // may still be completing an uncertain write or closing an owned connection.
    drop(listener);
    if listener_error.is_some() || !cleanup.is_empty() {
        Err(HostError {
            listener: listener_error,
            cleanup,
            supervisor: None,
        })
    } else {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    pub protocol_version: u16,
    pub instance_id: Uuid,
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub client_id: Uuid,
    pub max_frame_bytes: usize,
    pub max_in_flight: usize,
    pub operations: Vec<String>,
    pub capabilities: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloReply {
    version: u16,
    id: u64,
    result: Hello,
}

/// Readiness is a bounded protocol/identity check, not merely an open socket.
/// This probe owns no output connections and never retries a device command.
pub async fn probe(endpoint: &Endpoint, instance: Uuid, deadline: Duration) -> io::Result<Hello> {
    tokio::time::timeout(deadline, async {
        let mut stream = endpoint.connect(deadline).await?;
        let request = serde_json::to_vec(&Request {
            version: VERSION,
            id: 1,
            command: Command::Hello {},
        })
        .map_err(|_| incompatible())?;
        stream
            .write_all(&(request.len() as u32).to_le_bytes())
            .await?;
        stream.write_all(&request).await?;
        let bytes = read_frame(&mut stream, deadline)
            .await
            .map_err(|error| match error {
                ProtocolError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, "Hub readiness deadline expired")
                }
                _ => incompatible(),
            })?
            .ok_or_else(incompatible)?;
        let reply: HelloReply = serde_json::from_slice(&bytes).map_err(|_| incompatible())?;
        let hello = reply.result;
        if reply.version != VERSION
            || reply.id != 1
            || hello.protocol_version != VERSION
            || hello.instance_id != instance
            || hello.host_instance.is_nil()
            || hello.configuration_revision.is_nil()
            || hello.client_id.is_nil()
            || hello.max_frame_bytes == 0
            || hello.max_frame_bytes > MAX_FRAME_BYTES
            || hello.max_in_flight == 0
            || hello.max_in_flight > MAX_IN_FLIGHT
        {
            return Err(incompatible());
        }
        Ok(hello)
    })
    .await
    .unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Hub readiness deadline expired",
        ))
    })
}
fn incompatible() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Hub readiness response has an incompatible protocol or identity",
    )
}
