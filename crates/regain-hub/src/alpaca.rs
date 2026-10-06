//! Bounded upstream Alpaca transport. Typed output controllers choose the poll
//! plan; this adapter never invents capabilities or retries a command itself.
use crate::{
    config::{ConnectionPolicy, Readout, SourceBackend, SourceConfig},
    source::{Backend, BackendFuture, ErrorKind, SampleBatch, SourceError, Values},
};
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
const MAX_SAMPLES: usize = 1024;

#[derive(Clone, Copy)]
pub enum SampleType {
    Boolean,
    Number,
    Text,
}
#[derive(Clone)]
pub struct SampleRequest {
    pub key: String,
    pub member: String,
    pub parameters: Values,
    pub value_type: SampleType,
    pub sensor_age: Option<String>,
}
impl SampleRequest {
    pub fn safety() -> Self {
        Self {
            key: "issafe".into(),
            member: "issafe".into(),
            parameters: Values::new(),
            value_type: SampleType::Boolean,
            sensor_age: None,
        }
    }
    pub fn readout(readout: &Readout, observing_conditions: bool) -> Self {
        match readout {
            Readout::Channel { channel, .. } => Self {
                key: readout.sample_key(),
                member: "getswitchvalue".into(),
                parameters: Values::from([("Id".into(), Value::from(*channel))]),
                value_type: SampleType::Number,
                sensor_age: None,
            },
            Readout::Property { property, .. } => Self {
                key: readout.sample_key(),
                member: property.clone(),
                parameters: Values::new(),
                value_type: if property == "issafe" {
                    SampleType::Boolean
                } else {
                    SampleType::Number
                },
                sensor_age: observing_conditions.then(|| property.clone()),
            },
        }
    }
}

pub struct AlpacaBackend {
    client: Client,
    root: Url,
    client_id: u32,
    transaction: u32,
    policy: ConnectionPolicy,
    opened_connection: bool,
    connection_uncertain: bool,
    samples: Vec<SampleRequest>,
    cursor: usize,
    pending_age: Option<(f64, tokio::time::Instant)>,
    safety_source: bool,
    weather_source: bool,
    sample_failures: u32,
    attempts_per_cycle: u32,
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
        let mut root = Url::parse(base_url).map_err(|_| invalid("Invalid Alpaca URL"))?;
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
        let device_type = serde_json::to_value(device_type).expect("Device type serializes");
        root.set_path(&format!(
            "{}/api/v1/{}/{device_number}/",
            root.path().trim_end_matches('/'),
            device_type.as_str().unwrap()
        ));
        let mut keys = BTreeSet::new();
        if samples.len() > MAX_SAMPLES {
            return Err(invalid("Too many poll samples"));
        }
        let safety_source = device_type.as_str() == Some("safetymonitor");
        let weather_source = device_type.as_str() == Some("observingconditions");
        if safety_source
            && (samples.len() != 1
                || samples[0].key != "issafe"
                || samples[0].member != "issafe"
                || !matches!(samples[0].value_type, SampleType::Boolean)
                || !samples[0].parameters.is_empty()
                || samples[0].sensor_age.is_some())
        {
            return Err(invalid(
                "SafetyMonitor polling requires one IsSafe observation per attempt",
            ));
        }
        for sample in &samples {
            if sample.key.is_empty() || sample.key.len() > 200 || !keys.insert(&sample.key) {
                return Err(invalid("Poll sample keys must be unique and bounded"));
            }
            validate_member(&sample.member)?;
            if let Some(property) = &sample.sensor_age {
                validate_member(property)?;
            }
            encode_parameters(&sample.parameters)?;
        }
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(mut authorization) = authorization {
            authorization.set_sensitive(true);
            headers.insert(AUTHORIZATION, authorization);
        }
        let deadline = Duration::from_secs_f64(config.polling.request_timeout_seconds);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(deadline)
            .connect_timeout(deadline)
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|_| invalid("Could not initialize Alpaca HTTP client"))?;
        Ok(Self {
            client,
            root,
            client_id: (Uuid::new_v4().as_u128() as u32).max(1),
            transaction: 0,
            policy: *connection_policy,
            opened_connection: false,
            connection_uncertain: false,
            samples,
            cursor: 0,
            pending_age: None,
            safety_source,
            weather_source,
            sample_failures: 0,
            attempts_per_cycle: config.polling.attempts_per_cycle,
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
        let mut response = request.send().await.map_err(|_| transport_error(write))?;
        let status = response.status().as_u16();
        if status != 200 {
            let mut error = SourceError::new(
                if matches!(status, 408 | 429 | 500 | 502 | 503 | 504) {
                    ErrorKind::Transient
                } else {
                    ErrorKind::Permanent
                },
                "Alpaca server returned an HTTP error",
            );
            error.upstream_code = Some(i32::from(status));
            error.retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry_after);
            return Err(error);
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
        parse_response(&body, self.transaction, write)
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
        let valid = match sample.value_type {
            SampleType::Boolean => value.is_boolean(),
            SampleType::Number => value.as_f64().is_some_and(f64::is_finite),
            SampleType::Text => value.is_string(),
        };
        if !valid {
            return Err(bad_response(false));
        }
        Ok((value, age))
    }
    async fn collect_samples(&mut self) -> Result<SampleBatch, SourceError> {
        let mut batch = SampleBatch::default();
        let mut text_bytes = 0usize;
        for sample in self.samples.clone() {
            match self.read_sample(&sample).await {
                Ok((value, age)) => {
                    text_bytes += value.as_str().map_or(0, str::len);
                    if text_bytes > MAX_RESPONSE_BYTES {
                        return Err(bad_response(false));
                    }
                    batch.ages_seconds.insert(sample.key.clone(), age);
                    batch.values.insert(sample.key, value);
                }
                Err(error) => {
                    // Transport outages and Retry-After apply to the source.
                    // A rejected sensor/property only removes that measurement.
                    if self.samples.len() == 1
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
        if self.samples.is_empty() {
            return Ok(SampleBatch::default());
        }
        if self.safety_source {
            return self.collect_samples().await;
        }
        let sample = self.samples[self.cursor].clone();
        let mut batch = SampleBatch {
            partial: true,
            ..SampleBatch::default()
        };
        let needs_age = sample.sensor_age.is_some() && self.pending_age.is_none();
        let began = tokio::time::Instant::now();
        let age = self
            .pending_age
            .map_or(0.0, |(age, at)| age + at.elapsed().as_secs_f64());
        let result = if needs_age {
            self.request(
                false,
                "timesincelastupdate",
                Values::from([(
                    "SensorName".into(),
                    Value::from(sample.sensor_age.clone().unwrap()),
                )]),
            )
            .await
        } else {
            self.request(false, &sample.member, sample.parameters.clone())
                .await
        };
        match result {
            Ok(value) if needs_age => {
                if let Some(age) = value.as_f64().filter(|age| age.is_finite() && *age >= 0.0) {
                    self.sample_failures = 0;
                    self.pending_age = Some((age, began));
                    batch.more = true;
                    return Ok(batch);
                }
                batch.errors.insert(
                    sample.key,
                    SourceError::new(ErrorKind::Unavailable, "Sensor has no valid update time"),
                );
            }
            Ok(value) => {
                let valid = match sample.value_type {
                    SampleType::Boolean => value.is_boolean(),
                    SampleType::Number => value.as_f64().is_some_and(f64::is_finite),
                    SampleType::Text => value.is_string(),
                };
                if valid {
                    batch.ages_seconds.insert(sample.key.clone(), age);
                    batch.values.insert(sample.key, value);
                } else {
                    batch.errors.insert(sample.key, bad_response(false));
                }
            }
            Err(error) => {
                if error.transport_lost {
                    return Err(error);
                }
                if matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected) {
                    self.sample_failures += 1;
                    if self.sample_failures < self.attempts_per_cycle {
                        batch.errors.insert(sample.key, error);
                        batch.more = true;
                        return Ok(batch);
                    }
                }
                batch.errors.insert(sample.key, error);
            }
        }
        self.sample_failures = 0;
        self.pending_age = None;
        self.cursor = (self.cursor + 1) % self.samples.len();
        batch.more = self.cursor != 0;
        Ok(batch)
    }
}
impl Backend for AlpacaBackend {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.connection_uncertain {
                return Err(SourceError::uncertain());
            }
            let connected = self
                .request(false, "connected", Values::new())
                .await?
                .as_bool()
                .ok_or_else(|| bad_response(false))?;
            if connected {
                return Ok(());
            }
            if self.policy == ConnectionPolicy::ExternallyManaged {
                return Err(SourceError::new(
                    ErrorKind::Transient,
                    "Upstream device is disconnected; its owner must connect it",
                ));
            }
            // Set before awaiting: cancellation must not silently replay this PUT.
            self.connection_uncertain = true;
            let result = self
                .request(
                    true,
                    "connected",
                    Values::from([("Connected".into(), Value::Bool(true))]),
                )
                .await;
            match result {
                Ok(_) => {
                    self.opened_connection = true;
                    self.connection_uncertain = false;
                    Ok(())
                }
                Err(error) => {
                    if !matches!(error.kind, ErrorKind::Transient | ErrorKind::Uncertain) {
                        self.connection_uncertain = false;
                    }
                    Err(error)
                }
            }
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.opened_connection {
                // Only release a connection whose opening was acknowledged.
                self.request(
                    true,
                    "connected",
                    Values::from([("Connected".into(), Value::Bool(false))]),
                )
                .await?;
                self.opened_connection = false;
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
    }
    fn restart_poll(&mut self) {
        self.cursor = 0;
        self.pending_age = None;
        self.sample_failures = 0;
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
        let kind = match number {
            0x400 | 0x40c => ErrorKind::Unsupported,
            0x401 => ErrorKind::InvalidValue,
            0x402 => ErrorKind::Unavailable,
            0x407 => ErrorKind::Disconnected,
            _ => ErrorKind::Permanent,
        };
        return Err(SourceError {
            upstream_code: Some(number),
            transport_lost: number == 0x407,
            ..SourceError::new(kind, "Upstream Alpaca device rejected the request")
        });
    }
    match fields.value {
        Some(value) => Ok(value),
        None if write => Ok(Value::Null),
        None => Err(bad_response(false)),
    }
}
