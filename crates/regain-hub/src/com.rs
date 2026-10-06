//! Private Windows COM workers. No COM object, STA or blocking vendor call lives
//! in the Rust host; cancellation retires the worker through shared transport.
use crate::{
    config::{Bitness, ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
    native::NativeRuntime,
    sampling::{PropertyPoll, SampleRequest},
    source::{
        Backend, BackendFuture, ConnectionInfo, ConnectionMethod, ErrorKind, SampleBatch,
        SourceError, Values,
    },
};
use regain_core::accessory::{AccessoryError, AccessoryWorker};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{process::Command, time::Instant};

pub fn available_architectures(runtime: &NativeRuntime) -> Vec<Bitness> {
    if !cfg!(windows) {
        return Vec::new();
    }
    [Bitness::X86, Bitness::X64]
        .into_iter()
        .filter(|b| worker_path(runtime, *b).is_file())
        .collect()
}
fn architecture(bitness: Bitness) -> &'static str {
    match bitness {
        Bitness::X86 => "x86",
        Bitness::X64 => "x64",
    }
}
fn worker_path(runtime: &NativeRuntime, bitness: Bitness) -> PathBuf {
    runtime
        .directory
        .join("hub-ascom")
        .join(architecture(bitness))
        .join("Regain.Hub.ASCOM.exe")
}

pub struct ComBackend {
    path: PathBuf,
    prog_id: String,
    denied_classes: Vec<uuid::Uuid>,
    device: DeviceType,
    bitness: Bitness,
    policy: ConnectionPolicy,
    deadline: Duration,
    connection_deadline: Duration,
    connecting_since: Option<Instant>,
    polling: PropertyPoll,
    worker: Option<AccessoryWorker>,
    request_id: i64,
    info: Option<ConnectionInfo>,
    ready: bool,
    connection_uncertain: bool,
    disconnecting: bool,
}
impl ComBackend {
    /// Configuration preparation checks helper availability, without driver
    /// activation, hardware discovery, or COM registry/connection probing.
    pub fn new(
        config: &SourceConfig,
        runtime: &NativeRuntime,
        samples: Vec<SampleRequest>,
    ) -> Result<Self, SourceError> {
        let SourceBackend::Com {
            prog_id,
            device_type,
            bitness,
            connection_policy,
        } = &config.backend
        else {
            return Err(invalid());
        };
        if !cfg!(windows) {
            return Err(unsupported(
                "COM sources require Windows; use an exported Alpaca source on other systems",
            ));
        }
        if !matches!(
            device_type,
            DeviceType::Switch
                | DeviceType::SafetyMonitor
                | DeviceType::ObservingConditions
                | DeviceType::Focuser
                | DeviceType::Rotator
        ) {
            return Err(unsupported(
                "This COM worker does not support the selected device class yet",
            ));
        }
        if prog_id.trim().is_empty()
            || prog_id.trim() != prog_id
            || prog_id.chars().count() > 200
            || !prog_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            || !config.polling.validate_timing().is_empty()
        {
            return Err(invalid());
        }
        let path = worker_path(runtime, *bitness);
        if !path.is_file() {
            return Err(unsupported(
                "The selected COM worker architecture is not installed",
            ));
        }
        Ok(Self {
            path,
            prog_id: prog_id.clone(),
            denied_classes: Vec::new(),
            device: *device_type,
            bitness: *bitness,
            policy: *connection_policy,
            deadline: Duration::from_secs_f64(config.polling.request_timeout_seconds),
            connection_deadline: Duration::from_secs_f64(config.polling.connection_timeout_seconds),
            connecting_since: None,
            polling: PropertyPoll::new(*device_type, samples, config.polling.attempts_per_cycle)?,
            worker: None,
            request_id: 0,
            info: None,
            ready: false,
            connection_uncertain: false,
            disconnecting: false,
        })
    }
    pub(crate) fn exclude_exports(&mut self, classes: Vec<uuid::Uuid>) {
        self.denied_classes = classes;
    }
    fn start(&mut self) -> Result<(), SourceError> {
        if self.worker.is_some() {
            return Ok(());
        }
        let device = serde_json::to_value(self.device).expect("Device type serializes");
        let mut command = Command::new(&self.path);
        command
            .args([
                "--import",
                "--prog-id",
                &self.prog_id,
                "--device-type",
                device.as_str().unwrap(),
                "--connection-policy",
                if self.policy == ConnectionPolicy::Managed {
                    "managed"
                } else {
                    "externallyManaged"
                },
                "--bitness",
                architecture(self.bitness),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !self.denied_classes.is_empty() {
            command.args([
                "--deny-clsids",
                &self
                    .denied_classes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let child = command.spawn().map_err(|_| {
            SourceError::new(ErrorKind::Permanent, "Could not start the COM worker")
        })?;
        // Attach ownership before the first connectStep activates vendor code.
        self.worker = Some(AccessoryWorker::new_independent(child).map_err(|_| {
            SourceError::new(
                ErrorKind::Permanent,
                "Could not establish COM worker ownership",
            )
        })?);
        self.request_id = 0;
        self.info = None;
        self.ready = false;
        Ok(())
    }
    async fn request(
        &mut self,
        operation: &str,
        member: Option<&str>,
        parameters: Values,
    ) -> Result<Value, SourceError> {
        let write = matches!(operation, "write" | "refresh" | "disconnectStep")
            || operation == "connectStep" && self.policy == ConnectionPolicy::Managed;
        let connection_change = matches!(operation, "connectStep" | "disconnectStep")
            && self.policy == ConnectionPolicy::Managed;
        self.request_id = self.request_id.checked_add(1).ok_or_else(invalid)?;
        let mut request = json!({"protocol":1,"id":self.request_id,"operation":operation});
        if let Some(member) = member {
            request["member"] = json!(member);
            request["parameters"] = json!(parameters);
        }
        let worker = self.worker.as_mut().ok_or_else(|| {
            SourceError::new(ErrorKind::Disconnected, "COM worker is disconnected")
        })?;
        // Armed before await: cancellation of a handshake cannot permit its
        // mutation to be replayed after reset in a fresh worker.
        if connection_change {
            self.connection_uncertain = true;
        }
        let reply: Reply = match worker
            .request_typed_with_timeout(request, self.deadline)
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                let invalid = error
                    .downcast_ref::<AccessoryError>()
                    .is_some_and(|e| matches!(e, AccessoryError::InvalidRequest));
                if invalid {
                    if connection_change {
                        self.connection_uncertain = false;
                    }
                    return Err(invalid_request());
                }
                self.ready = false;
                if let Some(info) = &mut self.info {
                    info.uncertain |= write;
                }
                self.reset();
                return Err(transport_error(write));
            }
        };
        let method = if reply.connection.method == Method::Async {
            ConnectionMethod::Async
        } else {
            ConnectionMethod::Legacy
        };
        let expected_method = if reply.connection.interface_version.is_some_and(|v| {
            v >= match self.device {
                DeviceType::ObservingConditions => 2,
                DeviceType::Focuser | DeviceType::Rotator => 4,
                _ => 3,
            }
        }) {
            ConnectionMethod::Async
        } else {
            ConnectionMethod::Legacy
        };
        if reply.protocol != 1
            || reply.id != self.request_id
            || reply.connection.device_type != self.device
            || reply
                .connection
                .interface_version
                .is_some_and(|v| v == 0 || v > i16::MAX as u16)
            || method != expected_method
            || reply.connection.uncertain && reply.connection.ready
            || self.policy == ConnectionPolicy::ExternallyManaged
                && reply.connection.owns_connection
            || reply.error.is_none()
                && matches!(operation, "read" | "write" | "refresh")
                && !reply.connection.ready
            || reply.error.is_some() && !reply.value.is_null()
            || reply.error.is_none()
                && operation == "connectStep"
                && reply.value.as_bool() != Some(reply.connection.ready)
            || reply.error.is_none()
                && operation == "disconnectStep"
                && (reply.value.as_bool().is_none()
                    || reply.connection.ready
                    || reply.value == true
                        && (reply.connection.owns_connection || reply.connection.uncertain))
            || reply.error.is_none()
                && matches!(operation, "write" | "refresh")
                && !reply.value.is_null()
            || reply.value.is_array()
            || reply.value.is_object()
        {
            self.reset();
            return Err(transport_error(write));
        }
        self.ready = reply.connection.ready;
        if connection_change {
            self.connection_uncertain = reply.connection.uncertain;
        }
        self.info = Some(ConnectionInfo {
            device_type: self.device,
            interface_version: reply.connection.interface_version,
            method,
            owns_connection: reply.connection.owns_connection,
            uncertain: reply.connection.uncertain,
        });
        if let Some(error) = reply.error {
            let kind = match error.kind {
                Kind::Unsupported => ErrorKind::Unsupported,
                Kind::InvalidValue => ErrorKind::InvalidValue,
                Kind::Disconnected => ErrorKind::Disconnected,
                Kind::Unavailable => ErrorKind::Unavailable,
                Kind::Permanent => ErrorKind::Permanent,
                Kind::Transient => ErrorKind::Transient,
                Kind::Uncertain => ErrorKind::Uncertain,
            };
            return Err(SourceError {
                upstream_code: error.code,
                ..SourceError::new(kind, "Upstream COM driver rejected the request")
            });
        }
        Ok(reply.value)
    }
    async fn connection_step(&mut self) -> Result<bool, SourceError> {
        if self.connection_uncertain {
            return Err(SourceError::uncertain());
        }
        if self.ready {
            return Ok(true);
        }
        let started = *self.connecting_since.get_or_insert_with(Instant::now);
        if started.elapsed() >= self.connection_deadline {
            self.reset();
            return Err(SourceError::timeout());
        }
        self.start()?;
        let remaining = self.connection_deadline.saturating_sub(started.elapsed());
        let result =
            match tokio::time::timeout(remaining, self.request("connectStep", None, Values::new()))
                .await
            {
                Ok(result) => result?,
                Err(_) => {
                    self.reset();
                    return Err(transport_error(self.policy == ConnectionPolicy::Managed));
                }
            };
        let ready = result.as_bool().ok_or_else(|| transport_error(false))?;
        if ready != self.ready {
            self.reset();
            return Err(transport_error(false));
        }
        if ready {
            self.connecting_since = None;
        }
        Ok(ready)
    }
    async fn sample_step(&mut self) -> Result<SampleBatch, SourceError> {
        let Some(sample) = self.polling.prepare() else {
            return Ok(SampleBatch::default());
        };
        let result = self
            .request("read", Some(&sample.member), sample.parameters.clone())
            .await;
        self.polling.finish(
            sample,
            result,
            SourceError::new(
                ErrorKind::Unavailable,
                "COM driver returned an invalid scalar value",
            ),
        )
    }
    async fn disconnect_operation(&mut self) -> Result<(), SourceError> {
        loop {
            let done = self.request("disconnectStep", None, Values::new()).await?;
            if done == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.disconnecting = false;
        self.ready = false;
        self.connecting_since = None;
        if let Some(worker) = self.worker.take() {
            worker.close().await;
        }
        Ok(())
    }
}
impl Backend for ComBackend {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            while !self.connection_step().await? {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(())
        })
    }
    fn connect_step(&mut self) -> BackendFuture<'_, bool> {
        Box::pin(self.connection_step())
    }
    fn connection_info(&self) -> Option<ConnectionInfo> {
        self.info.clone()
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.connection_uncertain {
                return Err(SourceError::uncertain());
            }
            if self.worker.is_none() {
                return Ok(());
            }
            self.disconnecting = true;
            match tokio::time::timeout(self.deadline, self.disconnect_operation()).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => {
                    self.reset();
                    Err(error)
                }
                Err(_) => {
                    self.reset();
                    Err(transport_error(true))
                }
            }
        })
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move { self.request("read", Some(&member), parameters).await })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move { self.request("write", Some(&member), parameters).await })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            let mut values = Values::new();
            loop {
                let batch = self.sample_step().await?;
                if let Some(error) = batch.errors.into_values().next() {
                    return Err(error);
                }
                values.extend(batch.values);
                if !batch.more {
                    return Ok(values);
                }
            }
        })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(self.sample_step())
    }
    fn reset(&mut self) {
        self.connection_uncertain |= self.disconnecting && self.policy == ConnectionPolicy::Managed;
        self.worker.take();
        self.ready = false;
        self.connecting_since = None;
        if let Some(info) = &mut self.info {
            // A killed worker cannot confirm upstream cleanup. Retain ownership
            // evidence as uncertain diagnostics, never as a reusable lease.
            info.uncertain |= info.owns_connection || self.connection_uncertain;
        }
        self.polling.restart();
    }
    fn restart_poll(&mut self) {
        self.polling.restart();
    }
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.device == DeviceType::ObservingConditions {
                self.request("refresh", None, Values::new()).await?;
            }
            Ok(())
        })
    }
}
fn invalid() -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, "Invalid COM source configuration")
}
fn invalid_request() -> SourceError {
    SourceError::new(
        ErrorKind::InvalidValue,
        "COM request exceeds the worker protocol bounds",
    )
}
fn unsupported(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::Unsupported, message)
}
fn transport_error(write: bool) -> SourceError {
    SourceError {
        transport_lost: true,
        ..if write {
            SourceError::uncertain()
        } else {
            SourceError::transient()
        }
    }
}
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    protocol: u16,
    id: i64,
    value: Value,
    #[serde(deserialize_with = "nullable")]
    error: Option<WireError>,
    connection: WireConnection,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireError {
    kind: Kind,
    #[serde(deserialize_with = "nullable")]
    code: Option<i32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Kind {
    Unsupported,
    InvalidValue,
    Disconnected,
    Unavailable,
    Permanent,
    Transient,
    Uncertain,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Method {
    Legacy,
    Async,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireConnection {
    device_type: DeviceType,
    #[serde(deserialize_with = "nullable")]
    interface_version: Option<u16>,
    method: Method,
    owns_connection: bool,
    uncertain: bool,
    ready: bool,
}
