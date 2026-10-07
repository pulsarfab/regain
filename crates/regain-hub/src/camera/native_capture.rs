//! Native capture admission around the existing core recovery implementation.
//! The caller owns capture lifecycle; this boundary adds no retries or deadlines.
use super::image::{CameraImage, ImageBudget, NativeFramePermit};
use crate::source::SourceError;
use regain_core::{CancellationToken, Exposure, Session};

#[derive(Debug)]
pub enum NativeCaptureError {
    Contract(SourceError),
    Capture(anyhow::Error),
}
impl std::fmt::Display for NativeCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(f),
            Self::Capture(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for NativeCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Contract(error) => error,
            Self::Capture(error) => error.as_ref(),
        })
    }
}

/// Reserve host payload memory before sending any exposure command. Core Session
/// keeps its configured retained-frame retries, cooling and recovery metadata.
pub async fn capture(
    session: &mut Session,
    exposure: Exposure,
    budget: &ImageBudget,
    token: &CancellationToken,
) -> Result<CameraImage, NativeCaptureError> {
    let permit = budget
        .reserve_native(&exposure)
        .map_err(NativeCaptureError::Contract)?;
    capture_admitted(session, exposure, permit, token).await
}

pub(super) async fn capture_admitted(
    session: &mut Session,
    exposure: Exposure,
    permit: NativeFramePermit,
    token: &CancellationToken,
) -> Result<CameraImage, NativeCaptureError> {
    let frame = session
        .capture(exposure, token)
        .await
        .map_err(NativeCaptureError::Capture)?;
    permit.adopt(frame).map_err(NativeCaptureError::Contract)
}
