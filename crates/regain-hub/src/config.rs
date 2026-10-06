//! Persisted hub identities and revision-checked configuration replacement.
use crate::parameters::{FieldError, PollPolicy, SafetyPolicy};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Camera,
    Switch,
    SafetyMonitor,
    ObservingConditions,
    Focuser,
    Rotator,
    FilterWheel,
    CoverCalibrator,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionPolicy {
    #[default]
    ExternallyManaged,
    Managed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bitness {
    X86,
    X64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeDevice {
    CameraDirect,
    CameraSdk,
    Caa,
    Efw,
    Eaf,
    Fc3,
    Falcon,
    Ofp2,
    Eta,
}
impl NativeDevice {
    pub fn device_type(self) -> DeviceType {
        match self {
            Self::CameraDirect | Self::CameraSdk => DeviceType::Camera,
            Self::Caa | Self::Falcon => DeviceType::Rotator,
            Self::Efw => DeviceType::FilterWheel,
            Self::Eaf | Self::Fc3 | Self::Eta => DeviceType::Focuser,
            Self::Ofp2 => DeviceType::CoverCalibrator,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SourceBackend {
    Native {
        device: NativeDevice,
        identity: String,
    },
    Alpaca {
        base_url: String,
        device_type: DeviceType,
        device_number: u32,
        #[serde(default)]
        connection_policy: ConnectionPolicy,
        #[serde(default)]
        credential_reference: Option<String>,
    },
    Com {
        prog_id: String,
        device_type: DeviceType,
        bitness: Bitness,
        #[serde(default)]
        connection_policy: ConnectionPolicy,
    },
    Virtual {
        output: Uuid,
    },
    Simulated {
        device_type: DeviceType,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceConfig {
    pub id: Uuid,
    pub label: String,
    pub backend: SourceBackend,
    #[serde(default)]
    pub polling: PollPolicy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Readout {
    Channel { source: Uuid, channel: u32 },
    Property { source: Uuid, property: String },
}
impl Readout {
    pub fn source(&self) -> Uuid {
        match self {
            Self::Channel { source, .. } | Self::Property { source, .. } => *source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafetyMember {
    pub source: Uuid,
    pub enabled: bool,
    #[serde(default)]
    pub policy: SafetyPolicy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwitchChannel {
    pub id: Uuid,
    pub number: u32,
    pub label: String,
    pub readout: Readout,
    pub writable: bool,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub units: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeatherMetric {
    CloudCover,
    DewPoint,
    Humidity,
    Pressure,
    RainRate,
    SkyBrightness,
    SkyQuality,
    SkyTemperature,
    StarFwhm,
    Temperature,
    WindDirection,
    WindGust,
    WindSpeed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Measurement {
    /// First fresh valid source wins; changing source clears the averaging window.
    pub sources: Vec<Readout>,
    pub maximum_age_seconds: f64,
    pub average_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VirtualDevice {
    Safety {
        members: Vec<SafetyMember>,
    },
    Switch {
        channels: Vec<SwitchChannel>,
    },
    Weather {
        measurements: BTreeMap<WeatherMetric, Measurement>,
    },
    Proxy {
        source: Uuid,
        device_type: DeviceType,
    },
}
impl VirtualDevice {
    pub fn device_type(&self) -> DeviceType {
        match self {
            Self::Safety { .. } => DeviceType::SafetyMonitor,
            Self::Switch { .. } => DeviceType::Switch,
            Self::Weather { .. } => DeviceType::ObservingConditions,
            Self::Proxy { device_type, .. } => *device_type,
        }
    }
    fn sources(&self) -> Vec<Uuid> {
        match self {
            Self::Safety { members } => members.iter().map(|s| s.source).collect(),
            Self::Switch { channels } => channels.iter().map(|c| c.readout.source()).collect(),
            Self::Weather { measurements } => measurements
                .values()
                .flat_map(|m| m.sources.iter().map(Readout::source))
                .collect(),
            Self::Proxy { source, .. } => vec![*source],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputConfig {
    pub id: Uuid,
    pub number: u32,
    pub label: String,
    pub device: VirtualDevice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HubConfig {
    pub schema_version: u32,
    pub revision: Uuid,
    pub instance_id: Uuid,
    pub sources: Vec<SourceConfig>,
    pub outputs: Vec<OutputConfig>,
    /// Owned by the store. Retired IDs remain reserved after deletion/restart.
    #[serde(default)]
    pub identities: IdentityLedger,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityLedger {
    sources: BTreeMap<Uuid, String>,
    outputs: BTreeMap<Uuid, OutputIdentity>,
    channels: BTreeMap<Uuid, ChannelIdentity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputIdentity {
    number: u32,
    device_type: DeviceType,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelIdentity {
    output: Uuid,
    number: u32,
    readout: Readout,
}

impl SourceConfig {
    fn identity(&self) -> Result<String, &'static str> {
        Ok(match &self.backend {
            SourceBackend::Alpaca {
                base_url,
                device_type,
                device_number,
                ..
            } => format!(
                "alpaca:{}:{device_type:?}:{device_number}",
                normalized_url(base_url)?
            ),
            SourceBackend::Native { device, identity } => {
                let device =
                    if matches!(device, NativeDevice::CameraDirect | NativeDevice::CameraSdk) {
                        "Camera".into()
                    } else {
                        format!("{device:?}")
                    };
                format!("native:{device}:{identity}")
            }
            SourceBackend::Com {
                prog_id,
                device_type,
                ..
            } => format!("com:{}:{device_type:?}", prog_id.to_ascii_lowercase()),
            SourceBackend::Virtual { output } => format!("virtual:{output}"),
            SourceBackend::Simulated { device_type } => {
                format!("simulation:{device_type:?}:{}", self.id)
            }
        })
    }
}

impl IdentityLedger {
    fn register(&mut self, sources: &[SourceConfig], outputs: &[OutputConfig]) -> Vec<FieldError> {
        let mut errors = Vec::new();
        for source in sources {
            let Ok(identity) = source.identity() else {
                continue;
            };
            if self
                .sources
                .get(&source.id)
                .is_some_and(|old| *old != identity)
                || self.outputs.contains_key(&source.id)
                || self.channels.contains_key(&source.id)
            {
                errors.push(FieldError::new(
                    "sources",
                    "identity",
                    "Existing source IDs cannot be retargeted or repurposed",
                ));
            } else {
                self.sources.insert(source.id, identity);
            }
        }
        for output in outputs {
            let identity = OutputIdentity {
                number: output.number,
                device_type: output.device.device_type(),
            };
            if self
                .outputs
                .get(&output.id)
                .is_some_and(|old| old != &identity)
                || self
                    .outputs
                    .iter()
                    .any(|(id, old)| *id != output.id && old == &identity)
                || self.sources.contains_key(&output.id)
                || self.channels.contains_key(&output.id)
            {
                errors.push(FieldError::new(
                    "outputs",
                    "identity",
                    "Existing or retired output IDs/numbers cannot be reassigned",
                ));
            } else {
                self.outputs.insert(output.id, identity);
            }
            if let VirtualDevice::Switch { channels } = &output.device {
                for channel in channels {
                    let identity = ChannelIdentity {
                        output: output.id,
                        number: channel.number,
                        readout: channel.readout.clone(),
                    };
                    if self
                        .channels
                        .get(&channel.id)
                        .is_some_and(|old| old != &identity)
                        || self.channels.iter().any(|(id, old)| {
                            *id != channel.id
                                && old.output == output.id
                                && old.number == channel.number
                        })
                        || self.outputs.contains_key(&channel.id)
                        || self.sources.contains_key(&channel.id)
                    {
                        errors.push(FieldError::new(
                            "outputs",
                            "identity",
                            "Existing or retired channel IDs/numbers cannot move or retarget",
                        ));
                    } else {
                        self.channels.insert(channel.id, identity);
                    }
                }
            }
        }
        errors
    }
}

pub fn normalized_url(input: &str) -> Result<String, &'static str> {
    let mut url = url::Url::parse(input).map_err(|_| "Use an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Use HTTP(S) without credentials, query, or fragment");
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

impl HubConfig {
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            sources: vec![],
            outputs: vec![],
            identities: IdentityLedger::default(),
        }
    }
    pub fn source_type(&self, id: Uuid) -> Option<DeviceType> {
        match &self.sources.iter().find(|s| s.id == id)?.backend {
            SourceBackend::Native { device, .. } => Some(device.device_type()),
            SourceBackend::Alpaca { device_type, .. }
            | SourceBackend::Com { device_type, .. }
            | SourceBackend::Simulated { device_type } => Some(*device_type),
            SourceBackend::Virtual { output } => self
                .outputs
                .iter()
                .find(|o| o.id == *output)
                .map(|o| o.device.device_type()),
        }
    }
    pub fn validate(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let mut error = |path: String, code: &str, message: &str| {
            errors.push(FieldError::new(path, code, message))
        };
        if self.schema_version != SCHEMA_VERSION {
            error(
                "schemaVersion".into(),
                "version",
                "Unsupported hub schema version; migrate explicitly before loading",
            );
        }
        if self.revision.is_nil() {
            error("revision".into(), "identity", "Revision cannot be nil");
        }
        if self.instance_id.is_nil() {
            error("instanceId".into(), "identity", "Instance ID cannot be nil");
        }
        if self.sources.len() > 256 || self.outputs.len() > 256 {
            error(
                "sources".into(),
                "limit",
                "At most 256 sources and outputs are supported",
            );
            // Bound graph traversal before descending into user-controlled data.
            return errors;
        }
        let mut ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for (i, source) in self.sources.iter().enumerate() {
            let p = format!("sources[{i}]");
            if source.id.is_nil() || !ids.insert(source.id) {
                error(
                    format!("{p}.id"),
                    "identity",
                    "Source IDs must be non-nil and unique",
                );
            }
            if source.label.trim().is_empty() || source.label.len() > 200 {
                error(format!("{p}.label"), "label", "Use a label of 1–200 bytes");
            }
            for e in source.polling.validate_timing() {
                error(format!("{p}.polling.{}", e.path), &e.code, &e.message);
            }
            let identity = match &source.backend {
                SourceBackend::Alpaca {
                    base_url,
                    device_type,
                    device_number,
                    credential_reference,
                    ..
                } => {
                    if credential_reference.as_ref().is_some_and(|r| {
                        r.is_empty()
                            || r.len() > 200
                            || !r
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                    }) {
                        error(
                            format!("{p}.backend.credentialReference"),
                            "credential",
                            "Use a local credential reference, never inline credentials",
                        );
                    }
                    match normalized_url(base_url) {
                        Ok(url) => Some(format!("alpaca:{url}:{device_type:?}:{device_number}")),
                        Err(why) => {
                            error(format!("{p}.backend.baseUrl"), "url", why);
                            None
                        }
                    }
                }
                SourceBackend::Native { device, identity } => {
                    if identity.trim().is_empty() {
                        error(
                            format!("{p}.backend.identity"),
                            "identity",
                            "Select a physical device identity",
                        );
                    }
                    // Direct and SDK camera paths must not claim the same camera twice.
                    let _ = device;
                    source.identity().ok()
                }
                SourceBackend::Com {
                    prog_id,
                    device_type,
                    ..
                } => {
                    if prog_id.is_empty()
                        || prog_id.len() > 200
                        || !prog_id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                    {
                        error(
                            format!("{p}.backend.progId"),
                            "identity",
                            "Use a registered COM ProgID",
                        );
                    }
                    Some(format!(
                        "com:{}:{device_type:?}",
                        prog_id.to_ascii_lowercase()
                    ))
                }
                SourceBackend::Virtual { output } => {
                    if !self.outputs.iter().any(|o| o.id == *output) {
                        error(
                            format!("{p}.backend.output"),
                            "reference",
                            "Virtual output does not exist",
                        );
                    }
                    Some(format!("virtual:{output}"))
                }
                SourceBackend::Simulated { .. } => None,
            };
            if identity.is_some_and(|key| !identities.insert(key)) {
                error(
                    format!("{p}.backend"),
                    "duplicate",
                    "Configure a source once and share its ID",
                );
            }
        }
        let mut numbers = BTreeSet::new();
        for (i, output) in self.outputs.iter().enumerate() {
            let p = format!("outputs[{i}]");
            if output.id.is_nil() || !ids.insert(output.id) {
                error(
                    format!("{p}.id"),
                    "identity",
                    "Output IDs must be non-nil and distinct from other IDs",
                );
            }
            if !numbers.insert((output.device.device_type(), output.number)) {
                error(
                    format!("{p}.number"),
                    "identity",
                    "Device number must be unique within its device class",
                );
            }
            if output.label.trim().is_empty() || output.label.len() > 200 {
                error(format!("{p}.label"), "label", "Use a label of 1–200 bytes");
            }
            for source in output.device.sources() {
                if !self.sources.iter().any(|s| s.id == source) {
                    error(
                        format!("{p}.device"),
                        "reference",
                        "Referenced source does not exist",
                    );
                }
            }
            match &output.device {
                VirtualDevice::Safety { members } => {
                    if members.len() > 64 || !members.iter().any(|m| m.enabled) {
                        error(
                            format!("{p}.device.members"),
                            "required",
                            "Use 1–64 memberships with at least one enabled source",
                        );
                    }
                    let mut seen = BTreeSet::new();
                    for (j, member) in members.iter().enumerate() {
                        let path = format!("{p}.device.members[{j}]");
                        if !seen.insert(member.source) {
                            error(
                                path.clone(),
                                "duplicate",
                                "A source may only appear once in a safety output",
                            );
                        }
                        if self.source_type(member.source) != Some(DeviceType::SafetyMonitor) {
                            error(
                                format!("{path}.source"),
                                "type",
                                "Safety inputs must be SafetyMonitor devices",
                            );
                        }
                        if let Some(source) = self.sources.iter().find(|s| s.id == member.source) {
                            for e in member.policy.validate_source(&source.polling) {
                                error(format!("{path}.policy.{}", e.path), &e.code, &e.message);
                            }
                        }
                    }
                }
                VirtualDevice::Switch { channels } => {
                    if channels.is_empty() || channels.len() > 1024 {
                        error(
                            format!("{p}.device.channels"),
                            "required",
                            "Use 1–1024 switch channels",
                        );
                    }
                    let mut channel_numbers = BTreeSet::new();
                    for (j, channel) in channels.iter().enumerate() {
                        let path = format!("{p}.device.channels[{j}]");
                        if channel.id.is_nil()
                            || !ids.insert(channel.id)
                            || !channel_numbers.insert(channel.number)
                        {
                            error(
                                format!("{path}.id"),
                                "identity",
                                "Use unique non-nil channel IDs and distinct channel numbers",
                            );
                        }
                        if channel.label.trim().is_empty()
                            || channel.label.len() > 200
                            || channel.units.len() > 80
                        {
                            error(
                                path.clone(),
                                "label",
                                "Channel label/units are missing or too long",
                            );
                        }
                        if !channel.minimum.is_finite()
                            || !channel.maximum.is_finite()
                            || !channel.step.is_finite()
                            || channel.minimum > channel.maximum
                            || channel.step <= 0.0
                        {
                            error(
                                path.clone(),
                                "range",
                                "Use finite ordered bounds and a positive step",
                            );
                        }
                        for e in self.validate_readout(&channel.readout) {
                            error(format!("{path}.readout"), &e.code, &e.message);
                        }
                    }
                }
                VirtualDevice::Weather { measurements } => {
                    if measurements.is_empty() {
                        error(
                            format!("{p}.device.measurements"),
                            "required",
                            "Select at least one weather measurement",
                        );
                    }
                    for (metric, measurement) in measurements {
                        let path = format!("{p}.device.measurements.{metric:?}");
                        if measurement.sources.is_empty() || measurement.sources.len() > 16 {
                            error(
                                path.clone(),
                                "required",
                                "Select 1–16 sources in fallback order",
                            );
                        }
                        if !measurement.maximum_age_seconds.is_finite()
                            || !(0.1..=3600.0).contains(&measurement.maximum_age_seconds)
                            || !measurement.average_seconds.is_finite()
                            || !(0.0..=3600.0).contains(&measurement.average_seconds)
                        {
                            error(
                                path.clone(),
                                "range",
                                "Use a maximum age of 0.1–3600 seconds and averaging of 0–3600 seconds",
                            );
                        }
                        for readout in &measurement.sources {
                            for e in self.validate_readout(readout) {
                                error(path.clone(), &e.code, &e.message);
                            }
                        }
                    }
                }
                VirtualDevice::Proxy {
                    source,
                    device_type,
                } => {
                    if self.source_type(*source) != Some(*device_type) {
                        error(
                            format!("{p}.device.source"),
                            "type",
                            "Proxy device type must match its source",
                        );
                    }
                }
            }
        }
        // Source -> output -> source forms a bipartite graph; use IDs only after
        // collecting identity errors, and bound traversal with active/done sets.
        let edges: BTreeMap<Uuid, Vec<Uuid>> = self
            .sources
            .iter()
            .map(|s| {
                (
                    s.id,
                    match s.backend {
                        SourceBackend::Virtual { output } => vec![output],
                        _ => vec![],
                    },
                )
            })
            .chain(self.outputs.iter().map(|o| (o.id, o.device.sources())))
            .collect();
        fn visit(
            id: Uuid,
            edges: &BTreeMap<Uuid, Vec<Uuid>>,
            active: &mut BTreeSet<Uuid>,
            done: &mut BTreeSet<Uuid>,
        ) -> bool {
            if done.contains(&id) {
                return false;
            }
            if !active.insert(id) {
                return true;
            }
            if edges
                .get(&id)
                .is_some_and(|next| next.iter().any(|n| visit(*n, edges, active, done)))
            {
                return true;
            }
            active.remove(&id);
            done.insert(id);
            false
        }
        let mut done = BTreeSet::new();
        if edges
            .keys()
            .any(|id| visit(*id, &edges, &mut BTreeSet::new(), &mut done))
        {
            error(
                "outputs".into(),
                "cycle",
                "Virtual devices must not depend on themselves, directly or indirectly",
            );
        }
        errors
    }
    fn validate_readout(&self, readout: &Readout) -> Vec<FieldError> {
        match readout {
            Readout::Channel { source, .. }
                if self.source_type(*source) != Some(DeviceType::Switch) =>
            {
                vec![FieldError::new(
                    "",
                    "type",
                    "Channel readouts require a Switch source",
                )]
            }
            Readout::Property { source, property } => {
                if self.source_type(*source).is_none()
                    || property.is_empty()
                    || property.len() > 80
                    || !property.chars().all(|c| c.is_ascii_lowercase())
                {
                    vec![FieldError::new(
                        "",
                        "property",
                        "Select a source and a lowercase scalar property",
                    )]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }
    /// Export omits credential references as well as never containing secrets.
    pub fn export(&self) -> Self {
        let mut result = self.clone();
        for source in &mut result.sources {
            if let SourceBackend::Alpaca {
                credential_reference,
                ..
            } = &mut source.backend
            {
                *credential_reference = None;
            }
        }
        result
    }
}

#[derive(Debug)]
pub enum ApplyError {
    Invalid(Vec<FieldError>),
    Conflict,
    Connected,
    Io(std::io::Error),
}

/// Callers must hold their operation/configuration gate around apply and lease
/// changes. The store handles revisions, persistence and atomic publication.
pub struct ConfigStore {
    path: Option<PathBuf>,
    current: Mutex<HubConfig>,
}
impl ConfigStore {
    pub fn new(path: Option<PathBuf>, mut initial: HubConfig) -> Result<Self, ApplyError> {
        let mut errors = initial.validate();
        errors.extend(
            initial
                .identities
                .register(&initial.sources, &initial.outputs),
        );
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        Ok(Self {
            path,
            current: Mutex::new(initial),
        })
    }
    pub fn load(path: &Path) -> Result<Self, ApplyError> {
        if std::fs::metadata(path).map_err(ApplyError::Io)?.len() > 4 * 1024 * 1024 {
            return Err(ApplyError::Invalid(vec![FieldError::new(
                "",
                "limit",
                "Hub configuration exceeds 4 MiB",
            )]));
        }
        let bytes = std::fs::read(path).map_err(ApplyError::Io)?;
        let initial = serde_json::from_slice(&bytes).map_err(|_| {
            ApplyError::Invalid(vec![FieldError::new(
                "",
                "document",
                "Invalid hub configuration JSON or unknown fields",
            )])
        })?;
        Self::new(Some(path.to_path_buf()), initial)
    }
    pub fn snapshot(&self) -> HubConfig {
        self.current.lock().unwrap().clone()
    }
    pub fn apply(
        &self,
        expected_revision: Uuid,
        mut next: HubConfig,
        connected: bool,
    ) -> Result<HubConfig, ApplyError> {
        let errors = next.validate();
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        let mut current = self.current.lock().unwrap();
        if current.revision != expected_revision || next.revision != expected_revision {
            return Err(ApplyError::Conflict);
        }
        if connected {
            return Err(ApplyError::Connected);
        }
        let mut errors = Vec::new();
        if current.instance_id != next.instance_id {
            errors.push(FieldError::new(
                "instanceId",
                "identity",
                "Instance identity cannot change during an update",
            ));
        }
        if next.identities != current.identities {
            errors.push(FieldError::new(
                "identities",
                "identity",
                "Identity history is managed by the hub and cannot be edited",
            ));
        }
        errors.extend(next.identities.register(&next.sources, &next.outputs));
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        next.revision = Uuid::new_v4();
        if let Some(path) = &self.path {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(ApplyError::Io)?;
            let bytes = serde_json::to_vec_pretty(&next)
                .map_err(|e| ApplyError::Io(std::io::Error::other(e)))?;
            temp.write_all(&bytes).map_err(ApplyError::Io)?;
            temp.as_file().sync_all().map_err(ApplyError::Io)?;
            temp.persist(path).map_err(|e| ApplyError::Io(e.error))?;
        }
        *current = next.clone();
        Ok(next)
    }
}
