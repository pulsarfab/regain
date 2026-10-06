//! Typed filter-wheel commands over the common source session. Names and focus
//! offsets belong to the source; the hub preserves slot order and never infers
//! completion from an acknowledged write or issues calibration on disconnect.
use crate::{
    readout::invalid,
    source::{
        ErrorKind, MAX_SAMPLE_ARRAY_LENGTH, MAX_SAMPLE_TEXT_BYTES, SourceError, SourceHandle,
        Values,
    },
    typed_source::TypedSourceSession,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

/// Resource admission limit, not a hardware slot count. Actual bounds are read
/// from both source arrays before every command; no slot mapping is synthesized.
pub const MAX_FILTER_SLOTS: usize = MAX_SAMPLE_ARRAY_LENGTH;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum FilterWheelProperty {
    Names,
    FocusOffsets,
    Position,
}
impl FilterWheelProperty {
    pub const ALL: [Self; 3] = [Self::Names, Self::FocusOffsets, Self::Position];
    pub fn value_type(self) -> &'static str {
        match self {
            Self::Names => "strings",
            Self::FocusOffsets => "integers",
            Self::Position => "integer",
        }
    }
    pub fn sample_request(self) -> crate::sampling::SampleRequest {
        use crate::sampling::{SampleRequest, SampleType};
        SampleRequest {
            key: self.member().into(),
            member: self.member().into(),
            parameters: Values::new(),
            value_type: match self {
                Self::Names => SampleType::Strings,
                Self::FocusOffsets => SampleType::Int32s,
                Self::Position => SampleType::Number,
            },
            sensor_age: None,
        }
    }
    pub fn member(self) -> &'static str {
        match self {
            Self::Names => "names",
            Self::FocusOffsets => "focusoffsets",
            Self::Position => "position",
        }
    }
    pub fn decode(self, value: &Value) -> Result<FilterWheelValue, SourceError> {
        match self {
            Self::Position => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .filter(|value| (-1..MAX_FILTER_SLOTS as i32).contains(value))
                .map(|value| FilterWheelValue::Integer { value })
                .ok_or_else(bad_reading),
            Self::Names => {
                let array = array(value)?;
                let mut total = 0usize;
                let names = array
                    .iter()
                    .map(|value| {
                        let name = value.as_str().ok_or_else(bad_reading)?;
                        total = total.checked_add(name.len()).ok_or_else(bad_reading)?;
                        if total > MAX_SAMPLE_TEXT_BYTES {
                            return Err(bad_reading());
                        }
                        Ok(name.to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FilterWheelValue::Strings { value: names })
            }
            Self::FocusOffsets => {
                let offsets = array(value)?
                    .iter()
                    .map(|value| {
                        value
                            .as_i64()
                            .and_then(|value| i32::try_from(value).ok())
                            .ok_or_else(bad_reading)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                // ASCOM requires at least one reference filter. Retain signed
                // offsets exactly; never normalize or fabricate a reference.
                if !offsets.contains(&0) {
                    return Err(bad_reading());
                }
                Ok(FilterWheelValue::Integers { value: offsets })
            }
        }
    }
}
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum FilterWheelValue {
    Integer {
        value: i32,
    },
    Strings {
        #[schemars(length(min = 1, max = 1024))]
        value: Vec<String>,
    },
    Integers {
        #[schemars(schema_with = "offset_array_schema", length(min = 1, max = 1024), extend("contains" = {"const": 0}))]
        value: Vec<i32>,
    },
}
pub type FilterWheelSample = crate::readout::TypedSample<FilterWheelValue>;

pub(crate) fn cached_property(
    state: &crate::source::SourceSnapshot,
    property: FilterWheelProperty,
    now: Duration,
) -> Result<FilterWheelSample, SourceError> {
    if !state.transport_connected {
        return Err(SourceError::new(
            ErrorKind::Disconnected,
            "Source is disconnected",
        ));
    }
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    let read = |property: FilterWheelProperty| {
        let key = property.member();
        if let Some(error) = state.sample_errors.get(key) {
            return Err(error.clone());
        }
        let value = property.decode(state.values.get(key).ok_or_else(bad_reading)?)?;
        crate::readout::typed_sample(state, key, now, value)
    };
    // Match live property reads: all metadata must agree before any property
    // claims a valid wheel. Keep the oldest dependency age, never rejuvenate it.
    let names = read(FilterWheelProperty::Names)?;
    let offsets = read(FilterWheelProperty::FocusOffsets)?;
    let FilterWheelValue::Strings {
        value: ref names_value,
    } = names.value
    else {
        unreachable!()
    };
    let FilterWheelValue::Integers {
        value: ref offsets_value,
    } = offsets.value
    else {
        unreachable!()
    };
    if names_value.len() != offsets_value.len() {
        return Err(bad_reading());
    }
    let age = names.age_seconds.max(offsets.age_seconds);
    let mut sample = match property {
        FilterWheelProperty::Names => names,
        FilterWheelProperty::FocusOffsets => offsets,
        FilterWheelProperty::Position => {
            let position = read(property)?;
            let FilterWheelValue::Integer { value } = position.value else {
                unreachable!()
            };
            if value >= names_value.len() as i32 {
                return Err(bad_reading());
            }
            position
        }
    };
    sample.age_seconds = sample.age_seconds.max(age);
    Ok(sample)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterWheelCapabilities {
    pub names: Vec<String>,
    pub focus_offsets: Vec<i32>,
}

/// Saved metadata for a direct wheel whose USB protocol has no filter names or
/// optical offsets. Imported ASCOM/Alpaca drivers continue to own their arrays.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeFilterWheelMetadata {
    /// Filter names in zero-based slot order. Keep this array aligned with offsets.
    #[schemars(length(min = 1, max = 1024))]
    pub names: Vec<String>,
    /// Signed focuser offsets in slot order; at least one must be zero. The hub
    /// reports these values and does not move a focuser when the filter changes.
    #[schemars(schema_with = "offset_array_schema", length(min = 1, max = 1024), extend("contains" = {"const": 0}))]
    pub focus_offsets: Vec<i32>,
}
pub(crate) fn offset_array_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = <Vec<i32> as schemars::JsonSchema>::json_schema(generator);
    // New rows start at the required reference value, rather than the signed
    // Int32 minimum. Both editors consume this same item default and bounds.
    let item = schema.get_mut("items").unwrap().as_object_mut().unwrap();
    item.insert("default".into(), json!(0));
    // `format: int32` is only an annotation in JSON Schema. Explicit bounds
    // enforce the same wire range in independent validators and both editors.
    item.insert("minimum".into(), json!(i32::MIN));
    item.insert("maximum".into(), json!(i32::MAX));
    schema
}
impl NativeFilterWheelMetadata {
    pub fn validate(&self) -> Vec<crate::parameters::FieldError> {
        use crate::parameters::FieldError;
        let mut errors = Vec::new();
        if FilterWheelProperty::Names
            .decode(&json!(self.names))
            .is_err()
        {
            errors.push(FieldError::new(
                "names",
                "filterMetadata",
                "Use 1–1024 names with at most one MiB of UTF-8 text",
            ));
        }
        if FilterWheelProperty::FocusOffsets
            .decode(&json!(self.focus_offsets))
            .is_err()
        {
            errors.push(FieldError::new(
                "focusOffsets",
                "filterMetadata",
                "Use 1–1024 signed Int32 offsets with at least one zero reference",
            ));
        }
        if self.names.len() != self.focus_offsets.len() {
            errors.push(FieldError::new(
                "focusOffsets",
                "filterMetadata",
                "Names and focus offsets must have matching slot counts",
            ));
        }
        errors
    }
    pub(crate) fn arrays(&self, slots: usize) -> Result<(Value, Value), SourceError> {
        if self.names.len() != slots || !self.validate().is_empty() {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Saved filter metadata does not match the wheel slots",
            ));
        }
        Ok((json!(self.names), json!(self.focus_offsets)))
    }
}

pub struct FilterWheelController {
    source: Arc<SourceHandle>,
    connection_timeout: Duration,
}
impl FilterWheelController {
    pub(crate) fn source(&self) -> &Arc<SourceHandle> {
        &self.source
    }
    /// Construction performs no I/O. Bound initial source readiness and the
    /// required array/position reads with the source's connection deadline.
    pub fn new(
        source: Arc<SourceHandle>,
        connection_timeout: Duration,
    ) -> Result<Self, SourceError> {
        if connection_timeout.is_zero() || connection_timeout > Duration::from_secs(300) {
            return Err(invalid("Invalid filter wheel connection deadline"));
        }
        Ok(Self {
            source,
            connection_timeout,
        })
    }
    pub async fn connect(&self) -> Result<FilterWheelSession, SourceError> {
        tokio::time::timeout(self.connection_timeout, async {
            let session = FilterWheelSession {
                source: TypedSourceSession::connect(self.source.clone()).await?,
            };
            // Moving (-1) is a valid initial state, not a connection failure.
            session.position().await?;
            Ok(session)
        })
        .await
        .map_err(|_| {
            SourceError::new(
                ErrorKind::Unavailable,
                "Filter wheel connection deadline expired",
            )
        })?
    }
}
pub struct FilterWheelSession {
    source: TypedSourceSession,
}
impl FilterWheelSession {
    pub(crate) fn cached_sample(
        &self,
        property: FilterWheelProperty,
        now: Duration,
    ) -> Result<FilterWheelSample, SourceError> {
        cached_property(&self.source.snapshot()?, property, now)
    }
    pub(crate) fn device_state(&self, now: Duration) -> Values {
        self.source
            .snapshot()
            .ok()
            .and_then(|state| cached_property(&state, FilterWheelProperty::Position, now).ok())
            .map(|sample| match sample.value {
                FilterWheelValue::Integer { value } => {
                    Values::from([("Position".into(), json!(value))])
                }
                _ => unreachable!(),
            })
            .unwrap_or_default()
    }
    pub fn connected(&self) -> bool {
        self.source.connected()
    }
    pub fn generation(&self) -> Uuid {
        self.source.generation()
    }
    pub async fn capabilities(&self) -> Result<FilterWheelCapabilities, SourceError> {
        let names = self.source.read("names").await?;
        let FilterWheelValue::Strings { value: names } =
            FilterWheelProperty::Names.decode(&names)?
        else {
            unreachable!()
        };
        let offsets = self.source.read("focusoffsets").await?;
        let FilterWheelValue::Integers {
            value: focus_offsets,
        } = FilterWheelProperty::FocusOffsets.decode(&offsets)?
        else {
            unreachable!()
        };
        if names.len() != focus_offsets.len() {
            return Err(bad_reading());
        }
        Ok(FilterWheelCapabilities {
            names,
            focus_offsets,
        })
    }
    async fn read_position(&self, slots: usize) -> Result<i32, SourceError> {
        let value = self.source.read("position").await?;
        let FilterWheelValue::Integer { value } = FilterWheelProperty::Position.decode(&value)?
        else {
            unreachable!()
        };
        if value >= slots as i32 {
            return Err(bad_reading());
        }
        Ok(value)
    }
    pub async fn position(&self) -> Result<i32, SourceError> {
        let capabilities = self.capabilities().await?;
        self.read_position(capabilities.names.len()).await
    }
    pub async fn property(&self, property: FilterWheelProperty) -> Result<Value, SourceError> {
        if property == FilterWheelProperty::Position {
            return Ok(json!(self.position().await?));
        }
        let capabilities = self.capabilities().await?;
        Ok(match property {
            FilterWheelProperty::Names => json!(capabilities.names),
            FilterWheelProperty::FocusOffsets => json!(capabilities.focus_offsets),
            FilterWheelProperty::Position => unreachable!(),
        })
    }
    /// Acknowledge the source's nonblocking Position write. Clients observe
    /// Position until the requested slot is actually reached. Moving or unknown
    /// state cannot authorize another write; there is no invented Halt command.
    pub async fn move_to(&self, position: i32) -> Result<(), SourceError> {
        if position < 0 || position as usize >= MAX_FILTER_SLOTS {
            return Err(invalid("Invalid filter wheel position"));
        }
        let operation = self.source.operation().await?;
        let capabilities = self.capabilities().await?;
        if position as usize >= capabilities.names.len() {
            return Err(invalid("Filter wheel position is outside the source slots"));
        }
        if self.read_position(capabilities.names.len()).await? == -1 {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Filter wheel is already moving",
            ));
        }
        self.source
            .write(
                &operation,
                "position",
                Values::from([("Position".into(), json!(position))]),
            )
            .await
    }
}
fn array(value: &Value) -> Result<&Vec<Value>, SourceError> {
    value
        .as_array()
        .filter(|values| !values.is_empty() && values.len() <= MAX_FILTER_SLOTS)
        .ok_or_else(bad_reading)
}
fn bad_reading() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Source returned an invalid filter wheel property",
    )
}
