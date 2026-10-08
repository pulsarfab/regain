//! Persisted hub identities and revision-checked configuration replacement.
use crate::parameters::{FieldError, PollPolicy, SafetyPolicy};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LABEL_CHARS: usize = 200;
pub const MAX_DEVICES: usize = 256;
pub const MAX_SAFETY_MEMBERS: usize = 64;
pub const MAX_SWITCH_CHANNELS: usize = 1024;
pub const MAX_MEASUREMENT_SOURCES: usize = 16;
pub const MAX_HISTORY_SECONDS: f64 = 3600.0;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionPolicy {
    /// Read an upstream connection managed by another application. Never disconnect it.
    #[default]
    ExternallyManaged,
    /// Connect while the hub holds leases; release only connections the hub opened.
    Managed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Bitness {
    X86,
    X64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SourceBackend {
    /// # Direct Regain driver
    /// Use the existing Rust worker and its device-specific recovery behavior.
    #[schemars(extend("x-regain" = {"requiresCapability":"nativeSources"}, "allOf" = [
        {"if":{"properties":{"device":{"enum":["camera-direct","camera-sdk"]}}},
         "then":{"required":["camera"],"properties":{"camera":{"type":"object"}}},
         "else":{"properties":{"camera":{"type":"null"}}}},
        {"if":{"properties":{"device":{"const":"camera-sdk"}}},
         "then":{"properties":{"camera":{"properties":{"sdkFallback":{"const":false}}}}}}
    ]))]
    Native {
        /// Hardware driver and backend to use.
        #[schemars(extend("x-regain" = {"enumCapabilities":{"camera-direct":"nativeCameraSources","camera-sdk":"nativeCameraSources"}}))]
        device: NativeDevice,
        /// Stable hardware serial or device identity; never a discovery-list index.
        #[schemars(length(min = 1, max = MAX_LABEL_CHARS))]
        identity: String,
        /// Optional metadata for a direct EFW only. Leave absent to use numbered
        /// filter names and zero offsets. Explicit arrays must match hardware
        /// slots; calibration/reconnect never silently replace saved metadata.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(title = "Direct EFW filter metadata")]
        filter_wheel: Option<crate::filterwheel::NativeFilterWheelMetadata>,
        /// Required only for native cameras. Shared recovery definitions retain
        /// the existing camera defaults; camera setup remains capability gated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(extend("x-regain" = {"requiresCapability":"nativeCameraSources"}))]
        camera: Option<crate::camera::config::NativeCameraConfig>,
    },
    /// # Remote Alpaca
    /// Read or control an ASCOM Alpaca device over HTTP or HTTPS.
    #[schemars(extend("x-regain" = {"requiresCapability":"alpacaSources"}))]
    Alpaca {
        /// Server base URL. Credentials, query strings, and fragments are not allowed.
        #[schemars(title = "Server URL", url)]
        base_url: String,
        /// Interface index on this host for a literal IPv6 link-local server.
        /// Keep the URL unscoped and set this separately. Other URLs omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(title = "IPv6 interface scope", range(min = 1, max = 4294967295u64))]
        scope_id: Option<u32>,
        /// ASCOM device class advertised by the source.
        device_type: DeviceType,
        /// Stable device number on that server.
        #[schemars(range(min = 0, max = 4294967295u64))]
        device_number: u32,
        /// Optional upstream catalog identity. Pinned sources verify this identity
        /// before device requests; changing or removing a saved pin is refused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            title = "Upstream unique ID",
            length(min = 1, max = 256),
            regex(pattern = "^[ -~]+$")
        )]
        unique_id: Option<String>,
        /// Whether the hub or another application manages the upstream connection.
        #[serde(default)]
        connection_policy: ConnectionPolicy,
        /// Name of a credential in local protected storage. Never enter the credential itself.
        #[serde(default)]
        #[schemars(length(min = 1, max = MAX_LABEL_CHARS), extend("x-regain" = {"sensitive":true,"export":"omit"}))]
        credential_reference: Option<String>,
    },
    /// # Windows ASCOM driver
    /// Import a registered driver in an isolated Windows COM worker.
    #[schemars(extend("x-regain" = {"requiresCapability":"comSources"}))]
    Com {
        /// Registered ASCOM ProgID, for example ASCOM.Example.SafetyMonitor.
        #[schemars(title = "Driver ProgID", length(min = 1, max = MAX_LABEL_CHARS))]
        prog_id: String,
        /// ASCOM interface exposed by this source.
        #[schemars(extend("x-regain" = {"enumCapabilities":{"camera":"cameraComSources"}}))]
        device_type: DeviceType,
        /// Match the architecture in which the source driver is registered.
        #[schemars(extend("x-regain" = {"enumCapabilities":{"x86":"comX86Sources","x64":"comX64Sources"}}))]
        bitness: Bitness,
        /// Whether the hub or another application manages the upstream connection.
        #[serde(default)]
        connection_policy: ConnectionPolicy,
    },
    /// # Another hub device
    /// Reference a virtual output by its stable ID. Cyclic references are rejected.
    #[schemars(extend("x-regain" = {"requiresCapability":"virtualSources"}))]
    Virtual {
        /// Stable ID of the virtual output to read.
        #[schemars(extend("x-regain" = {"reference":"output"}))]
        output: Uuid,
    },
    /// # Simulation
    /// A visibly labelled simulated source for configuration and failure testing.
    #[schemars(extend("x-regain" = {"requiresCapability":"simulation"}))]
    Simulated {
        /// Simulated device interface.
        #[schemars(extend("x-regain" = {"enumCapabilities":{"camera":"cameraSimulation"}}))]
        device_type: DeviceType,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceConfig {
    /// Stable source ID. Refer to the same ID to share this source.
    #[schemars(extend("readOnly" = true))]
    pub id: Uuid,
    /// Display name for this source; changing it does not change device identity.
    #[schemars(length(min = 1, max = MAX_LABEL_CHARS))]
    pub label: String,
    /// Select the source transport and its device identity.
    pub backend: SourceBackend,
    /// Shared read cadence, deadlines, and retry policy. Writes are never blindly retried.
    #[serde(default)]
    pub polling: PollPolicy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Readout {
    /// # Switch channel
    /// Read one channel from a Switch source, retaining its access permissions.
    Channel {
        /// Stable ID of the Switch source.
        #[schemars(extend("x-regain" = {"reference":"source"}))]
        source: Uuid,
        /// Upstream channel number, not this output's channel number.
        channel: u32,
        /// Explicit unit of an upstream channel, needed for weather mappings.
        /// No conversion is performed; use the target metric's canonical unit.
        #[serde(default)]
        #[schemars(length(max = 80))]
        unit: Option<String>,
    },
    /// # Device property
    /// Read a scalar property such as temperature from a compatible source.
    Property {
        /// Stable source ID.
        #[schemars(extend("x-regain" = {"reference":"source"}))]
        source: Uuid,
        /// Lowercase property name advertised by the source, for example temperature.
        #[schemars(length(min = 1, max = 80), regex(pattern = "^[a-z]+$"))]
        property: String,
        /// Unit assertion for a property without standard unit metadata.
        /// A standard property's known unit cannot be overridden.
        #[serde(default)]
        #[schemars(length(max = 80))]
        unit: Option<String>,
    },
}
impl Readout {
    pub fn source(&self) -> Uuid {
        match self {
            Self::Channel { source, .. } | Self::Property { source, .. } => *source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafetyMember {
    /// Required SafetyMonitor source. Each enabled membership participates in AND aggregation.
    #[schemars(extend("x-regain" = {"reference":"source"}))]
    pub source: Uuid,
    /// Include this source in the safety decision. At least one source must be enabled.
    pub enabled: bool,
    /// This membership's confirmation and freshness rules, independent of other outputs.
    #[serde(default)]
    pub policy: SafetyPolicy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwitchChannel {
    /// Stable channel ID. Removing a channel never makes its number available for reuse.
    #[schemars(extend("readOnly" = true))]
    pub id: Uuid,
    /// Persisted output channel number. It does not change when channels are reordered.
    #[schemars(range(max = 1023))]
    #[schemars(extend("x-regain" = {"immutableAfterCreate":true}))]
    pub number: u32,
    /// Name shown to clients for this channel.
    #[schemars(length(min = 1, max = MAX_LABEL_CHARS))]
    pub label: String,
    /// Upstream channel or scalar property; an existing channel cannot be silently retargeted.
    pub readout: Readout,
    /// Request writes only when the source supports them; cannot make a read-only sensor writable.
    #[schemars(extend("x-regain" = {"requiresCapability":"writeReadout"}))]
    pub writable: bool,
    /// Minimum exposed value. Runtime writes must also satisfy the source's own bounds.
    pub minimum: f64,
    /// Maximum exposed value; must be at least the minimum.
    pub maximum: f64,
    /// Positive value increment supported by this channel.
    #[schemars(extend("exclusiveMinimum" = 0))]
    pub step: f64,
    /// Unit label for display; no implicit unit conversion is performed.
    #[schemars(length(max = 80))]
    pub units: String,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Measurement {
    /// First fresh valid source wins; changing source clears the averaging window.
    #[schemars(length(min = 1, max = MAX_MEASUREMENT_SOURCES))]
    pub sources: Vec<Readout>,
    /// Hard sample age limit. An expired sample is unavailable and may trigger fallback.
    #[schemars(range(min = 0.1, max = MAX_HISTORY_SECONDS), extend("x-regain" = {"units":"s"}))]
    pub maximum_age_seconds: f64,
    /// Averaging interval; zero returns the latest fresh sample. All non-gust metrics
    /// in an output use the same interval. Source changes clear history; wind gust
    /// preserves the upstream three-second peak over two minutes without re-averaging.
    #[schemars(range(min = 0.0, max = MAX_HISTORY_SECONDS), extend("x-regain" = {"units":"s"}))]
    pub average_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VirtualDevice {
    /// # Combined safety
    /// Require every enabled source to permit operation, with per-source diagnostics.
    Safety {
        /// Required safety inputs and their independent policies.
        #[schemars(length(min = 1, max = MAX_SAFETY_MEMBERS))]
        members: Vec<SafetyMember>,
    },
    /// # Combined switches and gauges
    /// Publish selected read-only sensors and writable controls as one Switch device.
    Switch {
        /// Stable channel assignments and their source mappings.
        #[schemars(length(min = 1, max = MAX_SWITCH_CHANNELS))]
        channels: Vec<SwitchChannel>,
    },
    /// # Combined weather
    /// Select a source, freshness limit, and fallback order for each weather measurement.
    Weather {
        /// Measurement names follow the ASCOM ObservingConditions interface.
        #[schemars(extend("minProperties" = 1))]
        measurements: BTreeMap<WeatherMetric, Measurement>,
    },
    /// # Republish a device
    /// Preserve the source's interface and capabilities through another frontend.
    #[schemars(extend("x-regain" = {"requiresCapability":"proxyOutputs"}))]
    Proxy {
        /// Stable ID of the upstream source.
        #[schemars(extend("x-regain" = {"reference":"source"}))]
        source: Uuid,
        /// Must match the upstream device class.
        #[schemars(extend("x-regain" = {"enumCapabilities":{"camera":"cameraOutputs","switch":"broaderProxyOutputs","safetymonitor":"broaderProxyOutputs","observingconditions":"broaderProxyOutputs","focuser":"focuserOutputs","rotator":"rotatorOutputs","filterwheel":"filterWheelOutputs","covercalibrator":"coverCalibratorOutputs"}}))]
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
    pub(crate) fn sources(&self) -> Vec<Uuid> {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputConfig {
    /// Stable virtual-device ID used by all frontends.
    #[schemars(extend("readOnly" = true))]
    pub id: Uuid,
    /// Stable device number within its ASCOM device class; never derived from display order.
    #[schemars(extend("x-regain" = {"immutableAfterCreate":true}))]
    pub number: u32,
    /// Name shown in equipment lists and setup dialogs.
    #[schemars(length(min = 1, max = MAX_LABEL_CHARS))]
    pub label: String,
    /// Select how this output combines or republishes its inputs.
    pub device: VirtualDevice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HubConfig {
    /// Persisted configuration format; unsupported versions require explicit migration.
    #[schemars(extend("const" = SCHEMA_VERSION, "readOnly" = true))]
    pub schema_version: u32,
    /// Revision returned by the hub. An update must supply this exact revision.
    #[schemars(extend("readOnly" = true))]
    pub revision: Uuid,
    /// Stable hub instance identity; never copied from another installation.
    #[schemars(extend("readOnly" = true))]
    pub instance_id: Uuid,
    /// Sources shared by virtual devices. Configure each physical/remote source once.
    #[schemars(length(max = MAX_DEVICES))]
    pub sources: Vec<SourceConfig>,
    /// Virtual devices published through NINA, Alpaca, or native ASCOM.
    #[schemars(length(max = MAX_DEVICES))]
    pub outputs: Vec<OutputConfig>,
    /// Explicit calibrated motion groups. These do not create standard device outputs.
    #[serde(default)]
    #[schemars(length(max = 64))]
    #[schemars(extend("x-regain" = {"requiresCapability":"focuserGroups"}))]
    pub focuser_groups: Vec<crate::coordination::FocuserGroupConfig>,
    /// Explicit camera bursts; each member produces a separate image/result.
    #[serde(default)]
    #[schemars(length(max = 64))]
    #[schemars(extend("x-regain" = {"requiresCapability":"cameraGroups"}))]
    pub camera_groups: Vec<crate::coordination::CameraGroupConfig>,
    /// Owned by the store. Retired IDs remain reserved after deletion/restart.
    #[serde(default)]
    #[schemars(extend("readOnly" = true, "x-regain" = {"hidden":true}))]
    pub identities: IdentityLedger,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityLedger {
    sources: BTreeMap<Uuid, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    alpaca_pins: BTreeMap<Uuid, String>,
    outputs: BTreeMap<Uuid, OutputIdentity>,
    channels: BTreeMap<Uuid, ChannelIdentity>,
    #[serde(default)]
    groups: BTreeSet<Uuid>,
    #[serde(default)]
    camera_groups: BTreeSet<Uuid>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputIdentity {
    number: u32,
    device_type: DeviceType,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
                scope_id,
                device_type,
                device_number,
                ..
            } => format!(
                "alpaca:{}:{device_type:?}:{device_number}",
                normalized_alpaca_url(base_url, *scope_id)?
            ),
            SourceBackend::Native {
                device, identity, ..
            } => {
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
    pub(crate) fn output_numbers(&self) -> BTreeSet<(DeviceType, u32)> {
        self.outputs
            .values()
            .map(|output| (output.device_type, output.number))
            .collect()
    }

    pub(crate) fn register(
        &mut self,
        sources: &[SourceConfig],
        outputs: &[OutputConfig],
        groups: &[crate::coordination::FocuserGroupConfig],
        camera_groups: &[crate::coordination::CameraGroupConfig],
    ) -> Vec<FieldError> {
        let mut errors = Vec::new();
        for source in sources {
            let Ok(identity) = source.identity() else {
                continue;
            };
            let pin = match &source.backend {
                SourceBackend::Alpaca { unique_id, .. } => {
                    unique_id.as_deref().map(normalize_alpaca_id)
                }
                _ => None,
            };
            if self
                .sources
                .get(&source.id)
                .is_some_and(|old| *old != identity)
                || self
                    .alpaca_pins
                    .get(&source.id)
                    .is_some_and(|old| pin.as_ref() != Some(old))
                || self.outputs.contains_key(&source.id)
                || self.channels.contains_key(&source.id)
                || self.groups.contains(&source.id)
                || self.camera_groups.contains(&source.id)
            {
                errors.push(FieldError::new(
                    "sources",
                    "identity",
                    "Existing source IDs cannot be retargeted or repurposed",
                ));
            } else {
                self.sources.insert(source.id, identity);
                if let Some(pin) = pin {
                    self.alpaca_pins.insert(source.id, pin);
                }
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
                || self.groups.contains(&output.id)
                || self.camera_groups.contains(&output.id)
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
                        || self.groups.contains(&channel.id)
                        || self.camera_groups.contains(&channel.id)
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
        for group in groups {
            if self.sources.contains_key(&group.id)
                || self.outputs.contains_key(&group.id)
                || self.channels.contains_key(&group.id)
                || self.camera_groups.contains(&group.id)
            {
                errors.push(FieldError::new(
                    "focuserGroups",
                    "identity",
                    "Group IDs cannot repurpose source, output or channel identities",
                ));
            } else {
                self.groups.insert(group.id);
            }
        }
        for group in camera_groups {
            if self.sources.contains_key(&group.id)
                || self.outputs.contains_key(&group.id)
                || self.channels.contains_key(&group.id)
                || self.groups.contains(&group.id)
            {
                errors.push(FieldError::new(
                    "cameraGroups",
                    "identity",
                    "Camera group IDs cannot repurpose other identities",
                ));
            } else {
                self.camera_groups.insert(group.id);
            }
        }
        errors
    }
}

pub(crate) fn valid_alpaca_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && value.bytes().all(|b| b.is_ascii() && !b.is_ascii_control())
}
pub(crate) fn normalize_alpaca_id(value: &str) -> String {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .unwrap_or_else(|_| value.to_owned())
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
pub fn normalized_alpaca_url(input: &str, scope: Option<u32>) -> Result<String, &'static str> {
    let normalized = normalized_url(input)?;
    let root = url::Url::parse(&normalized).map_err(|_| "Use an absolute HTTP(S) URL")?;
    let address = crate::alpaca::http::scoped_address(&root, scope)?;
    Ok(match address {
        Some(address) => format!("{normalized}|scope={}", address.scope_id()),
        None => normalized,
    })
}

impl HubConfig {
    /// Include retired slots so clients never shift channel identities after an
    /// edit. Gaps are exposed as unavailable, read-only tombstones.
    pub fn switch_slot_count(&self, output: Uuid) -> u32 {
        let active = self
            .outputs
            .iter()
            .find(|item| item.id == output)
            .into_iter()
            .flat_map(|item| match &item.device {
                VirtualDevice::Switch { channels } => channels
                    .iter()
                    .map(|channel| channel.number)
                    .collect::<Vec<_>>(),
                _ => vec![],
            });
        active
            .chain(
                self.identities
                    .channels
                    .values()
                    .filter(|channel| channel.output == output)
                    .map(|channel| channel.number),
            )
            .max()
            .map_or(0, |number| number.saturating_add(1))
    }
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            sources: vec![],
            outputs: vec![],
            focuser_groups: vec![],
            camera_groups: vec![],
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
        if self.identities.alpaca_pins.iter().any(|(id, pin)| {
            id.is_nil() || !valid_alpaca_id(pin) || !self.identities.sources.contains_key(id)
        }) {
            error(
                "identities.alpacaPins".into(),
                "identity",
                "Saved Alpaca pins must refer to known source IDs and valid catalog identities",
            );
        }
        if self
            .identities
            .channels
            .values()
            .any(|channel| channel.number >= MAX_SWITCH_CHANNELS as u32)
        {
            error(
                "identities.channels".into(),
                "range",
                "Retired switch slots must stay within the supported channel range",
            );
        }
        if self.sources.len() > MAX_DEVICES
            || self.outputs.len() > MAX_DEVICES
            || self.focuser_groups.len() > 64
            || self.camera_groups.len() > 64
        {
            error(
                "sources".into(),
                "limit",
                "At most 256 sources, 256 outputs and 64 groups of each kind are supported",
            );
            // Bound graph traversal before descending into user-controlled data.
            return errors;
        }
        let mut ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut alpaca_pins = BTreeSet::new();
        for (i, source) in self.sources.iter().enumerate() {
            let p = format!("sources[{i}]");
            if source.id.is_nil() || !ids.insert(source.id) {
                error(
                    format!("{p}.id"),
                    "identity",
                    "Source IDs must be non-nil and unique",
                );
            }
            if source.label.trim().is_empty() || source.label.chars().count() > MAX_LABEL_CHARS {
                error(format!("{p}.label"), "label", "Use a label of 1–200 bytes");
            }
            for e in source.polling.validate_timing() {
                error(format!("{p}.polling.{}", e.path), &e.code, &e.message);
            }
            let identity = match &source.backend {
                SourceBackend::Alpaca {
                    base_url,
                    scope_id,
                    device_type,
                    device_number,
                    unique_id,
                    credential_reference,
                    ..
                } => {
                    if unique_id.as_deref().is_some_and(|id| !valid_alpaca_id(id)) {
                        error(
                            format!("{p}.backend.uniqueId"),
                            "identity",
                            "Use a nonempty ASCII catalog identity of at most 256 bytes",
                        );
                    }
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
                    match normalized_alpaca_url(base_url, *scope_id) {
                        Ok(url) => {
                            if let Some(id) = unique_id
                                && !alpaca_pins.insert(normalize_alpaca_id(id))
                            {
                                error(
                                    format!("{p}.backend.uniqueId"),
                                    "duplicate",
                                    "Configure a pinned Alpaca device once and share its source ID",
                                );
                            }
                            Some(format!("alpaca:{url}:{device_type:?}:{device_number}"))
                        }
                        Err(why) => {
                            error(format!("{p}.backend.baseUrl"), "url", why);
                            None
                        }
                    }
                }
                SourceBackend::Native {
                    device,
                    identity,
                    filter_wheel,
                    camera,
                } => {
                    if identity.trim().is_empty() || identity.chars().count() > MAX_LABEL_CHARS {
                        error(
                            format!("{p}.backend.identity"),
                            "identity",
                            "Select a physical device identity",
                        );
                    }
                    if let Some(metadata) = filter_wheel {
                        if *device != NativeDevice::Efw {
                            error(
                                format!("{p}.backend.filterWheel"),
                                "type",
                                "Filter metadata is only supported by direct EFW sources",
                            );
                        }
                        for field in metadata.validate() {
                            error(
                                format!("{p}.backend.filterWheel.{}", field.path),
                                &field.code,
                                &field.message,
                            );
                        }
                    }
                    let is_camera = device.device_type() == DeviceType::Camera;
                    match camera {
                        Some(camera) if is_camera => {
                            for field in camera.validate(*device == NativeDevice::CameraDirect) {
                                error(
                                    format!("{p}.backend.camera.{}", field.path),
                                    &field.code,
                                    &field.message,
                                );
                            }
                        }
                        Some(_) => error(
                            format!("{p}.backend.camera"),
                            "type",
                            "Camera settings are only supported by native camera sources",
                        ),
                        None if is_camera => error(
                            format!("{p}.backend.camera"),
                            "required",
                            "Select native camera settings",
                        ),
                        None => {}
                    }
                    // Direct and SDK camera paths must not claim the same camera twice.
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
                    if self.outputs.iter().any(|output| {
                        crate::ascom_export::prog_id(
                            self.instance_id,
                            output.id,
                            output.device.device_type(),
                        )
                        .is_some_and(|own| own.eq_ignore_ascii_case(prog_id))
                    }) {
                        error(
                            format!("{p}.backend.progId"),
                            "cycle",
                            "Use a virtual source for an output of this hub",
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
            if output.label.trim().is_empty() || output.label.chars().count() > MAX_LABEL_CHARS {
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
                    if members.len() > MAX_SAFETY_MEMBERS || !members.iter().any(|m| m.enabled) {
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
                    if channels.is_empty() || channels.len() > MAX_SWITCH_CHANNELS {
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
                            || channel.number >= MAX_SWITCH_CHANNELS as u32
                        {
                            error(
                                format!("{path}.id"),
                                "identity",
                                "Use unique non-nil channel IDs and distinct channel numbers",
                            );
                        }
                        if channel.label.trim().is_empty()
                            || channel.label.chars().count() > MAX_LABEL_CHARS
                            || channel.units.chars().count() > 80
                        {
                            error(
                                path.clone(),
                                "label",
                                "Channel label/units are missing or too long",
                            );
                        }
                        if crate::switch::Grid::new(channel.minimum, channel.maximum, channel.step)
                            .is_err()
                        {
                            error(
                                path.clone(),
                                "range",
                                "Use finite increasing bounds spanning whole positive steps",
                            );
                        }
                        for e in self.validate_readout(&channel.readout) {
                            error(format!("{path}.readout"), &e.code, &e.message);
                        }
                        if channel.writable && matches!(channel.readout, Readout::Property { .. }) {
                            error(
                                format!("{path}.writable"),
                                "capability",
                                "Scalar property gauges are read-only",
                            );
                        }
                    }
                }
                VirtualDevice::Weather { measurements } => {
                    for e in crate::weather::validate_measurements(self, measurements) {
                        error(format!("{p}.device.{}", e.path), &e.code, &e.message);
                    }
                    if measurements.is_empty() {
                        error(
                            format!("{p}.device.measurements"),
                            "required",
                            "Select at least one weather measurement",
                        );
                    }
                    for (metric, measurement) in measurements {
                        let path = format!(
                            "{p}.device.measurements.{}",
                            serde_json::to_value(metric).unwrap().as_str().unwrap()
                        );
                        if measurement.sources.is_empty()
                            || measurement.sources.len() > MAX_MEASUREMENT_SOURCES
                        {
                            error(
                                path.clone(),
                                "required",
                                "Select 1–16 sources in fallback order",
                            );
                        }
                        if !measurement.maximum_age_seconds.is_finite()
                            || !(0.1..=MAX_HISTORY_SECONDS)
                                .contains(&measurement.maximum_age_seconds)
                            || !measurement.average_seconds.is_finite()
                            || !(0.0..=MAX_HISTORY_SECONDS).contains(&measurement.average_seconds)
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
        for (index, group) in self.focuser_groups.iter().enumerate() {
            let path = format!("focuserGroups[{index}]");
            if group.id.is_nil() || !ids.insert(group.id) {
                error(
                    format!("{path}.id"),
                    "identity",
                    "Group IDs must be non-nil and distinct from other IDs",
                );
            }
            if let Err(problem) = group.validate() {
                error(path.clone(), "group", problem.message);
                continue;
            }
            let mut leaves = BTreeSet::new();
            for (member, calibration) in group.members.iter().enumerate() {
                let reference = format!("{path}.members[{member}].source");
                match self.physical_focuser_source(calibration.source) {
                    Ok(leaf) if leaves.insert(leaf) => {}
                    Ok(_) => error(
                        reference,
                        "duplicate",
                        "Group members must resolve to different physical focusers",
                    ),
                    Err(problem) => error(reference, "source", problem),
                }
            }
        }
        for (index, group) in self.camera_groups.iter().enumerate() {
            let path = format!("cameraGroups[{index}]");
            if group.id.is_nil() || !ids.insert(group.id) {
                error(
                    format!("{path}.id"),
                    "identity",
                    "Group IDs must be non-nil and distinct from other IDs",
                );
            }
            if let Err(problem) = group.validate() {
                error(path.clone(), "group", problem.message);
                continue;
            }
            let mut leaves = BTreeSet::new();
            for (member, source) in group.members.iter().enumerate() {
                let reference = format!("{path}.members[{member}]");
                match self.physical_camera_source(*source) {
                    Ok(leaf) if leaves.insert(leaf) => {}
                    Ok(_) => error(
                        reference,
                        "duplicate",
                        "Group members must resolve to different physical cameras",
                    ),
                    Err(problem) => error(reference, "source", problem),
                }
            }
        }
        errors
    }
    /// Follow only typed proxy aliases. Bound traversal independently of
    /// prior graph validation so malformed/cyclic drafts remain safe to inspect.
    pub fn physical_focuser_source(&self, source: Uuid) -> Result<Uuid, &'static str> {
        self.physical_source(source, DeviceType::Focuser)
    }
    pub fn physical_camera_source(&self, source: Uuid) -> Result<Uuid, &'static str> {
        self.physical_source(source, DeviceType::Camera)
    }
    fn physical_source(
        &self,
        mut source: Uuid,
        expected: DeviceType,
    ) -> Result<Uuid, &'static str> {
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(source) || visited.len() > MAX_DEVICES {
                return Err("Source aliases contain a cycle");
            }
            let config = self
                .sources
                .iter()
                .find(|entry| entry.id == source)
                .ok_or("Group source does not exist")?;
            match config.backend {
                SourceBackend::Virtual { output } => {
                    let output = self
                        .outputs
                        .iter()
                        .find(|entry| entry.id == output)
                        .ok_or("Group alias output does not exist")?;
                    match output.device {
                        VirtualDevice::Proxy {
                            source: next,
                            device_type,
                        } if device_type == expected => source = next,
                        _ => {
                            return Err(
                                "Group members must resolve through matching proxy outputs",
                            );
                        }
                    }
                }
                _ if self.source_type(source) == Some(expected) => return Ok(source),
                _ => return Err("Group member source has the wrong device class"),
            }
        }
    }
    fn validate_readout(&self, readout: &Readout) -> Vec<FieldError> {
        let unit = match readout {
            Readout::Channel { unit, .. } | Readout::Property { unit, .. } => unit,
        };
        if unit.as_ref().is_some_and(|unit| unit.chars().count() > 80) {
            return vec![FieldError::new(
                "unit",
                "range",
                "Unit labels are limited to 80 characters",
            )];
        }
        match readout {
            Readout::Channel { channel, .. } if *channel > 32766 => vec![FieldError::new(
                "",
                "range",
                "ASCOM channel IDs must fit the Switch interface range",
            )],
            Readout::Channel { source, .. }
                if self.source_type(*source) != Some(DeviceType::Switch) =>
            {
                vec![FieldError::new(
                    "",
                    "type",
                    "Channel readouts require a Switch source",
                )]
            }
            Readout::Property {
                source, property, ..
            } => {
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
    /// Replacement happened, but its final durability flush failed. Never
    /// report this as rollback or retry with the old expected revision.
    Committed {
        configuration: Box<HubConfig>,
        error: std::io::Error,
    },
}

/// Callers must hold their operation/configuration gate around apply and lease
/// changes. The store handles revisions, persistence and atomic publication.
pub struct ConfigStore {
    identity: Uuid,
    path: Option<PathBuf>,
    current: Mutex<HubConfig>,
}
/// Validated and flushed, but not yet published. Dropping this value removes
/// its temporary file. Only its originating store can commit it.
pub struct PreparedConfig {
    store: Uuid,
    expected: Uuid,
    next: HubConfig,
    staged: Option<tempfile::NamedTempFile>,
}
impl PreparedConfig {
    pub fn configuration(&self) -> &HubConfig {
        &self.next
    }
}
impl ConfigStore {
    /// Explicit first-time setup. Publish a flushed empty configuration without
    /// replacing any existing file, directory or symlink. This starts no host
    /// and configures no equipment, output or implicit simulated fallback.
    pub fn create(path: &Path) -> Result<Self, ApplyError> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(ApplyError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "New hub configuration path must be an absolute file path",
            )));
        }
        let parent = path
            .parent()
            .unwrap()
            .canonicalize()
            .map_err(ApplyError::Io)?;
        let path = parent.join(path.file_name().unwrap());
        let store = Self::new(Some(path.clone()), HubConfig::empty())?;
        let configuration = store.snapshot();
        let mut staged = tempfile::NamedTempFile::new_in(&parent).map_err(ApplyError::Io)?;
        let bytes = serde_json::to_vec_pretty(&configuration)
            .map_err(|error| ApplyError::Io(std::io::Error::other(error)))?;
        staged.write_all(&bytes).map_err(ApplyError::Io)?;
        staged.as_file().sync_all().map_err(ApplyError::Io)?;
        let saved = staged
            .persist_noclobber(&path)
            .map_err(|error| ApplyError::Io(error.error))?;
        let durability = saved.sync_all();
        #[cfg(unix)]
        let durability = durability.and_then(|()| std::fs::File::open(&parent)?.sync_all());
        if let Err(error) = durability {
            return Err(ApplyError::Committed {
                configuration: Box::new(configuration),
                error,
            });
        }
        Ok(store)
    }
    pub fn new(path: Option<PathBuf>, mut initial: HubConfig) -> Result<Self, ApplyError> {
        let mut errors = initial.validate();
        errors.extend(initial.identities.register(
            &initial.sources,
            &initial.outputs,
            &initial.focuser_groups,
            &initial.camera_groups,
        ));
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        Ok(Self {
            identity: Uuid::new_v4(),
            path,
            current: Mutex::new(initial),
        })
    }
    pub fn load(path: &Path) -> Result<Self, ApplyError> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(ApplyError::Io)?
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(ApplyError::Io)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ApplyError::Invalid(vec![FieldError::new(
                "",
                "limit",
                "Hub configuration exceeds 4 MiB",
            )]));
        }
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
        next: HubConfig,
        connected: bool,
    ) -> Result<HubConfig, ApplyError> {
        let prepared = self.prepare(expected_revision, next, connected)?;
        self.commit(prepared)
    }
    pub fn prepare(
        &self,
        expected_revision: Uuid,
        mut next: HubConfig,
        connected: bool,
    ) -> Result<PreparedConfig, ApplyError> {
        let errors = next.validate();
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        let current = self.current.lock().unwrap();
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
        errors.extend(next.identities.register(
            &next.sources,
            &next.outputs,
            &next.focuser_groups,
            &next.camera_groups,
        ));
        if !errors.is_empty() {
            return Err(ApplyError::Invalid(errors));
        }
        next.revision = Uuid::new_v4();
        drop(current);
        let staged = if let Some(path) = &self.path {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(ApplyError::Io)?;
            let bytes = serde_json::to_vec_pretty(&next)
                .map_err(|e| ApplyError::Io(std::io::Error::other(e)))?;
            if bytes.len() > MAX_CONFIG_BYTES {
                return Err(ApplyError::Invalid(vec![FieldError::new(
                    "",
                    "limit",
                    "Hub configuration exceeds 4 MiB",
                )]));
            }
            temp.write_all(&bytes).map_err(ApplyError::Io)?;
            temp.as_file().sync_all().map_err(ApplyError::Io)?;
            Some(temp)
        } else {
            None
        };
        Ok(PreparedConfig {
            store: self.identity,
            expected: expected_revision,
            next,
            staged,
        })
    }
    pub fn commit(&self, prepared: PreparedConfig) -> Result<HubConfig, ApplyError> {
        let mut current = self.current.lock().unwrap();
        if prepared.store != self.identity || current.revision != prepared.expected {
            return Err(ApplyError::Conflict);
        }
        let saved = match prepared.staged {
            Some(temp) => Some(
                temp.persist(self.path.as_ref().expect("Staged file has a destination"))
                    .map_err(|e| ApplyError::Io(e.error))?,
            ),
            None => None,
        };
        *current = prepared.next;
        let configuration = current.clone();
        drop(current);
        if let Some(file) = saved {
            let durability = file.sync_all();
            #[cfg(unix)]
            let durability = durability.and_then(|()| {
                std::fs::File::open(
                    self.path
                        .as_ref()
                        .unwrap()
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new(".")),
                )?
                .sync_all()
            });
            if let Err(error) = durability {
                return Err(ApplyError::Committed {
                    configuration: Box::new(configuration),
                    error,
                });
            }
        }
        Ok(configuration)
    }
}
