//! Versioned scalar hub IPC over a host-supplied, user-protected local stream.
//! No listener is opened here. Camera images require their separate contract.
use crate::{
    config::{HubConfig, WeatherMetric},
    credentials::{CredentialError, SecretAuthorization},
    description::describe_config,
    runtime::{ClientSession, HubRuntime},
    service::{HubService, ServiceClient, UpdateError},
    source::{ErrorKind, SourceError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};
use uuid::Uuid;

pub const VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 8;

#[derive(Clone, Copy)]
pub struct Limits {
    pub frame_timeout: Duration,
    pub operation_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            frame_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    Io,
    Malformed,
    FrameTooLarge,
    Timeout,
    Version,
    Handshake,
    RequestOrder,
    Overloaded,
}
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Io => "Local IPC transport failed",
            Self::Malformed => "Malformed hub IPC message",
            Self::FrameTooLarge => "Hub IPC frame exceeds the size limit",
            Self::Timeout => "Hub IPC frame deadline expired",
            Self::Version => "Unsupported hub IPC version",
            Self::Handshake => "Hub IPC requires an initial hello",
            Self::RequestOrder => "Hub IPC request IDs must increase",
            Self::Overloaded => "Too many concurrent hub IPC requests",
        })
    }
}
impl std::error::Error for ProtocolError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub id: u64,
    pub command: Command,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Command {
    Hello {},
    DescribeConfig {},
    GetConfig {},
    ValidateConfig {
        candidate: Box<HubConfig>,
    },
    ApplyConfig {
        #[serde(rename = "expectedRevision")]
        expected_revision: Uuid,
        candidate: Box<HubConfig>,
    },
    HostStatus {},
    CreateCredential {
        authorization: SecretAuthorization,
    },
    CredentialStatus {
        reference: String,
    },
    DeleteCredential {
        reference: String,
    },
    ListDevices {},
    SourceStatus {
        source: Uuid,
    },
    InspectSource {
        source: Uuid,
        start: u32,
        limit: u32,
    },
    UpdateSimulation {
        source: Uuid,
        update: crate::simulated::SimulationUpdate,
    },
    Connect {
        output: Uuid,
    },
    Disconnect {
        output: Uuid,
    },
    Get {
        output: Uuid,
        property: Get,
    },
    Put {
        output: Uuid,
        property: Put,
    },
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "member", rename_all = "camelCase", deny_unknown_fields)]
pub enum Get {
    Connected {},
    IsSafe {},
    SafetyStatus {},
    MaxSwitch {},
    GetSwitch { id: u32 },
    GetSwitchValue { id: u32 },
    GetSwitchName { id: u32 },
    GetSwitchDescription { id: u32 },
    CanWrite { id: u32 },
    MinSwitchValue { id: u32 },
    MaxSwitchValue { id: u32 },
    SwitchStep { id: u32 },
    Measurement { metric: WeatherMetric },
    TimeSinceLastUpdate { sensor: String },
    AveragePeriod {},
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "member", rename_all = "camelCase", deny_unknown_fields)]
pub enum Put {
    SetSwitch { id: u32, state: bool },
    SetSwitchValue { id: u32, value: f64 },
    AveragePeriod { hours: f64 },
    Refresh {},
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcError {
    pub code: &'static str,
    pub message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<crate::parameters::FieldError>,
}
impl From<SourceError> for RpcError {
    fn from(error: SourceError) -> Self {
        Self {
            code: match error.kind {
                ErrorKind::Connecting => "connecting",
                ErrorKind::Disconnected => "disconnected",
                ErrorKind::Busy => "busy",
                ErrorKind::Transient => "transient",
                ErrorKind::Permanent => "permanent",
                ErrorKind::Uncertain => "uncertain",
                ErrorKind::Unsupported => "unsupported",
                ErrorKind::InvalidValue => "invalidValue",
                ErrorKind::Unavailable => "unavailable",
            },
            message: error.message,
            upstream_code: error.upstream_code,
            retry_after_seconds: error.retry_after.map(|delay| delay.as_secs_f64()),
            fields: Vec::new(),
        }
    }
}
impl From<UpdateError> for RpcError {
    fn from(error: UpdateError) -> Self {
        let (code, message) = match &error {
            UpdateError::Invalid(_) => ("invalidConfig", "Configuration cannot be applied"),
            UpdateError::Conflict => (
                "revisionConflict",
                "Configuration revision changed; reload before editing",
            ),
            UpdateError::Connected => (
                "connected",
                "Disconnect all outputs and finish pending operations before applying configuration",
            ),
            UpdateError::Busy => ("busy", "Another configuration operation is in progress"),
            UpdateError::Io => (
                "persistence",
                "Configuration file could not be replaced; previous configuration is retained",
            ),
            UpdateError::Unsupported => (
                "unsupported",
                "This runtime does not support configuration updates",
            ),
            UpdateError::Stopped => (
                "unavailable",
                "Hub is stopped or blocked; inspect hostStatus",
            ),
            UpdateError::Task => (
                "uncertain",
                "Configuration update outcome is uncertain; reload configuration and inspect hostStatus",
            ),
        };
        Self {
            code,
            message,
            upstream_code: None,
            retry_after_seconds: None,
            fields: match error {
                UpdateError::Invalid(fields) => fields,
                _ => Vec::new(),
            },
        }
    }
}
impl From<CredentialError> for RpcError {
    fn from(error: CredentialError) -> Self {
        let (code, message) = match error {
            CredentialError::Invalid => (
                "invalidValue",
                "Invalid authorization value or credential reference",
            ),
            CredentialError::Missing => ("unavailable", "Credential is missing"),
            CredentialError::Unavailable => (
                "unavailable",
                "Protected credential storage is unavailable or invalid",
            ),
            CredentialError::InUse => (
                "inUse",
                "Remove this credential reference from configuration before deleting it",
            ),
            CredentialError::Busy => (
                "busy",
                "Another configuration or credential operation is in progress",
            ),
            CredentialError::Unsupported => (
                "unsupported",
                "This host has no credential storage provider",
            ),
        };
        Self {
            code,
            message,
            upstream_code: None,
            retry_after_seconds: None,
            fields: Vec::new(),
        }
    }
}
#[derive(Serialize)]
struct Response {
    version: u16,
    id: u64,
    #[serde(flatten)]
    outcome: Outcome,
}
#[derive(Serialize)]
#[serde(untagged)]
enum Outcome {
    Success { result: Value },
    Failure { error: RpcError },
}
impl Response {
    fn new(id: u64, result: Result<Value, RpcError>) -> Self {
        Self {
            version: VERSION,
            id,
            outcome: match result {
                Ok(result) => Outcome::Success { result },
                Err(error) => Outcome::Failure { error },
            },
        }
    }
}

/// A stalled command cannot stop EOF detection or cached reads. Each stream
/// has one host-created client, one bounded reader, and at most eight operations.
/// Frames/operations have separate deadlines. Dropping this future closes the
/// client even when a write was dispatched; that write is never replayed here.
pub async fn serve_stream<T>(
    stream: T,
    runtime: Arc<HubRuntime>,
    limits: Limits,
) -> Result<(), ProtocolError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_service_stream(stream, HubService::read_only(runtime), limits).await
}
pub async fn serve_service_stream<T>(
    stream: T,
    service: Arc<HubService>,
    limits: Limits,
) -> Result<(), ProtocolError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if limits.frame_timeout.is_zero() || limits.operation_timeout.is_zero() {
        return Err(ProtocolError::Timeout);
    }
    let client = Arc::new(ServiceClient::default());
    let _close = CloseClient(client.clone());
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (frames, mut incoming) = mpsc::channel(1);
    let mut reading = JoinSet::new();
    reading.spawn(async move {
        let mut first = true;
        loop {
            let frame = if first {
                timeout(
                    limits.frame_timeout,
                    read_frame(&mut reader, limits.frame_timeout),
                )
                .await
                .unwrap_or(Err(ProtocolError::Timeout))
            } else {
                read_frame(&mut reader, limits.frame_timeout).await
            };
            first = false;
            let finished = !matches!(&frame, Ok(Some(_)));
            if frames.send(frame).await.is_err() || finished {
                break;
            }
        }
    });
    let mut tasks = JoinSet::new();
    let mut last_id = 0;
    let mut greeted = false;
    loop {
        tokio::select! {
            frame = incoming.recv() => {
                let Some(bytes) = frame.ok_or(ProtocolError::Io)?? else { return Ok(()); };
                let request: Request = serde_json::from_slice(&bytes).map_err(|_| ProtocolError::Malformed)?;
                if request.version != VERSION { return Err(ProtocolError::Version); }
                if request.id <= last_id { return Err(ProtocolError::RequestOrder); }
                last_id = request.id;
                if !greeted {
                    if !matches!(request.command, Command::Hello {}) { return Err(ProtocolError::Handshake); }
                    greeted = true;
                    let mut operations = vec!["describeConfig","getConfig","validateConfig","listDevices","sourceStatus","inspectSource","updateSimulation","connect","disconnect","get","put","hostStatus"];
                    if service.can_apply() { operations.push("applyConfig"); }
                    if service.credential_description().is_some() { operations.extend(["createCredential", "credentialStatus", "deleteCredential"]); }
                    let hello = json!({"protocolVersion":VERSION, "instanceId":service.instance_id(),
                        "hostInstance":service.host_id(), "configurationRevision":service.configuration().revision, "clientId":client.id(),
                        "maxFrameBytes":MAX_FRAME_BYTES, "maxInFlight":MAX_IN_FLIGHT,
                        "operations":operations,
                        "capabilities":["switchOutputs","safetyOutputs","weatherOutputs"]});
                    write_response(&mut writer, Response::new(request.id, Ok(hello)), limits.frame_timeout).await?;
                    continue;
                }
                if matches!(request.command, Command::Hello {}) { return Err(ProtocolError::Handshake); }
                if tasks.len() >= MAX_IN_FLIGHT { return Err(ProtocolError::Overloaded); }
                let service = service.clone();
                let client = client.clone();
                let mut operation = Box::pin(async move {
                    let write = matches!(request.command, Command::Put { .. } | Command::ApplyConfig { .. } | Command::CreateCredential { .. } | Command::DeleteCredential { .. } | Command::UpdateSimulation { .. });
                    let result = timeout(limits.operation_timeout, dispatch_service(&service, &client, request.command)).await
                        .unwrap_or_else(|_| Err(if write { SourceError::uncertain().into() } else {
                            RpcError { code:"timeout", message:"Hub operation deadline expired", upstream_code:None, retry_after_seconds:None, fields:Vec::new() }
                        }));
                    Response::new(request.id, result)
                });
                // Start requests in arrival order, without waiting for their
                // I/O. In particular Disconnect must see an earlier Connect's
                // reservation even if the worker task has not been scheduled.
                match std::future::poll_fn(|cx| std::task::Poll::Ready(operation.as_mut().poll(cx))).await {
                    std::task::Poll::Ready(response) => write_response(&mut writer, response, limits.frame_timeout).await?,
                    std::task::Poll::Pending => { tasks.spawn(operation); }
                }
            }
            response = tasks.join_next(), if !tasks.is_empty() => {
                let response = response.ok_or(ProtocolError::Io)?.map_err(|_| ProtocolError::Io)?;
                write_response(&mut writer, response, limits.frame_timeout).await?;
            }
        }
    }
}
struct CloseClient(Arc<ServiceClient>);
impl Drop for CloseClient {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn dispatch_service(
    service: &Arc<HubService>,
    client: &ServiceClient,
    command: Command,
) -> Result<Value, RpcError> {
    match command {
        Command::DescribeConfig {} => {
            let mut description = describe_config(&[
                "nativeSources",
                "alpacaSources",
                "writeReadout",
                "simulation",
            ]);
            description["credentialStorage"] =
                service.credential_description().unwrap_or(Value::Null);
            description["capabilityInspection"] = crate::capabilities::description();
            description["simulationControl"] = crate::simulated::description();
            Ok(description)
        }
        Command::CreateCredential { authorization } => {
            Ok(json!(service.create_credential(authorization).await?))
        }
        Command::CredentialStatus { reference } => {
            Ok(json!(service.credential_status(reference).await?))
        }
        Command::DeleteCredential { reference } => {
            Ok(json!(service.delete_credential(reference).await?))
        }
        Command::GetConfig {} => Ok(json!(service.configuration())),
        Command::HostStatus {} => Ok(json!(service.status())),
        Command::ApplyConfig {
            expected_revision,
            candidate,
        } => Ok(json!(service.apply(expected_revision, *candidate).await?)),
        command => {
            let runtime = service.runtime()?;
            let client = client.bind(&runtime)?;
            dispatch(&runtime, &client, command).await
        }
    }
}

async fn dispatch(
    runtime: &HubRuntime,
    client: &Arc<ClientSession>,
    command: Command,
) -> Result<Value, RpcError> {
    let target = match &command {
        Command::Connect { output }
        | Command::Disconnect { output }
        | Command::Get { output, .. }
        | Command::Put { output, .. } => Some(*output),
        _ => None,
    };
    if target.is_some_and(|id| !runtime.contains_output(id)) {
        return Err(SourceError::new(ErrorKind::InvalidValue, "Unknown output ID").into());
    }
    Ok(match command {
        Command::ApplyConfig { .. }
        | Command::HostStatus {}
        | Command::DescribeConfig {}
        | Command::CreateCredential { .. }
        | Command::CredentialStatus { .. }
        | Command::DeleteCredential { .. } => {
            unreachable!("Handled by service dispatcher")
        }
        Command::Hello {} => {
            return Err(RpcError {
                code: "invalidRequest",
                message: "Hello is only valid as the first request",
                upstream_code: None,
                retry_after_seconds: None,
                fields: Vec::new(),
            });
        }
        Command::GetConfig {} => json!(runtime.configuration()),
        Command::ValidateConfig { candidate } => {
            // Persisted-schema/relationship validation only. Durable apply must
            // additionally prepare adapters, check capabilities and revision.
            let errors = crate::factory::source_plans(&candidate)
                .err()
                .unwrap_or_default();
            json!({"valid":errors.is_empty(), "errors":errors, "scope":"configuration"})
        }
        Command::ListDevices {} => json!(runtime.outputs()),
        Command::SourceStatus { source } => json!(runtime.source_snapshot(source)?),
        Command::UpdateSimulation { source, update } => {
            json!(runtime.update_simulation(source, update).await?)
        }
        Command::InspectSource {
            source,
            start,
            limit,
        } => json!(runtime.inspect_source(source, start, limit).await?),
        Command::Connect { output } => {
            client.connect(output).await?;
            Value::Null
        }
        Command::Disconnect { output } => {
            client.disconnect(output);
            Value::Null
        }
        Command::Get {
            output,
            property: Get::Connected {},
        } => json!(client.connection(output).is_ok()),
        Command::Get { output, property } => {
            let connection = client.connection(output)?;
            match property {
                Get::Connected {} => unreachable!(),
                Get::IsSafe {} => json!(connection.safety()?.snapshot().is_safe),
                Get::SafetyStatus {} => json!(connection.safety()?.snapshot()),
                Get::MaxSwitch {} => json!(connection.switch_definition()?.max_switch()),
                Get::GetSwitch { id } => json!(connection.switch()?.state(id)?),
                Get::GetSwitchValue { id } => json!(connection.switch()?.value(id)?),
                Get::GetSwitchName { id } => json!(connection.switch_definition()?.name(id)?),
                Get::GetSwitchDescription { id } => {
                    let channel = connection.switch_definition()?.channel(id)?;
                    json!(
                        channel
                            .map(|c| format!("{}; source {}", c.units, c.readout.source()))
                            .unwrap_or_else(|| "Removed channel".into())
                    )
                }
                Get::CanWrite { id } => json!(connection.switch()?.can_write(id).await?),
                Get::MinSwitchValue { id } => json!(
                    connection
                        .switch_definition()?
                        .channel(id)?
                        .map_or(0.0, |c| c.minimum)
                ),
                Get::MaxSwitchValue { id } => json!(
                    connection
                        .switch_definition()?
                        .channel(id)?
                        .map_or(1.0, |c| c.maximum)
                ),
                Get::SwitchStep { id } => json!(
                    connection
                        .switch_definition()?
                        .channel(id)?
                        .map_or(1.0, |c| c.step)
                ),
                Get::Measurement { metric } => json!(connection.weather()?.read(metric)?),
                Get::TimeSinceLastUpdate { sensor } => {
                    json!(connection.weather()?.time_since_last_update(&sensor)?)
                }
                Get::AveragePeriod {} => json!(connection.weather()?.average_period_hours()),
            }
        }
        Command::Put { output, property } => {
            let connection = client.connection(output)?;
            match property {
                Put::SetSwitch { id, state } => connection.switch()?.set_state(id, state).await?,
                Put::SetSwitchValue { id, value } => {
                    connection.switch()?.set_value(id, value).await?
                }
                Put::AveragePeriod { hours } => {
                    connection.weather()?.set_average_period_hours(hours)?
                }
                Put::Refresh {} => connection.weather()?.refresh().await?,
            }
            Value::Null
        }
    })
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    deadline: Duration,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, ProtocolError> {
    let mut header = [0u8; 4];
    if reader
        .read(&mut header[..1])
        .await
        .map_err(|_| ProtocolError::Io)?
        == 0
    {
        return Ok(None);
    }
    timeout(deadline, async {
        reader
            .read_exact(&mut header[1..])
            .await
            .map_err(|_| ProtocolError::Malformed)?;
        let size = u32::from_le_bytes(header) as usize;
        if size == 0 {
            return Err(ProtocolError::Malformed);
        }
        if size > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        // Clear complete, queued and partially read secret-bearing frames when
        // the stream closes, parsing fails, or a frame deadline expires.
        let mut bytes = zeroize::Zeroizing::new(vec![0; size]);
        reader
            .read_exact(&mut bytes)
            .await
            .map_err(|_| ProtocolError::Malformed)?;
        Ok(Some(bytes))
    })
    .await
    .unwrap_or(Err(ProtocolError::Timeout))
}

struct BoundedBytes(Vec<u8>);
impl io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES - self.0.len() {
            return Err(io::Error::other("IPC response limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: Response,
    deadline: Duration,
) -> Result<(), ProtocolError> {
    let mut bytes = BoundedBytes(Vec::new());
    if serde_json::to_writer(&mut bytes, &response).is_err() {
        bytes.0.clear();
        let error = Response::new(
            response.id,
            Err(RpcError {
                code: "responseTooLarge",
                message: "Hub response exceeds the frame limit",
                upstream_code: None,
                retry_after_seconds: None,
                fields: Vec::new(),
            }),
        );
        serde_json::to_writer(&mut bytes, &error).map_err(|_| ProtocolError::FrameTooLarge)?;
    }
    timeout(deadline, async {
        writer
            .write_all(&(bytes.0.len() as u32).to_le_bytes())
            .await
            .map_err(|_| ProtocolError::Io)?;
        writer
            .write_all(&bytes.0)
            .await
            .map_err(|_| ProtocolError::Io)?;
        writer.flush().await.map_err(|_| ProtocolError::Io)
    })
    .await
    .unwrap_or(Err(ProtocolError::Timeout))
}
