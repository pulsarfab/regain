//! Frozen source metadata for scalar image consumers, captured under admission.
use super::{integer, unavailable};
use crate::{
    camera::properties::{CameraProperty, CameraValue},
    source::{ErrorKind, SourceError},
    typed_source::TypedSourceSession,
};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraCaptureProfile {
    pub max_adu: i32,
    pub sensor_type: i32,
    pub bayer_offset_x: i32,
    pub bayer_offset_y: i32,
    pub sensor_name: Option<String>,
}
impl CameraCaptureProfile {
    pub(crate) async fn scalar(source: &TypedSourceSession) -> Result<Self, SourceError> {
        let max_adu = integer(source, "maxadu").await?;
        let sensor_type = integer(source, "sensortype").await?;
        if max_adu <= 0 || !matches!(sensor_type, 0 | 2) {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Camera metadata does not describe a supported scalar image",
            ));
        }
        let (bayer_offset_x, bayer_offset_y) = if sensor_type == 2 {
            let x = integer(source, "bayeroffsetx").await?;
            let y = integer(source, "bayeroffsety").await?;
            if x < 0 || y < 0 {
                return Err(unavailable("Invalid camera Bayer offsets"));
            }
            (x, y)
        } else {
            // ASCOM monochrome offsets are unsupported; there is no Bayer grid.
            (0, 0)
        };
        let sensor_name = match CameraProperty::SensorName.read(source).await {
            Ok(CameraValue::Text { value }) => Some(value),
            Err(error) if error.kind == ErrorKind::Unsupported => None,
            Err(error) => return Err(error),
            _ => return Err(unavailable("Invalid camera sensor name")),
        };
        Ok(Self {
            max_adu,
            sensor_type,
            bayer_offset_x,
            bayer_offset_y,
            sensor_name,
        })
    }
}
