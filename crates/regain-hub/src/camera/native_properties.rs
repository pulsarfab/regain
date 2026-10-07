//! Native RAW16 camera properties use the shared frontend types and validation.
//! Geometry is desired local state; acknowledged hardware controls stay in core.
use super::{
    image::CameraImage,
    native_owner::NativeOperationKind,
    properties::{CameraProperty, CameraSetting, CameraValue},
};
use crate::source::{ErrorKind, SourceError};
use regain_core::{Control, Exposure, Status};
use serde::Serialize;
use serde_json::{Value, json};

fn unavailable() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Native camera property is unavailable or invalid",
    )
}

fn invalid() -> SourceError {
    SourceError::new(
        ErrorKind::InvalidValue,
        "Native camera geometry setting is invalid",
    )
}
fn unsupported() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "Native camera property or setting is not supported",
    )
}
fn dimension(info: &Value, key: &str) -> Result<u32, SourceError> {
    info[key]
        .as_u64()
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| *v > 0)
        .map(|v| v as u32)
        .ok_or_else(unavailable)
}
fn bins(info: &Value) -> Result<Vec<u32>, SourceError> {
    let values = info["bins"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 1024)
        .ok_or_else(unavailable)?;
    values
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v > 0)
                .map(|v| v as u32)
                .ok_or_else(unavailable)
        })
        .collect()
}
fn cap(core: &Status, kind: i32) -> Result<&Control, SourceError> {
    let control = core.controls.get(&kind).ok_or_else(unsupported)?;
    if control.kind != kind || control.min > control.max {
        return Err(unavailable());
    }
    Ok(control)
}
fn value(core: &Status, kind: i32) -> Result<i64, SourceError> {
    let cap = cap(core, kind)?;
    core.values
        .get(&kind)
        .copied()
        .filter(|v| (cap.min..=cap.max).contains(v))
        .ok_or_else(unavailable)
}

pub(super) fn validate_imaging_control(
    core: &Status,
    kind: i32,
    requested: i64,
) -> Result<(), SourceError> {
    if !matches!(kind, 0 | 5) {
        return Err(unsupported());
    }
    if !core.connected || !core.control_connection_available {
        return Err(unavailable());
    }
    let control = cap(core, kind)?;
    if !control.writable {
        return Err(unsupported());
    }
    if !(control.min..=control.max).contains(&requested) {
        return Err(SourceError::new(
            ErrorKind::InvalidValue,
            "Native imaging control value is outside its range",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeGeometry {
    bin: u32,
    start_x: u32,
    start_y: u32,
    width: u32,
    height: u32,
}
impl NativeGeometry {
    pub fn initial(info: &Value) -> Result<Self, SourceError> {
        let bin = bins(info)?.into_iter().min().ok_or_else(unavailable)?;
        let width = dimension(info, "width")? / bin / 8 * 8;
        let height = dimension(info, "height")? / bin / 2 * 2;
        if width == 0 || height == 0 {
            return Err(unavailable());
        }
        Ok(Self {
            bin,
            start_x: 0,
            start_y: 0,
            width,
            height,
        })
    }
    pub(super) fn from_exposure(exposure: &Exposure) -> Self {
        Self {
            bin: exposure.bin,
            start_x: exposure.x,
            start_y: exposure.y,
            width: exposure.width,
            height: exposure.height,
        }
    }
    pub fn exposure(self, microseconds: u64, dark: bool) -> Exposure {
        Exposure {
            width: self.width,
            height: self.height,
            bin: self.bin,
            x: self.start_x,
            y: self.start_y,
            microseconds,
            dark,
        }
    }
    /// Validate individual setters, preserving intermediate ROI combinations.
    /// StartExposure validates combined binning, bounds and sensor alignment.
    pub fn configured(mut self, setting: CameraSetting, info: &Value) -> Result<Self, SourceError> {
        setting.validate()?;
        match setting {
            CameraSetting::BinX(v) | CameraSetting::BinY(v) => {
                if !bins(info)?.contains(&(v as u32)) {
                    return Err(invalid());
                }
                self.bin = v as u32;
            }
            CameraSetting::NumX(v) | CameraSetting::NumY(v) => {
                let maximum = dimension(
                    info,
                    if matches!(setting, CameraSetting::NumX(_)) {
                        "width"
                    } else {
                        "height"
                    },
                )? / self.bin;
                if v as u32 > maximum {
                    return Err(invalid());
                }
                if matches!(setting, CameraSetting::NumX(_)) {
                    self.width = v as u32;
                } else {
                    self.height = v as u32;
                }
            }
            CameraSetting::StartX(v) | CameraSetting::StartY(v) => {
                let maximum = dimension(
                    info,
                    if matches!(setting, CameraSetting::StartX(_)) {
                        "width"
                    } else {
                        "height"
                    },
                )? / self.bin;
                if v as u32 >= maximum {
                    return Err(invalid());
                }
                if matches!(setting, CameraSetting::StartX(_)) {
                    self.start_x = v as u32;
                } else {
                    self.start_y = v as u32;
                }
            }
            CameraSetting::ReadoutMode(0) => {}
            CameraSetting::ReadoutMode(_) => return Err(invalid()),
            _ => return Err(unsupported()),
        }
        Ok(self)
    }
}

pub(super) struct NativeProperties<'a> {
    pub core: &'a Status,
    pub geometry: NativeGeometry,
    pub operation: Option<NativeOperationKind>,
    pub image: Option<&'a CameraImage>,
    pub image_ready: bool,
    pub error: Option<&'a SourceError>,
}
impl NativeProperties<'_> {
    pub fn read(&self, property: CameraProperty) -> Result<CameraValue, SourceError> {
        use CameraProperty as P;
        let core = self.core;
        let info = &core.info;
        let reading = match property {
            P::CameraXSize => json!(dimension(info, "width")?),
            P::CameraYSize => json!(dimension(info, "height")?),
            P::PixelSizeX | P::PixelSizeY => info["pixelSize"].clone(),
            P::SensorName => info["name"].clone(),
            P::HasShutter => info["shutter"].clone(),
            P::SensorType => json!(if info["color"].as_bool().ok_or_else(unavailable)? {
                2
            } else {
                0
            }),
            P::BayerOffsetX | P::BayerOffsetY => {
                if !info["color"].as_bool().ok_or_else(unavailable)? {
                    return Err(unsupported());
                }
                let bayer = info["bayer"]
                    .as_u64()
                    .filter(|b| *b <= 3)
                    .ok_or_else(unavailable)?;
                let offset = if property == P::BayerOffsetX {
                    matches!(bayer, 1 | 2)
                } else {
                    matches!(bayer, 1 | 3)
                };
                json!(i32::from(offset))
            }
            P::CanAbortExposure => json!(true),
            P::CanAsymmetricBin | P::CanFastReadout | P::CanPulseGuide | P::CanStopExposure => {
                json!(false)
            }
            P::CanSetCcdTemperature => {
                if core.controls.contains_key(&16) && core.controls.contains_key(&17) {
                    json!(cap(core, 16)?.writable && cap(core, 17)?.writable)
                } else {
                    json!(false)
                }
            }
            P::CanGetCoolerPower => {
                if core.controls.contains_key(&15) {
                    cap(core, 15)?;
                    json!(true)
                } else {
                    json!(false)
                }
            }
            P::CcdTemperature => json!(value(core, 8)? as f64 / 10.0),
            P::CoolerPower => json!(value(core, 15)?),
            P::CoolerOn => match value(core, 17)? {
                0 => json!(false),
                1 => json!(true),
                _ => return Err(unavailable()),
            },
            P::SetCcdTemperature => json!(value(core, 16)?),
            P::Gain => json!(value(core, 0)?),
            P::GainMin => json!(cap(core, 0)?.min),
            P::GainMax => json!(cap(core, 0)?.max),
            P::Offset => json!(value(core, 5)?),
            P::OffsetMin => json!(cap(core, 5)?.min),
            P::OffsetMax => json!(cap(core, 5)?.max),
            P::ExposureMin => json!(cap(core, 1)?.min as f64 / 1e6),
            P::ExposureMax => json!(cap(core, 1)?.max as f64 / 1e6),
            P::ExposureResolution => json!(0.000001),
            P::MaxAdu => json!(65535), // Core returns normalized RAW16 pixels.
            P::MaxBinX | P::MaxBinY => {
                json!(bins(info)?.into_iter().max().ok_or_else(unavailable)?)
            }
            P::BinX | P::BinY => json!(self.geometry.bin),
            P::StartX => json!(self.geometry.start_x),
            P::StartY => json!(self.geometry.start_y),
            P::NumX => json!(self.geometry.width),
            P::NumY => json!(self.geometry.height),
            P::ReadoutMode => json!(0),
            P::ReadoutModes => json!(["RAW16"]),
            P::CameraState => json!(if self.error.is_some() {
                5
            } else {
                match self.operation {
                    Some(NativeOperationKind::Capturing) => match core.phase.as_str() {
                        "Starting exposure" | "Exposing" => 2,
                        phase
                            if phase == "Downloading"
                                || phase.starts_with("Rereading ready frame") =>
                        {
                            4
                        }
                        _ => 1,
                    },
                    Some(_) => 1,
                    None => 0,
                }
            }),
            P::ImageReady => {
                if let Some(error) = self.error {
                    return Err(error.clone());
                }
                json!(self.image_ready)
            }
            P::LastExposureDuration | P::LastExposureStartTime => {
                if let Some(error) = self.error {
                    return Err(error.clone());
                }
                let image = self
                    .image
                    .filter(|_| self.image_ready)
                    .ok_or_else(unavailable)?;
                let native = image.native().ok_or_else(unavailable)?;
                if property == P::LastExposureDuration {
                    json!(native.exposure().microseconds as f64 / 1e6)
                } else {
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Timing {
                        started_utc: Option<String>,
                    }
                    let timing: Timing = serde_json::from_slice(native.metadata_json())
                        .map_err(|_| unavailable())?;
                    json!(timing.started_utc.ok_or_else(unavailable)?)
                }
            }
            P::ElectronsPerAdu
            | P::FastReadout
            | P::FullWellCapacity
            | P::Gains
            | P::HeatSinkTemperature
            | P::IsPulseGuiding
            | P::Offsets
            | P::PercentCompleted
            | P::SubExposureDuration => return Err(unsupported()),
        };
        property.decode(&reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};
    fn core() -> Status {
        Status {
            info: json!({"width":960,"height":640,"bins":[1,2,4],"color":true,"bayer":0,"name":"private camera","pixelSize":3.76,"shutter":false}),
            controls: [
                (0, 0, 600, 100),
                (1, 32, 2_000_000_000, 10000),
                (8, -500, 1000, -100),
                (15, 0, 100, 30),
                (16, -40, 30, -10),
                (17, 0, 1, 1),
            ]
            .into_iter()
            .map(|(kind, min, max, value)| {
                (
                    kind,
                    Control {
                        kind,
                        min,
                        max,
                        value,
                        writable: !matches!(kind, 8 | 15),
                    },
                )
            })
            .collect(),
            values: BTreeMap::from([(0, 100), (8, -100), (15, 30), (16, -10), (17, 1)]),
            ..Status::default()
        }
    }
    fn read(core: &Status, property: CameraProperty) -> Result<CameraValue, SourceError> {
        NativeProperties {
            core,
            geometry: NativeGeometry::initial(&core.info).unwrap(),
            operation: None,
            image: None,
            image_ready: false,
            error: None,
        }
        .read(property)
    }
    #[test]
    fn missing_malformed_and_out_of_range_controls_never_become_default_values() {
        use CameraProperty as P;
        let mut core = core();
        core.values.remove(&0);
        assert_eq!(
            read(&core, P::Gain).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            read(&core, P::Offset).unwrap_err().kind,
            ErrorKind::Unsupported
        );
        core.values.insert(0, 601);
        assert_eq!(
            read(&core, P::Gain).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.controls.get_mut(&0).unwrap().max = i64::from(i32::MAX) + 1;
        assert_eq!(
            read(&core, P::GainMax).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.controls.get_mut(&0).unwrap().min = i64::from(i32::MAX) + 2;
        assert_eq!(
            read(&core, P::GainMin).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.controls.get_mut(&0).unwrap().kind = 5;
        assert_eq!(
            read(&core, P::GainMax).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.controls.get_mut(&17).unwrap().max = 2;
        core.values.insert(17, 2);
        assert_eq!(
            read(&core, P::CoolerOn).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.values.insert(15, 101);
        assert_eq!(
            read(&core, P::CoolerPower).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        core.values.insert(8, 1001);
        assert_eq!(
            read(&core, P::CcdTemperature).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        // Failure in one control does not fabricate or discard an unrelated fact.
        assert_eq!(
            read(&core, P::CameraXSize).unwrap(),
            CameraValue::Integer { value: 960 }
        );
        core.controls.remove(&8);
        assert_eq!(
            read(&core, P::CcdTemperature).unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
    #[test]
    fn native_info_uses_strict_types_and_validated_bayer_patterns() {
        use CameraProperty as P;
        let mut core = core();
        for (bayer, x, y) in [(0, 0, 0), (1, 1, 1), (2, 1, 0), (3, 0, 1)] {
            core.info["bayer"] = json!(bayer);
            assert_eq!(
                read(&core, P::BayerOffsetX).unwrap(),
                CameraValue::Integer { value: x }
            );
            assert_eq!(
                read(&core, P::BayerOffsetY).unwrap(),
                CameraValue::Integer { value: y }
            );
        }
        for (key, bad, property) in [
            ("bayer", json!(4), P::BayerOffsetX),
            ("color", json!("true"), P::SensorType),
            ("shutter", Value::Null, P::HasShutter),
            ("pixelSize", json!(0), P::PixelSizeX),
        ] {
            let prior = core.info[key].clone();
            core.info[key] = bad;
            assert_eq!(
                read(&core, property).unwrap_err().kind,
                ErrorKind::Unavailable
            );
            core.info[key] = prior;
        }
        core.info["color"] = json!(false);
        assert_eq!(
            read(&core, P::BayerOffsetX).unwrap_err().kind,
            ErrorKind::Unsupported
        );
        assert_eq!(
            read(&core, P::SensorType).unwrap(),
            CameraValue::Integer { value: 0 }
        );
        core.info["bins"] = json!([1, 0]);
        assert_eq!(
            NativeGeometry::initial(&core.info).unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    #[test]
    fn initial_geometry_uses_an_advertised_bin_and_symmetric_setters_preserve_roi() {
        let mut core = core();
        core.info["bins"] = json!([4, 2]);
        let geometry = NativeGeometry::initial(&core.info).unwrap();
        let exposure = geometry.exposure(10_000, true);
        assert_eq!(
            (exposure.bin, exposure.width, exposure.height),
            (2, 480, 320)
        );
        let changed = geometry
            .configured(CameraSetting::BinY(4), &core.info)
            .unwrap()
            .exposure(10_000, true);
        assert_eq!((changed.bin, changed.width, changed.height), (4, 480, 320));
        assert!(
            regain_core::validate_capture(&core.info, &core.controls, &changed, false).is_err()
        );
        assert_eq!(
            geometry
                .configured(CameraSetting::BinX(1), &core.info)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    #[test]
    fn invalid_or_missing_native_timestamp_is_independent_of_known_completed_duration() {
        let core = core();
        let exposure = NativeGeometry::initial(&core.info)
            .unwrap()
            .configured(CameraSetting::NumX(64), &core.info)
            .unwrap()
            .configured(CameraSetting::NumY(64), &core.info)
            .unwrap()
            .exposure(10_000, true);
        for started in [
            Value::Null,
            json!("2026-02-30T12:00:00Z"),
            json!("2026-01-01T12:00:00+01:00"),
            json!(1),
            json!("x".repeat(129)),
        ] {
            let budget = super::super::image::ImageBudget::new(1024 * 1024).unwrap();
            let image = budget
                .reserve_native(&exposure)
                .unwrap()
                .adopt(regain_core::Frame {
                    exposure: exposure.clone(),
                    pixels: Arc::from(vec![0; 8192]),
                    metadata: json!({"startedUtc":started}),
                })
                .unwrap();
            let view = NativeProperties {
                core: &core,
                geometry: NativeGeometry::from_exposure(&exposure),
                operation: None,
                image: Some(&image),
                image_ready: true,
                error: None,
            };
            assert_eq!(
                view.read(CameraProperty::LastExposureStartTime)
                    .unwrap_err()
                    .kind,
                ErrorKind::Unavailable
            );
            assert_eq!(
                view.read(CameraProperty::LastExposureDuration).unwrap(),
                CameraValue::Number { value: 0.01 }
            );
        }
    }
}
