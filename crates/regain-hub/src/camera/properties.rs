//! Camera properties and settings shared by transports and output frontends.
//! Geometry is checked as a combination at StartExposure, not after each setter.
use crate::{
    source::{ErrorKind, SampleBudget, SourceError, Values},
    typed_source::TypedSourceSession,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
enum Kind {
    Boolean,
    Integer,
    Number,
    Text,
    Strings,
}
macro_rules! properties {
    ($($name:ident => ($member:literal, $kind:ident)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(rename_all = "camelCase")]
        pub enum CameraProperty { $($name),+ }
        impl CameraProperty {
            pub const ALL: &'static [Self] = &[$(Self::$name),+];
            pub fn member(self) -> &'static str {
                match self { $(Self::$name => $member),+ }
            }
            fn kind(self) -> Kind {
                match self { $(Self::$name => Kind::$kind),+ }
            }
        }
    };
}
properties! {
    BayerOffsetX => ("bayeroffsetx", Integer),
    BayerOffsetY => ("bayeroffsety", Integer),
    BinX => ("binx", Integer),
    BinY => ("biny", Integer),
    CameraState => ("camerastate", Integer),
    CameraXSize => ("cameraxsize", Integer),
    CameraYSize => ("cameraysize", Integer),
    CanAbortExposure => ("canabortexposure", Boolean),
    CanAsymmetricBin => ("canasymmetricbin", Boolean),
    CanFastReadout => ("canfastreadout", Boolean),
    CanGetCoolerPower => ("cangetcoolerpower", Boolean),
    CanPulseGuide => ("canpulseguide", Boolean),
    CanSetCcdTemperature => ("cansetccdtemperature", Boolean),
    CanStopExposure => ("canstopexposure", Boolean),
    CcdTemperature => ("ccdtemperature", Number),
    CoolerOn => ("cooleron", Boolean),
    CoolerPower => ("coolerpower", Number),
    ElectronsPerAdu => ("electronsperadu", Number),
    ExposureMin => ("exposuremin", Number),
    ExposureMax => ("exposuremax", Number),
    ExposureResolution => ("exposureresolution", Number),
    FastReadout => ("fastreadout", Boolean),
    FullWellCapacity => ("fullwellcapacity", Number),
    Gain => ("gain", Integer),
    GainMin => ("gainmin", Integer),
    GainMax => ("gainmax", Integer),
    Gains => ("gains", Strings),
    HasShutter => ("hasshutter", Boolean),
    HeatSinkTemperature => ("heatsinktemperature", Number),
    ImageReady => ("imageready", Boolean),
    IsPulseGuiding => ("ispulseguiding", Boolean),
    LastExposureDuration => ("lastexposureduration", Number),
    LastExposureStartTime => ("lastexposurestarttime", Text),
    MaxAdu => ("maxadu", Integer),
    MaxBinX => ("maxbinx", Integer),
    MaxBinY => ("maxbiny", Integer),
    NumX => ("numx", Integer),
    NumY => ("numy", Integer),
    Offset => ("offset", Integer),
    OffsetMin => ("offsetmin", Integer),
    OffsetMax => ("offsetmax", Integer),
    Offsets => ("offsets", Strings),
    PercentCompleted => ("percentcompleted", Integer),
    PixelSizeX => ("pixelsizex", Number),
    PixelSizeY => ("pixelsizey", Number),
    ReadoutMode => ("readoutmode", Integer),
    ReadoutModes => ("readoutmodes", Strings),
    SensorName => ("sensorname", Text),
    SensorType => ("sensortype", Integer),
    SetCcdTemperature => ("setccdtemperature", Number),
    StartX => ("startx", Integer),
    StartY => ("starty", Integer),
    SubExposureDuration => ("subexposureduration", Number),
}

#[derive(Debug, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum CameraValue {
    Boolean { value: bool },
    Integer { value: i32 },
    Number { value: f64 },
    Text { value: String },
    Strings { value: Vec<String> },
}
fn bad_reading() -> SourceError {
    SourceError::new(ErrorKind::Unavailable, "Invalid typed camera property")
}
fn invalid() -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, "Invalid camera setting")
}
impl CameraProperty {
    pub fn sample_request(self) -> crate::sampling::SampleRequest {
        use crate::sampling::SampleType;
        crate::sampling::SampleRequest {
            key: self.member().into(),
            member: self.member().into(),
            parameters: Values::new(),
            value_type: match self.kind() {
                Kind::Boolean => SampleType::Boolean,
                Kind::Integer | Kind::Number => SampleType::Number,
                Kind::Text => SampleType::Text,
                Kind::Strings => SampleType::Strings,
            },
            sensor_age: None,
        }
    }
    pub fn decode(self, value: &Value) -> Result<CameraValue, SourceError> {
        if !SampleBudget::default().admit(value) {
            return Err(bad_reading());
        }
        let result = match self.kind() {
            Kind::Boolean => value.as_bool().map(|value| CameraValue::Boolean { value }),
            Kind::Integer => value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|value| match self {
                    Self::BinX
                    | Self::BinY
                    | Self::MaxBinX
                    | Self::MaxBinY
                    | Self::CameraXSize
                    | Self::CameraYSize
                    | Self::NumX
                    | Self::NumY => *value > 0,
                    Self::CameraState | Self::SensorType => (0..=5).contains(value),
                    Self::PercentCompleted => (0..=100).contains(value),
                    Self::BayerOffsetX
                    | Self::BayerOffsetY
                    | Self::StartX
                    | Self::StartY
                    | Self::ReadoutMode
                    | Self::MaxAdu => *value >= 0,
                    _ => true, // Numeric gain/offset modes can have negative bounds.
                })
                .map(|value| CameraValue::Integer { value }),
            Kind::Number => value
                .as_f64()
                .filter(|value| {
                    value.is_finite()
                        && match self {
                            Self::CoolerPower => (0.0..=100.0).contains(value),
                            Self::ExposureResolution | Self::PixelSizeX | Self::PixelSizeY => {
                                *value > 0.0
                            }
                            Self::ElectronsPerAdu
                            | Self::FullWellCapacity
                            | Self::ExposureMin
                            | Self::ExposureMax
                            | Self::LastExposureDuration
                            | Self::SubExposureDuration => *value >= 0.0,
                            _ => true,
                        }
                })
                .map(|value| CameraValue::Number { value }),
            Kind::Text => value
                .as_str()
                .filter(|value| {
                    self != Self::LastExposureStartTime
                        || super::acquisition::valid_start_time(value)
                })
                .map(|value| CameraValue::Text {
                    value: value.into(),
                }),
            Kind::Strings => value
                .as_array()
                .filter(|values| !values.is_empty())
                .and_then(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .map(|value| CameraValue::Strings { value }),
        };
        result.ok_or_else(bad_reading)
    }
    pub(crate) async fn read(
        self,
        source: &TypedSourceSession,
    ) -> Result<CameraValue, SourceError> {
        self.decode(&source.read(self.member()).await?)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "property",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum CameraSetting {
    BinX(i32),
    BinY(i32),
    NumX(i32),
    NumY(i32),
    StartX(i32),
    StartY(i32),
    Gain(i32),
    Offset(i32),
    ReadoutMode(i32),
    FastReadout(bool),
    CoolerOn(bool),
    SetCcdTemperature(f64),
    SubExposureDuration(f64),
}
impl CameraSetting {
    pub(crate) fn changes_capture(self) -> bool {
        !matches!(self, Self::CoolerOn(_) | Self::SetCcdTemperature(_))
    }
    pub fn property(self) -> CameraProperty {
        use CameraProperty as P;
        match self {
            Self::BinX(_) => P::BinX,
            Self::BinY(_) => P::BinY,
            Self::NumX(_) => P::NumX,
            Self::NumY(_) => P::NumY,
            Self::StartX(_) => P::StartX,
            Self::StartY(_) => P::StartY,
            Self::Gain(_) => P::Gain,
            Self::Offset(_) => P::Offset,
            Self::ReadoutMode(_) => P::ReadoutMode,
            Self::FastReadout(_) => P::FastReadout,
            Self::CoolerOn(_) => P::CoolerOn,
            Self::SetCcdTemperature(_) => P::SetCcdTemperature,
            Self::SubExposureDuration(_) => P::SubExposureDuration,
        }
    }
    pub fn validate(self) -> Result<(), SourceError> {
        let valid = match self {
            Self::BinX(v) | Self::BinY(v) | Self::NumX(v) | Self::NumY(v) => v > 0,
            Self::StartX(v) | Self::StartY(v) | Self::ReadoutMode(v) => v >= 0,
            Self::SetCcdTemperature(v) => v.is_finite() && v >= -273.15,
            Self::SubExposureDuration(v) => v.is_finite() && v >= 0.0,
            _ => true,
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }
    pub(crate) fn parameters(self) -> Values {
        let (name, value) = match self {
            Self::BinX(v) => ("BinX", json!(v)),
            Self::BinY(v) => ("BinY", json!(v)),
            Self::NumX(v) => ("NumX", json!(v)),
            Self::NumY(v) => ("NumY", json!(v)),
            Self::StartX(v) => ("StartX", json!(v)),
            Self::StartY(v) => ("StartY", json!(v)),
            Self::Gain(v) => ("Gain", json!(v)),
            Self::Offset(v) => ("Offset", json!(v)),
            Self::ReadoutMode(v) => ("ReadoutMode", json!(v)),
            Self::FastReadout(v) => ("FastReadout", json!(v)),
            Self::CoolerOn(v) => ("CoolerOn", json!(v)),
            Self::SetCcdTemperature(v) => ("SetCCDTemperature", json!(v)),
            Self::SubExposureDuration(v) => ("SubExposureDuration", json!(v)),
        };
        Values::from([(name.into(), value)])
    }
    pub(crate) async fn preflight(self, source: &TypedSourceSession) -> Result<(), SourceError> {
        use CameraProperty as P;
        if self.changes_capture()
            && !matches!(
                P::CameraState.read(source).await?,
                CameraValue::Integer { value: 0 }
            )
        {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Camera is not idle for settings",
            ));
        }
        match self {
            Self::BinX(value) | Self::BinY(value) => {
                let limit = if matches!(self, Self::BinX(_)) {
                    P::MaxBinX
                } else {
                    P::MaxBinY
                };
                if value > integer(limit.read(source).await?)? {
                    return Err(invalid());
                }
                // Symmetric-bin propagation belongs to the source setter. Do not
                // invent a second write or reject an intermediate ROI here.
            }
            Self::Gain(value) | Self::Offset(value) => {
                let (names, minimum, maximum) = if matches!(self, Self::Gain(_)) {
                    (P::Gains, P::GainMin, P::GainMax)
                } else {
                    (P::Offsets, P::OffsetMin, P::OffsetMax)
                };
                match names.read(source).await {
                    Ok(CameraValue::Strings { value: names }) => {
                        if value < 0 || value as usize >= names.len() {
                            return Err(invalid());
                        }
                    }
                    Err(error) if error.kind == ErrorKind::Unsupported => {
                        let min = integer(minimum.read(source).await?)?;
                        let max = integer(maximum.read(source).await?)?;
                        if min > max {
                            return Err(bad_reading());
                        }
                        if value < min || value > max {
                            return Err(invalid());
                        }
                    }
                    Err(error) => return Err(error),
                    _ => return Err(bad_reading()),
                }
            }
            Self::ReadoutMode(value) => {
                let CameraValue::Strings { value: names } = P::ReadoutModes.read(source).await?
                else {
                    return Err(bad_reading());
                };
                if value as usize >= names.len() {
                    return Err(invalid());
                }
            }
            Self::FastReadout(_) => require(P::CanFastReadout, source).await?,
            Self::SetCcdTemperature(_) => require(P::CanSetCcdTemperature, source).await?,
            _ => {}
        }
        Ok(())
    }
}
fn integer(value: CameraValue) -> Result<i32, SourceError> {
    match value {
        CameraValue::Integer { value } => Ok(value),
        _ => Err(bad_reading()),
    }
}
async fn require(property: CameraProperty, source: &TypedSourceSession) -> Result<(), SourceError> {
    match property.read(source).await? {
        CameraValue::Boolean { value: true } => Ok(()),
        CameraValue::Boolean { value: false } => Err(SourceError::new(
            ErrorKind::Unsupported,
            "Camera capability is not supported",
        )),
        _ => Err(bad_reading()),
    }
}
