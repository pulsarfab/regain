//! Native camera input for the common source actor. The actor owns client leases;
//! NativeCamera retains worker operations, settings and immutable image storage.
use super::{
    image::{CameraImage, ImageBudget},
    native_owner::{NativeCamera, NativeOperationKind},
    properties::{CameraProperty, CameraSetting},
};
use crate::{
    config::DeviceType,
    sampling::{PropertyPoll, SampleRequest},
    source::{
        Backend, BackendFuture, ConnectionInfo, ConnectionMethod, ErrorKind, SampleBatch,
        SampleBudget, SourceError, Values,
    },
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;

pub struct NativeCameraBackend {
    owner: Arc<NativeCamera>,
    samples: Vec<SampleRequest>,
    connecting: Option<oneshot::Receiver<Result<(), SourceError>>>,
    // Automatic actor reset must not clear an unknown setting/initialization
    // outcome and replay it through the next connection. Only full disconnect
    // (the last client lease) clears this fence.
    uncertain: Option<SourceError>,
}
fn invalid() -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, "Invalid native camera parameters")
}
fn unsupported() -> SourceError {
    SourceError::new(ErrorKind::Unsupported, "Unsupported native camera member")
}
fn no_parameters(parameters: &Values) -> Result<(), SourceError> {
    if parameters.is_empty() {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn property(member: &str) -> Result<CameraProperty, SourceError> {
    CameraProperty::ALL
        .iter()
        .copied()
        .find(|p| p.member() == member)
        .ok_or_else(unsupported)
}
impl NativeCameraBackend {
    /// No discovery or equipment I/O. The owner must use the host's shared image
    /// budget/activity counter; camera_image verifies budget identity at dispatch.
    pub fn new(owner: Arc<NativeCamera>, samples: Vec<SampleRequest>) -> Result<Self, SourceError> {
        let plan = PropertyPoll::new(DeviceType::Camera, samples, 1)?;
        for sample in plan.samples() {
            property(&sample.member)?;
            no_parameters(&sample.parameters)?;
            if sample.sensor_age.is_some() {
                return Err(invalid());
            }
        }
        Ok(Self {
            owner,
            samples: plan.samples().to_vec(),
            connecting: None,
            uncertain: None,
        })
    }
    fn available(&self) -> Result<(), SourceError> {
        if let Some(error) = &self.uncertain {
            return Err(error.clone());
        }
        let state = self.owner.snapshot();
        // Core owns worker retirement/replacement during recovery and explicit
        // abort. Its control-handle availability is not the logical source
        // connection lifetime. Cached reads must not cancel a retained capture
        // or change source generation merely because that handle is absent.
        if state.connected {
            return Ok(());
        }
        let mut error = state.error.unwrap_or_else(|| {
            SourceError::new(
                ErrorKind::Disconnected,
                "Native camera worker is unavailable",
            )
        });
        error.transport_lost = true;
        Err(error)
    }
    fn batch(&self) -> Result<SampleBatch, SourceError> {
        self.available()?;
        let mut batch = SampleBatch::default();
        let mut budget = SampleBudget::default();
        for sample in &self.samples {
            let result = self
                .owner
                .read_property_observation(property(&sample.member)?)
                .and_then(|reading| {
                    let value = reading.value.into_value();
                    if !sample.valid_value(&value) || !budget.admit(&value) {
                        return Err(SourceError::new(
                            ErrorKind::Unavailable,
                            "Invalid native camera sample",
                        ));
                    }
                    let age = reading
                        .observed_at
                        .map(|at| {
                            tokio::time::Instant::now()
                                .checked_duration_since(at)
                                .map(|age| age.as_secs_f64())
                                .ok_or_else(invalid)
                        })
                        .transpose()?;
                    Ok((value, age))
                });
            match result {
                Ok((value, age)) => {
                    batch.values.insert(sample.key.clone(), value);
                    // No hardware age for negotiated metadata or local state.
                    // SourceActor's zero age then describes that local reading.
                    if let Some(age) = age {
                        batch.ages_seconds.insert(sample.key.clone(), age);
                    }
                }
                Err(error) => {
                    batch.errors.insert(sample.key.clone(), error);
                }
            }
        }
        Ok(batch)
    }
}
impl Backend for NativeCameraBackend {
    fn simulated(&self) -> bool {
        self.owner.simulated()
    }
    fn connection_info(&self) -> Option<ConnectionInfo> {
        Some(ConnectionInfo {
            device_type: DeviceType::Camera,
            interface_version: None,
            method: ConnectionMethod::Async,
            owns_connection: true,
            uncertain: self.uncertain.is_some(),
        })
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            loop {
                if self.connect_step().await? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    }
    fn connect_step(&mut self) -> BackendFuture<'_, bool> {
        Box::pin(async {
            if let Some(error) = &self.uncertain {
                return Err(error.clone());
            }
            if let Some(response) = &mut self.connecting {
                match response.try_recv() {
                    Ok(result) => {
                        self.connecting = None;
                        if let Err(error) = &result
                            && error.kind == ErrorKind::Uncertain
                        {
                            self.uncertain = Some(error.clone());
                        }
                        return result.map(|()| true);
                    }
                    Err(oneshot::error::TryRecvError::Empty) => return Ok(false),
                    Err(oneshot::error::TryRecvError::Closed) => {
                        self.connecting = None;
                        let error = SourceError::uncertain();
                        self.uncertain = Some(error.clone());
                        return Err(error);
                    }
                }
            }
            let state = self.owner.snapshot();
            if state.connected {
                return self.available().map(|()| true);
            }
            if state
                .operation
                .is_some_and(|op| op.kind == NativeOperationKind::Closing)
            {
                return Ok(false);
            }
            let (send, receive) = oneshot::channel();
            self.connecting = Some(receive);
            let owner = self.owner.clone();
            tokio::spawn(async move {
                let _ = send.send(owner.connect().await);
            });
            Ok(false)
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.connecting = None;
            self.uncertain = None;
            self.owner.close().await
        })
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            no_parameters(&parameters)?;
            self.available()?;
            self.owner
                .read_property(property(&member)?)
                .map(|value| value.into_value())
        })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.owner.wait_environment_refresh().await?;
            self.available()?;
            match member.as_str() {
                "startexposure" => {
                    if parameters.len() != 2 {
                        return Err(invalid());
                    }
                    let seconds = parameters
                        .get("Duration")
                        .and_then(Value::as_f64)
                        .ok_or_else(invalid)?;
                    let light = parameters
                        .get("Light")
                        .and_then(Value::as_bool)
                        .ok_or_else(invalid)?;
                    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| invalid())?;
                    let microseconds =
                        u64::try_from(duration.as_micros()).map_err(|_| invalid())?;
                    if microseconds == 0 {
                        return Err(invalid());
                    }
                    self.owner.start_configured(microseconds, !light)?;
                }
                "abortexposure" => {
                    no_parameters(&parameters)?;
                    self.owner.abort().await?;
                }
                "stopexposure" => {
                    no_parameters(&parameters)?;
                    return Err(unsupported());
                }
                _ => match CameraSetting::from_parameters(&member, &parameters)? {
                    CameraSetting::Gain(value) => {
                        self.owner.prepare_control(0, value.into()).await?;
                        self.owner.set_imaging_control(0, value.into()).await?;
                    }
                    CameraSetting::Offset(value) => {
                        self.owner.prepare_control(5, value.into()).await?;
                        self.owner.set_imaging_control(5, value.into()).await?;
                    }
                    CameraSetting::CoolerOn(value) => {
                        self.owner.prepare_control(17, i64::from(value)).await?;
                        self.owner.set_cooling(17, i64::from(value)).await?;
                    }
                    CameraSetting::SetCcdTemperature(value) => {
                        // Native controls use integer degrees. Never silently
                        // truncate an ASCOM floating-point temperature target.
                        if value.fract() != 0.0
                            || value < i32::MIN as f64
                            || value > i32::MAX as f64
                        {
                            return Err(invalid());
                        }
                        self.owner.prepare_control(16, value as i64).await?;
                        self.owner.set_cooling(16, value as i64).await?;
                    }
                    setting => self.owner.configure_geometry(setting)?,
                },
            }
            Ok(Value::Null)
        })
    }
    fn camera_image(&mut self, budget: ImageBudget) -> BackendFuture<'_, CameraImage> {
        Box::pin(async move {
            self.available()?;
            if !self.owner.shares_budget(&budget) {
                return Err(SourceError::new(
                    ErrorKind::Permanent,
                    "Native camera and host must share one image budget",
                ));
            }
            self.owner.image()
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { self.sample().await.map(|batch| batch.values) })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(async {
            // Publish the actual cached state before reserving a refresh. A poll
            // must not manufacture a permanently busy camera in its own cache.
            let batch = self.batch()?;
            if !self.samples.is_empty() {
                self.owner.begin_environment_refresh()?;
            }
            Ok(batch)
        })
    }
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.available()?;
            self.owner.begin_environment_refresh().map(|_| ())
        })
    }
    fn reset(&mut self) {
        let state = self.owner.snapshot();
        if let Some(error) = state
            .error
            .filter(|error| error.kind == ErrorKind::Uncertain)
        {
            self.uncertain = Some(error);
        } else if self.connecting.is_some()
            || state.cooling.is_some()
            || state.operation.is_some_and(|op| {
                matches!(
                    op.kind,
                    NativeOperationKind::Connecting | NativeOperationKind::Configuring
                )
            })
        {
            // Initialization may have applied settings before its waiter timed
            // out. Retiring it is cleanup, not evidence that no write occurred.
            self.uncertain = Some(SourceError::uncertain());
        }
        self.connecting = None;
        self.owner.reset();
    }
}
