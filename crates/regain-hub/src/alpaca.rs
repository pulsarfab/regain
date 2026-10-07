//! Bounded upstream Alpaca transport. Typed output controllers choose the poll
//! plan; this adapter never invents capabilities or retries a command itself.
use crate::{
    camera::image::{CameraImage, ImageBudget, ImageReadError, read_imagebytes},
    camera::json_image::read_json_image,
    config::{
        ConnectionPolicy, DeviceType, SourceBackend, SourceConfig, normalize_alpaca_id,
        valid_alpaca_id,
    },
    source::{
        Backend, BackendFuture, ConnectionInfo, ConnectionMethod, ErrorKind, SampleBatch,
        SampleBudget, SourceError, Values,
    },
};
use futures_util::TryStreamExt;
use reqwest::{
    Client, Method,
    header::{AUTHORIZATION, HeaderValue, RETRY_AFTER},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    time::{Duration, SystemTime},
};
use url::Url;
use uuid::Uuid;

pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub mod discovery;
use crate::sampling::PropertyPoll;
pub use crate::sampling::{SampleRequest, SampleType};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Discover,
    Check,
    Open,
    WaitOpen,
    Verify,
    Ready,
    WaitClose,
    Failed,
}

pub struct AlpacaBackend {
    client: Client,
    image_client: Option<Client>,
    root: Url,
    identity_pin: Option<IdentityPin>,
    client_id: u32,
    transaction: u32,
    policy: ConnectionPolicy,
    opened_connection: Option<ConnectionMethod>,
    connection_uncertain: bool,
    device_type: DeviceType,
    interface_version: Option<u16>,
    connection_method: Option<ConnectionMethod>,
    connection_phase: ConnectionPhase,
    connection_started: Option<tokio::time::Instant>,
    connection_deadline: Duration,
    polling: PropertyPoll,
    weather_source: bool,
}
struct IdentityPin {
    server: Url,
    number: u32,
    unique_id: String,
}
impl AlpacaBackend {
    /// `authorization` is resolved by the host's credential provider. This type
    /// does not persist credentials, expose Debug, or echo URLs/response text.
    pub fn new(
        config: &SourceConfig,
        samples: Vec<SampleRequest>,
        authorization: Option<HeaderValue>,
    ) -> Result<Self, SourceError> {
        let SourceBackend::Alpaca {
            base_url,
            device_type,
            device_number,
            unique_id,
            connection_policy,
            credential_reference,
        } = &config.backend
        else {
            return Err(invalid("Expected an Alpaca source"));
        };
        if !config.polling.validate_timing().is_empty() {
            return Err(invalid("Invalid source timing"));
        }
        if credential_reference.is_some() != authorization.is_some() {
            return Err(invalid(
                "The configured credential reference must be resolved before connecting",
            ));
        }
        let mut root = server_root(base_url)?;
        let identity_pin = unique_id
            .as_deref()
            .map(|id| {
                if !valid_alpaca_id(id) {
                    return Err(invalid("Invalid upstream catalog identity"));
                }
                Ok(IdentityPin {
                    server: root.clone(),
                    number: *device_number,
                    unique_id: normalize_alpaca_id(id),
                })
            })
            .transpose()?;
        let source_device_type = *device_type;
        let device_type = serde_json::to_value(device_type).expect("Device type serializes");
        root.set_path(&format!(
            "{}/api/v1/{}/{device_number}/",
            root.path().trim_end_matches('/'),
            device_type.as_str().unwrap()
        ));
        let weather_source = source_device_type == DeviceType::ObservingConditions;
        for sample in &samples {
            encode_parameters(&sample.parameters)?;
        }
        let polling = PropertyPoll::new(
            source_device_type,
            samples,
            config.polling.attempts_per_cycle,
        )?;
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(mut authorization) = authorization {
            authorization.set_sensitive(true);
            headers.insert(AUTHORIZATION, authorization);
        }
        let deadline = Duration::from_secs_f64(config.polling.request_timeout_seconds);
        let client = http_client(headers.clone(), deadline, true)?;
        // Image bodies have the caller's separately bounded download deadline.
        // Applying the scalar timeout here would truncate ordinary long reads.
        let image_client = (source_device_type == DeviceType::Camera)
            .then(|| http_client(headers, deadline, false))
            .transpose()?;
        Ok(Self {
            client,
            image_client,
            root,
            identity_pin,
            client_id: (Uuid::new_v4().as_u128() as u32).max(1),
            transaction: 0,
            policy: *connection_policy,
            opened_connection: None,
            connection_uncertain: false,
            device_type: source_device_type,
            interface_version: None,
            connection_method: None,
            connection_phase: ConnectionPhase::Discover,
            connection_started: None,
            connection_deadline: Duration::from_secs_f64(config.polling.connection_timeout_seconds),
            polling,
            weather_source,
        })
    }
    async fn request(
        &mut self,
        write: bool,
        member: &str,
        parameters: Values,
    ) -> Result<Value, SourceError> {
        validate_member(member)?;
        let mut parameters = encode_parameters(&parameters)?;
        self.verify_identity().await?;
        self.transaction = self.transaction.wrapping_add(1).max(1);
        parameters.push(("ClientID".into(), self.client_id.to_string()));
        parameters.push(("ClientTransactionID".into(), self.transaction.to_string()));
        let url = self
            .root
            .join(member)
            .map_err(|_| invalid("Invalid Alpaca member"))?;
        let builder = self
            .client
            .request(if write { Method::PUT } else { Method::GET }, url);
        let request = if write {
            builder.form(&parameters)
        } else {
            builder.query(&parameters)
        };
        let response = request.send().await.map_err(|_| transport_error(write))?;
        read_response(response, self.transaction, write, member).await
    }
    async fn image(&mut self, budget: ImageBudget) -> Result<CameraImage, SourceError> {
        if self.image_client.is_none() {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Source is not an Alpaca camera",
            ));
        }
        self.verify_identity().await?;
        let client = self.image_client.as_ref().ok_or_else(|| {
            SourceError::new(ErrorKind::Unsupported, "Source is not an Alpaca camera")
        })?;
        self.transaction = self.transaction.wrapping_add(1).max(1);
        let transaction = self.transaction;
        let url = self
            .root
            .join("imagearray")
            .map_err(|_| invalid("Invalid Alpaca member"))?;
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/imagebytes")
            .query(&[
                ("ClientID", self.client_id),
                ("ClientTransactionID", transaction),
            ])
            .send()
            .await
            .map_err(|_| transport_error(false))?;
        if response.status().as_u16() != 200 {
            return Err(http_error(&response, "imagearray"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or_default();
        let json = content_type.eq_ignore_ascii_case("application/json");
        if !json && !content_type.eq_ignore_ascii_case("application/imagebytes") {
            return Err(bad_response(false));
        }
        // Stream directly into the pre-reserved immutable image allocation.
        // No full HTTP body or JSON image allocation precedes the payload budget.
        let stream = response
            .bytes_stream()
            .map_err(|_| std::io::Error::other("Alpaca camera image transport failed"));
        let mut reader = tokio_util::io::StreamReader::new(stream);
        let image = if json {
            read_json_image(&mut reader, &budget, transaction).await
        } else {
            read_imagebytes(&mut reader, &budget, transaction)
                .await
                .map(|response| response.image)
        };
        image.map_err(image_error)
    }
    async fn verify_identity(&mut self) -> Result<(), SourceError> {
        let Some(pin) = &self.identity_pin else {
            return Ok(());
        };
        self.transaction = self.transaction.wrapping_add(1).max(1);
        let catalog = discovery::query(
            &self.client,
            pin.server.clone(),
            self.client_id,
            self.transaction,
        )
        .await?;
        if !catalog.iter().any(|device| {
            device.supported_device_type == Some(self.device_type)
                && device.number == pin.number
                && normalize_alpaca_id(&device.unique_id) == pin.unique_id
        }) {
            return Err(SourceError::new(
                ErrorKind::Permanent,
                "Pinned Alpaca identity no longer matches this device address; inspect the server catalog before reconnecting",
            ));
        }
        Ok(())
    }
    async fn read_sample(&mut self, sample: &SampleRequest) -> Result<(Value, f64), SourceError> {
        // Query age first. If the sensor updates between requests, attributing
        // the earlier age to the new value is conservative, never rejuvenating.
        let age = if let Some(property) = &sample.sensor_age {
            self.request(
                false,
                "timesincelastupdate",
                Values::from([("SensorName".into(), Value::from(property.clone()))]),
            )
            .await?
            .as_f64()
            .filter(|age| age.is_finite() && *age >= 0.0)
            .ok_or_else(|| {
                SourceError::new(ErrorKind::Unavailable, "Sensor has no valid update time")
            })?
        } else {
            0.0
        };
        let value = self
            .request(false, &sample.member, sample.parameters.clone())
            .await?;
        if !sample.valid_value(&value) {
            return Err(bad_response(false));
        }
        Ok((value, age))
    }
    async fn collect_samples(&mut self) -> Result<SampleBatch, SourceError> {
        let mut batch = SampleBatch::default();
        let mut budget = SampleBudget::default();
        for sample in self.polling.samples().to_vec() {
            match self.read_sample(&sample).await {
                Ok((value, age)) => {
                    if !budget.admit(&value) {
                        return Err(bad_response(false));
                    }
                    batch.ages_seconds.insert(sample.key.clone(), age);
                    batch.values.insert(sample.key, value);
                }
                Err(error) => {
                    // Transport outages and Retry-After apply to the source.
                    // A rejected sensor/property only removes that measurement.
                    if self.polling.samples().len() == 1
                        || error.transport_lost
                        || matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected)
                    {
                        return Err(error);
                    }
                    batch.errors.insert(sample.key, error);
                }
            }
        }
        Ok(batch)
    }
    async fn sample_step(&mut self) -> Result<SampleBatch, SourceError> {
        let Some(sample) = self.polling.prepare() else {
            return Ok(SampleBatch::default());
        };
        let result = self
            .request(false, &sample.member, sample.parameters.clone())
            .await;
        self.polling.finish(sample, result, bad_response(false))
    }
    async fn connection_step(&mut self) -> Result<bool, SourceError> {
        use ConnectionPhase::*;
        if self.connection_uncertain {
            return Err(SourceError::uncertain());
        }
        if self.connection_phase == Ready {
            return Ok(true);
        }
        if self.connection_phase == Failed {
            return Err(connection_expired());
        }
        let started = *self
            .connection_started
            .get_or_insert_with(tokio::time::Instant::now);
        let Some(remaining) = self.connection_deadline.checked_sub(started.elapsed()) else {
            self.connection_phase = Failed;
            return Err(connection_expired());
        };
        match tokio::time::timeout(remaining, self.connection_operation()).await {
            Ok(result) => result,
            Err(_) => {
                self.connection_phase = Failed;
                Err(connection_expired())
            }
        }
    }
    async fn connection_operation(&mut self) -> Result<bool, SourceError> {
        use ConnectionPhase::*;
        match self.connection_phase {
            Discover => {
                let version = match self.request(false, "interfaceversion", Values::new()).await {
                    Ok(value) => Some(
                        value
                            .as_u64()
                            .filter(|v| (1..=i16::MAX as u64).contains(v))
                            .ok_or_else(|| bad_response(false))? as u16,
                    ),
                    Err(error) if error.kind == ErrorKind::Unsupported => None,
                    Err(error) => return Err(error),
                };
                self.interface_version = version;
                self.connection_method = Some(
                    if version.is_some_and(|v| v >= asynchronous_version(self.device_type)) {
                        ConnectionMethod::Async
                    } else {
                        ConnectionMethod::Legacy
                    },
                );
                self.connection_phase = Check;
            }
            Check => {
                let connected = self.connection_boolean("connected").await?;
                let modern_managed = self.policy == ConnectionPolicy::Managed
                    && self.connection_method == Some(ConnectionMethod::Async);
                if connected && !modern_managed {
                    self.connection_phase = Ready;
                } else if self.policy == ConnectionPolicy::ExternallyManaged {
                    return Err(SourceError::new(
                        ErrorKind::Transient,
                        "Upstream device is disconnected; its owner must connect it",
                    ));
                } else {
                    // Modern Connected can describe shared hardware. Claim this
                    // client's connection even when another client already opened it.
                    self.connection_phase = Open;
                }
            }
            Open => {
                let method = self.connection_method.unwrap();
                self.connection_uncertain = true;
                let result = self.connection_write(method, true).await;
                match result {
                    Ok(()) => {
                        self.opened_connection = Some(method);
                        self.connection_uncertain = false;
                        self.connection_phase = if method == ConnectionMethod::Async {
                            WaitOpen
                        } else {
                            Ready
                        };
                    }
                    Err(error) => {
                        if !matches!(error.kind, ErrorKind::Transient | ErrorKind::Uncertain) {
                            self.connection_uncertain = false;
                        }
                        return Err(error);
                    }
                }
            }
            WaitOpen => {
                if !self.connection_boolean("connecting").await? {
                    self.connection_phase = Verify;
                }
            }
            Verify => {
                if self.connection_boolean("connected").await? {
                    self.connection_phase = Ready;
                } else {
                    self.connection_phase = Failed;
                    return Err(SourceError::new(
                        ErrorKind::Permanent,
                        "Upstream connection completed without connecting",
                    ));
                }
            }
            WaitClose => {
                if !self.connection_boolean("connecting").await? {
                    self.connection_phase = Discover;
                }
            }
            Ready | Failed => unreachable!(),
        }
        Ok(self.connection_phase == Ready)
    }
    async fn connection_boolean(&mut self, member: &str) -> Result<bool, SourceError> {
        self.request(false, member, Values::new())
            .await?
            .as_bool()
            .ok_or_else(|| bad_response(false))
    }
    async fn connection_write(
        &mut self,
        method: ConnectionMethod,
        connected: bool,
    ) -> Result<(), SourceError> {
        let (member, parameters) = match method {
            ConnectionMethod::Legacy => (
                "connected",
                Values::from([("Connected".into(), Value::Bool(connected))]),
            ),
            ConnectionMethod::Async => (
                if connected { "connect" } else { "disconnect" },
                Values::new(),
            ),
        };
        self.request(true, member, parameters).await.map(|_| ())
    }
}
impl Backend for AlpacaBackend {
    fn camera_image(&mut self, budget: ImageBudget) -> BackendFuture<'_, CameraImage> {
        Box::pin(self.image(budget))
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            // Direct callers get the complete handshake. Source actors invoke
            // connect_step instead, admitting commands between HTTP requests.
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
        self.connection_method.map(|method| ConnectionInfo {
            device_type: self.device_type,
            interface_version: self.interface_version,
            method,
            owns_connection: self.opened_connection.is_some(),
            uncertain: self.connection_uncertain,
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if let Some(method) = self.opened_connection {
                if self.connection_uncertain {
                    return Err(SourceError::uncertain());
                }
                // Consume the cleanup attempt before awaiting. Cancellation or
                // a lost reply must not replay Disconnect on a later shutdown.
                self.connection_uncertain = true;
                self.connection_write(method, false).await?;
                self.opened_connection = None;
                self.connection_uncertain = false;
                self.connection_phase = if method == ConnectionMethod::Async {
                    ConnectionPhase::WaitClose
                } else {
                    ConnectionPhase::Discover
                };
                self.connection_started = None;
            } else if self.connection_phase != ConnectionPhase::WaitClose {
                self.connection_phase = ConnectionPhase::Discover;
                self.connection_started = None;
            }
            Ok(())
        })
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move { self.request(false, &member, parameters).await })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            if matches!(member.as_str(), "connected" | "connect" | "disconnect") {
                return Err(invalid("Connection changes require source leases"));
            }
            self.request(true, &member, parameters).await
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            let batch = self.collect_samples().await?;
            if let Some(error) = batch.errors.into_values().next() {
                return Err(error);
            }
            Ok(batch.values)
        })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(self.sample_step())
    }
    fn reset(&mut self) {
        // Dropping a reqwest future cancels that local request; no serial stream
        // needs draining. Retain acknowledged connection ownership for cleanup.
        self.restart_poll();
        if matches!(
            self.connection_phase,
            ConnectionPhase::Ready
                | ConnectionPhase::Discover
                | ConnectionPhase::Check
                | ConnectionPhase::Open
        ) {
            self.connection_phase = ConnectionPhase::Discover;
            self.connection_started = None;
        }
    }
    fn restart_poll(&mut self) {
        self.polling.restart();
    }
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.weather_source {
                self.request(true, "refresh", Values::new()).await?;
            }
            Ok(())
        })
    }
}

fn invalid(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, message)
}
pub(crate) fn server_root(base_url: &str) -> Result<Url, SourceError> {
    if base_url.len() > 4096 {
        return Err(invalid("Invalid Alpaca URL"));
    }
    let root = Url::parse(base_url).map_err(|_| invalid("Invalid Alpaca URL"))?;
    if !matches!(root.scheme(), "http" | "https")
        || root.host_str().is_none()
        || !root.username().is_empty()
        || root.password().is_some()
        || root.query().is_some()
        || root.fragment().is_some()
    {
        return Err(invalid(
            "Alpaca URL must be HTTP(S), without credentials, query or fragment",
        ));
    }
    Ok(root)
}
async fn read_response(
    mut response: reqwest::Response,
    transaction: u32,
    write: bool,
    member: &str,
) -> Result<Value, SourceError> {
    let body = read_body(&mut response, write, member).await?;
    parse_response(&body, transaction, write)
}
async fn read_body(
    response: &mut reqwest::Response,
    write: bool,
    member: &str,
) -> Result<Vec<u8>, SourceError> {
    if response.status().as_u16() != 200 {
        return Err(http_error(response, member));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(bad_response(write));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| transport_error(write))? {
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            return Err(bad_response(write));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn image_error(error: ImageReadError) -> SourceError {
    match error {
        ImageReadError::Contract(error)
            if matches!(error.kind, ErrorKind::InvalidValue | ErrorKind::Unsupported) =>
        {
            bad_response(false)
        }
        // Preserve local budget/allocation/decoder failures; they do not prove
        // that the remote device sent a malformed image or lost its session.
        ImageReadError::Contract(error) => error,
        ImageReadError::Io(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            bad_response(false)
        }
        ImageReadError::Io(_) => transport_error(false),
        ImageReadError::Upstream { code, .. } => upstream_error(code as i32),
    }
}
fn http_client(
    headers: reqwest::header::HeaderMap,
    deadline: Duration,
    scalar: bool,
) -> Result<Client, SourceError> {
    let builder = Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .connect_timeout(deadline)
        .pool_max_idle_per_host(1);
    let builder = if scalar {
        builder.timeout(deadline)
    } else {
        builder
    };
    builder
        .build()
        .map_err(|_| invalid("Could not initialize Alpaca HTTP client"))
}
fn http_error(response: &reqwest::Response, member: &str) -> SourceError {
    let status = response.status().as_u16();
    SourceError {
        upstream_code: Some(i32::from(status)),
        retry_after: response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(retry_after),
        ..SourceError::new(
            if status == 404 && member == "interfaceversion" {
                ErrorKind::Unsupported
            } else if matches!(status, 408 | 429 | 500 | 502 | 503 | 504) {
                ErrorKind::Transient
            } else {
                ErrorKind::Permanent
            },
            "Alpaca server returned an HTTP error",
        )
    }
}
fn upstream_error(number: i32) -> SourceError {
    let kind = match number {
        0x400 | 0x40c => ErrorKind::Unsupported,
        0x401 => ErrorKind::InvalidValue,
        0x402 => ErrorKind::Unavailable,
        0x407 => ErrorKind::Disconnected,
        _ => ErrorKind::Permanent,
    };
    SourceError {
        upstream_code: Some(number),
        transport_lost: number == 0x407,
        ..SourceError::new(kind, "Upstream Alpaca device rejected the request")
    }
}
fn connection_expired() -> SourceError {
    SourceError::new(
        ErrorKind::Permanent,
        "Source connection timed out; disconnect or reconfigure before trying again",
    )
}
fn asynchronous_version(device: DeviceType) -> u16 {
    match device {
        DeviceType::Camera | DeviceType::Focuser | DeviceType::Rotator => 4,
        DeviceType::Switch | DeviceType::SafetyMonitor | DeviceType::FilterWheel => 3,
        DeviceType::ObservingConditions | DeviceType::CoverCalibrator => 2,
    }
}
fn transport_error(write: bool) -> SourceError {
    if write {
        SourceError::uncertain()
    } else {
        SourceError::transient()
    }
}
fn bad_response(write: bool) -> SourceError {
    if write {
        SourceError::uncertain()
    } else {
        SourceError::new(ErrorKind::Permanent, "Invalid or oversized Alpaca response")
    }
}
fn validate_member(member: &str) -> Result<(), SourceError> {
    if member.is_empty()
        || member.len() > 64
        || !member
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(invalid("Invalid Alpaca member"));
    }
    Ok(())
}
fn encode_parameters(values: &Values) -> Result<Vec<(String, String)>, SourceError> {
    if values.len() > 32 {
        return Err(invalid("Too many request parameters"));
    }
    let mut keys = BTreeSet::new();
    values
        .iter()
        .map(|(key, value)| {
            let canonical = key.to_ascii_lowercase();
            if key.is_empty()
                || key.len() > 64
                || !key.bytes().all(|byte| byte.is_ascii_alphanumeric())
                || matches!(canonical.as_str(), "clientid" | "clienttransactionid")
                || !keys.insert(canonical)
            {
                return Err(invalid("Invalid or reserved Alpaca parameter"));
            }
            let value = match value {
                Value::String(value) if value.len() <= 8192 => value.clone(),
                Value::Bool(_) | Value::Number(_) => value.to_string(),
                _ => return Err(invalid("Alpaca parameters must be bounded scalar values")),
            };
            Ok((key.clone(), value))
        })
        .collect()
}
fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
}
#[derive(Default)]
enum WireId {
    #[default]
    Missing,
    Present(u32),
}
impl<'de> Deserialize<'de> for WireId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        u32::deserialize(deserializer).map(Self::Present)
    }
}
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "ErrorNumber", default)]
    error_number: i32,
    #[serde(rename = "ErrorMessage", default)]
    _error_message: String,
    #[serde(rename = "ClientTransactionID", default)]
    client_transaction: WireId,
    #[serde(rename = "ServerTransactionID", default)]
    _server_transaction: WireId,
    #[serde(rename = "Value", default)]
    value: Option<Value>,
}
fn parse_response(body: &[u8], transaction: u32, write: bool) -> Result<Value, SourceError> {
    // Struct deserialization rejects duplicate fields as well as wrong types;
    // a JSON object map would silently choose the last of two safety Values.
    let fields: Envelope = serde_json::from_slice(body).map_err(|_| bad_response(write))?;
    // Match the Field Kit compatibility behavior for omitted ErrorNumber, but
    // never coerce malformed present values (including null) to success.
    let number = fields.error_number;
    if matches!(fields.client_transaction, WireId::Present(id) if id != transaction) {
        return Err(bad_response(write));
    }
    if number != 0 {
        return Err(upstream_error(number));
    }
    match fields.value {
        Some(value) => Ok(value),
        None if write => Ok(Value::Null),
        None => Err(bad_response(false)),
    }
}
