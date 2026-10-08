//! Bounded, multiplexed frontend IPC. A failed connection is terminal: callers
//! explicitly attach again and reacquire output leases; no command is replayed.
use crate::{
    endpoint::Endpoint,
    host::{Hello, handshake},
    ipc::{Command, MAX_FRAME_BYTES, MAX_IN_FLIGHT, ProtocolError, Request, VERSION, read_frame},
    parameters::FieldError,
};
use regain_core::CancellationToken;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
    pub upstream_code: Option<i32>,
    pub retry_after_seconds: Option<f64>,
    #[serde(default)]
    pub fields: Vec<FieldError>,
}
#[derive(Clone, Debug)]
pub enum ClientError {
    Disconnected,
    Timeout,
    Protocol,
    Busy,
    InvalidRequest,
    /// Transport failure after a mutating request was handed to the writer.
    Uncertain,
    Remote(RemoteError),
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never include request bytes or untrusted response text in diagnostics.
        f.write_str(match self {
            Self::Disconnected => "Hub connection is closed; attach again",
            Self::Timeout => "Hub request deadline expired",
            Self::Protocol => "Hub response has an invalid protocol or identity",
            Self::Busy => "Hub client request limit reached",
            Self::InvalidRequest => "Hub request exceeds its limits or is not advertised",
            Self::Uncertain => {
                "Hub operation may have completed; reconcile state before issuing another command"
            }
            Self::Remote(_) => "Hub rejected the request; inspect its structured error",
        })
    }
}
impl std::error::Error for ClientError {}

#[derive(Clone, Copy)]
pub struct ClientLimits {
    pub frame_timeout: Duration,
    /// Includes queueing, writing, and waiting for the reply. Closing a stalled
    /// connection releases this client's leases; it never cancels physical motion.
    pub request_timeout: Duration,
}
impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            frame_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(35),
        }
    }
}
struct Incoming {
    command: Command,
    deadline: Instant,
    mutate: bool,
    reply: oneshot::Sender<Result<Value, ClientError>>,
    permit: OwnedSemaphorePermit,
}
struct Pending {
    deadline: Instant,
    mutate: bool,
    reply: oneshot::Sender<Result<Value, ClientError>>,
    _permit: OwnedSemaphorePermit,
}
struct Inner {
    hello: Hello,
    limits: ClientLimits,
    send: mpsc::Sender<Incoming>,
    capacity: Arc<Semaphore>,
    stop: CancellationToken,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
#[derive(Clone)]
pub struct Client(Arc<Inner>);
impl Client {
    /// Endpoint checks OS ownership/permissions before the protocol handshake.
    pub async fn connect(
        endpoint: &Endpoint,
        instance: Uuid,
        deadline: Duration,
        limits: ClientLimits,
    ) -> Result<Self, ClientError> {
        timeout(deadline, async {
            let stream = endpoint.connect(deadline).await.map_err(connection_error)?;
            Self::from_stream(stream, instance, deadline, limits).await
        })
        .await
        .unwrap_or(Err(ClientError::Timeout))
    }
    /// The caller must supply an authenticated/user-protected stream. This also
    /// permits deterministic in-memory transport fault tests.
    pub async fn from_stream<T>(
        mut stream: T,
        instance: Uuid,
        deadline: Duration,
        limits: ClientLimits,
    ) -> Result<Self, ClientError>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if limits.frame_timeout.is_zero()
            || limits.request_timeout.is_zero()
            || deadline.is_zero()
            || deadline > Duration::from_secs(300)
            || instance.is_nil()
            || limits.frame_timeout > Duration::from_secs(60)
            || limits.request_timeout > Duration::from_secs(300)
        {
            return Err(ClientError::InvalidRequest);
        }
        let hello = handshake(&mut stream, instance, deadline)
            .await
            .map_err(connection_error)?;
        let capacity = Arc::new(Semaphore::new(hello.max_in_flight));
        let (send, receive) = mpsc::channel(hello.max_in_flight);
        let stop = CancellationToken::new();
        tokio::spawn(pump(
            stream,
            receive,
            stop.clone(),
            limits,
            hello.max_frame_bytes,
        ));
        Ok(Self(Arc::new(Inner {
            hello,
            limits,
            send,
            capacity,
            stop,
        })))
    }
    pub fn hello(&self) -> &Hello {
        &self.0.hello
    }
    pub fn is_connected(&self) -> bool {
        !self.0.stop.is_cancelled()
    }
    pub fn close(&self) {
        self.0.stop.cancel();
    }
    pub async fn closed(&self) {
        self.0.stop.cancelled().await;
    }
    /// Cancellation before dispatch skips the request. Once dispatched, the
    /// client retains its permit until reply/deadline, even if this future drops.
    pub async fn request(&self, command: Command) -> Result<Value, ClientError> {
        self.request_with_timeout(command, self.0.limits.request_timeout)
            .await
    }
    /// Read inert, revision-fenced camera controller metadata. No equipment lease
    /// is acquired. Callers must obtain a new descriptor after reattachment.
    pub async fn camera_timing(
        &self,
        output: Uuid,
    ) -> Result<crate::camera::ipc_timing::CameraOperationTiming, ClientError> {
        if !self
            .hello()
            .capabilities
            .iter()
            .any(|value| value == "cameraOperationTiming")
        {
            return Err(ClientError::InvalidRequest);
        }
        let value = self
            .request(Command::CameraTiming {
                output,
                expected_revision: self.hello().configuration_revision,
            })
            .await?;
        let timing: crate::camera::ipc_timing::CameraOperationTiming =
            serde_json::from_value(value).map_err(|_| ClientError::Protocol)?;
        if !timing.matches(self.hello(), output) {
            return Err(ClientError::Protocol);
        }
        Ok(timing)
    }
    pub async fn camera_capture_timing(
        &self,
        output: Uuid,
        seconds: f64,
    ) -> Result<crate::camera::ipc_timing::CameraCaptureTiming, ClientError> {
        if output.is_nil()
            || !seconds.is_finite()
            || seconds < 0.
            || !self
                .hello()
                .capabilities
                .iter()
                .any(|value| value == "cameraCaptureTiming")
        {
            return Err(ClientError::InvalidRequest);
        }
        let value = self
            .request(Command::CameraCaptureTiming {
                output,
                expected_revision: self.hello().configuration_revision,
                duration_seconds: seconds,
            })
            .await?;
        let timing: crate::camera::ipc_timing::CameraCaptureTiming =
            serde_json::from_value(value).map_err(|_| ClientError::Protocol)?;
        if !timing.matches(self.hello(), output, seconds) {
            return Err(ClientError::Protocol);
        }
        Ok(timing)
    }
    /// Only acknowledged camera operations may use the negotiated bound. Reads,
    /// image transfers and asynchronous connection admission keep their own limits.
    pub async fn request_camera(
        &self,
        timing: &crate::camera::ipc_timing::CameraOperationTiming,
        command: Command,
    ) -> Result<Value, ClientError> {
        let (output, kind) =
            crate::camera::ipc_timing::operation(&command).ok_or(ClientError::InvalidRequest)?;
        if !timing.matches(self.hello(), output)
            || !self
                .hello()
                .capabilities
                .iter()
                .any(|value| value == "cameraOperationTiming")
        {
            return Err(ClientError::InvalidRequest);
        }
        let deadline = timing.timeout(kind) + self.0.limits.frame_timeout * 2;
        self.request_with_timeout(
            Command::CameraControl {
                expected_revision: timing.configuration_revision,
                command: Box::new(command),
            },
            deadline.max(self.0.limits.request_timeout),
        )
        .await
    }
    async fn request_with_timeout(
        &self,
        command: Command,
        deadline: Duration,
    ) -> Result<Value, ClientError> {
        if !self.is_connected() {
            return Err(ClientError::Disconnected);
        }
        if matches!(command, Command::Hello {} | Command::CameraImage { .. })
            || !self
                .0
                .hello
                .operations
                .iter()
                .any(|op| op == operation(&command))
        {
            return Err(ClientError::InvalidRequest);
        }
        let permit = self
            .0
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| ClientError::Busy)?;
        let mutate = mutating(&command);
        let (reply, response) = oneshot::channel();
        self.0
            .send
            .try_send(Incoming {
                command,
                deadline: Instant::now() + deadline,
                mutate,
                reply,
                permit,
            })
            .map_err(|_| ClientError::Disconnected)?;
        response.await.unwrap_or(Err(if mutate {
            ClientError::Uncertain
        } else {
            ClientError::Disconnected
        }))
    }
}
pub(crate) fn connection_error(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::TimedOut => ClientError::Timeout,
        io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied => ClientError::Protocol,
        _ => ClientError::Disconnected,
    }
}
fn mutating(command: &Command) -> bool {
    matches!(
        command,
        Command::Put { .. }
            | Command::CameraControl { .. }
            | Command::ApplyConfig { .. }
            | Command::CreateCredential { .. }
            | Command::DeleteCredential { .. }
            | Command::UpdateSimulation { .. }
            | Command::StartCameraGroup { .. }
            | Command::StartFocuserGroup { .. }
            | Command::CancelCameraGroup { .. }
            | Command::CancelFocuserGroup { .. }
            | Command::Connect { .. }
            | Command::ChangeConnection { .. }
            | Command::Disconnect { .. }
    )
}
fn operation(command: &Command) -> &'static str {
    match command {
        Command::Hello {} => "hello",
        Command::CameraImage { .. } => "cameraImage",
        Command::CameraGroupImage { .. } => "cameraGroupImage",
        Command::CameraTiming { .. } => "cameraTiming",
        Command::CameraCaptureTiming { .. } => "cameraCaptureTiming",
        Command::CameraControl { .. } => "cameraControl",
        Command::DescribeConfig {} => "describeConfig",
        Command::StartCameraGroup { .. } => "startCameraGroup",
        Command::CameraGroupStatus { .. } => "cameraGroupStatus",
        Command::CancelCameraGroup { .. } => "cancelCameraGroup",
        Command::StartFocuserGroup { .. } => "startFocuserGroup",
        Command::FocuserGroupStatus { .. } => "focuserGroupStatus",
        Command::CancelFocuserGroup { .. } => "cancelFocuserGroup",
        Command::GetConfig {} => "getConfig",
        Command::DiscoverAlpaca { .. } => "discoverAlpaca",
        Command::SearchAlpaca { .. } => "searchAlpaca",
        Command::ValidateConfig { .. } => "validateConfig",
        Command::ApplyConfig { .. } => "applyConfig",
        Command::HostStatus {} => "hostStatus",
        Command::CreateCredential { .. } => "createCredential",
        Command::CredentialStatus { .. } => "credentialStatus",
        Command::DeleteCredential { .. } => "deleteCredential",
        Command::ListDevices {} => "listDevices",
        Command::SourceStatus { .. } => "sourceStatus",
        Command::OutputStatus { .. } => "outputStatus",
        Command::InspectSource { .. } => "inspectSource",
        Command::UpdateSimulation { .. } => "updateSimulation",
        Command::Connect { .. } => "connect",
        Command::ChangeConnection { .. } => "changeConnection",
        Command::Disconnect { .. } => "disconnect",
        Command::Get { .. } => "get",
        Command::Put { .. } => "put",
    }
}
struct Bytes {
    data: Zeroizing<Vec<u8>>,
    limit: usize,
}
impl io::Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.data.len() {
            return Err(io::Error::other("Request limit"));
        }
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode(request: &Request, limit: usize) -> Result<Zeroizing<Vec<u8>>, ClientError> {
    let mut bytes = Bytes {
        data: Zeroizing::new(Vec::new()),
        limit,
    };
    serde_json::to_writer(&mut bytes, request).map_err(|_| ClientError::InvalidRequest)?;
    Ok(bytes.data)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(u64, Result<Value, ClientError>), ClientError> {
    // Struct parsing rejects duplicate IDs/results. Presence is separate from
    // null, because successful void replies legitimately carry a null result.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        version: u16,
        id: u64,
        #[serde(default, deserialize_with = "present")]
        result: Option<Value>,
        #[serde(default, deserialize_with = "present")]
        error: Option<Value>,
    }
    fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        Value::deserialize(d).map(Some)
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| ClientError::Protocol)?;
    if reply.version != VERSION || reply.id <= 1 {
        return Err(ClientError::Protocol);
    }
    let result = match (reply.result, reply.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => {
            let error: RemoteError =
                serde_json::from_value(error).map_err(|_| ClientError::Protocol)?;
            if error
                .retry_after_seconds
                .is_some_and(|s| !s.is_finite() || s < 0.0)
            {
                return Err(ClientError::Protocol);
            }
            Err(ClientError::Remote(error))
        }
        _ => return Err(ClientError::Protocol),
    };
    Ok((reply.id, result))
}

async fn pump<T>(
    stream: T,
    mut incoming: mpsc::Receiver<Incoming>,
    stop: CancellationToken,
    limits: ClientLimits,
    max_frame: usize,
) where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let guard = stop.clone().drop_guard();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (frames, mut replies) = mpsc::channel(1);
    let mut reading = JoinSet::new();
    reading.spawn(async move {
        loop {
            let frame = read_frame(&mut reader, limits.frame_timeout).await;
            let done = !matches!(&frame, Ok(Some(_)));
            if frames.send(frame).await.is_err() || done {
                break;
            }
        }
    });
    let mut pending = BTreeMap::<u64, Pending>::new();
    let mut next_id = 2u64;
    let failure = loop {
        let deadline = pending.values().map(|p| p.deadline).min();
        tokio::select! {
            biased;
            _ = stop.cancelled() => break ClientError::Disconnected,
            _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => break ClientError::Timeout,
            frame = replies.recv() => {
                let bytes = match frame {
                    Some(Ok(Some(bytes))) if bytes.len() <= max_frame => bytes,
                    Some(Ok(None) | Err(ProtocolError::Io)) | None => break ClientError::Disconnected,
                    Some(Err(ProtocolError::Timeout)) => break ClientError::Timeout,
                    _ => break ClientError::Protocol,
                };
                let (id, result) = match decode(&bytes) { Ok(value) => value, Err(e) => break e };
                let Some(request) = pending.remove(&id) else { break ClientError::Protocol; };
                let _ = request.reply.send(result);
            },
            request = incoming.recv() => {
                let Some(request) = request else { break ClientError::Disconnected; };
                if request.reply.is_closed() { continue; }
                if request.deadline <= Instant::now() { let _ = request.reply.send(Err(ClientError::Timeout)); continue; }
                let id = next_id;
                let Some(next) = id.checked_add(1) else { let _ = request.reply.send(Err(ClientError::Disconnected)); break ClientError::Protocol; };
                next_id = next;
                let bytes = match encode(&Request { version: VERSION, id, command: request.command }, max_frame) {
                    Ok(bytes) => bytes,
                    Err(error) => { let _ = request.reply.send(Err(error)); continue; }
                };
                let frame_deadline = request.deadline.min(Instant::now() + limits.frame_timeout);
                pending.insert(id, Pending { deadline: request.deadline, mutate: request.mutate, reply: request.reply, _permit: request.permit });
                let written = tokio::select! {
                    biased;
                    _ = stop.cancelled() => break ClientError::Disconnected,
                    result = timeout_at(frame_deadline, async {
                        writer.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
                        writer.write_all(&bytes).await?; writer.flush().await
                    }) => result,
                };
                match written { Ok(Ok(())) => {}, Ok(Err(_)) => break ClientError::Disconnected, Err(_) => break ClientError::Timeout }
            }
        }
        debug_assert!(pending.len() <= MAX_IN_FLIGHT && max_frame <= MAX_FRAME_BYTES);
    };
    // Closing releases all stream-owned leases, including requests whose callers
    // cancelled. Never reconnect/replay, and never abandon the read half.
    reading.abort_all();
    while reading.join_next().await.is_some() {}
    drop(writer);
    incoming.close();
    for (_, request) in pending {
        let _ = request.reply.send(Err(if request.mutate {
            ClientError::Uncertain
        } else {
            failure.clone()
        }));
    }
    while let Some(request) = incoming.recv().await {
        let _ = request.reply.send(Err(ClientError::Disconnected));
    }
    drop(guard);
}
