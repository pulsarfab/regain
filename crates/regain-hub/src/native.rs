//! Native accessory sources share the production worker transport. Hardware
//! protocols, discovery, and motion coordinators remain in the vendor crates.
use crate::{
    config::{DeviceType, NativeDevice, SourceBackend, SourceConfig},
    native_reference::{NativeReferenceStore, ReferenceKey, ReferenceRecord, ReferenceState},
    source::{Backend, BackendFuture, ErrorKind, SampleBatch, SourceError, Values},
};
use regain_core::accessory::{AccessoryError, AccessoryWorker};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::process::Command;

/// Host-selected runtime settings, never inferred from a hardware failure.
#[derive(Clone)]
pub struct NativeRuntime {
    pub directory: PathBuf,
    pub simulate: bool,
    pub references: Option<NativeReferenceStore>,
}
pub struct NativeAccessoryBackend {
    runtime: NativeRuntime,
    device: NativeDevice,
    identity: String,
    filter_wheel: Option<crate::filterwheel::NativeFilterWheelMetadata>,
    deadline: Duration,
    worker: Option<AccessoryWorker>,
    verified_identity: Option<Value>,
    reference_key: ReferenceKey,
    reference: Option<ReferenceRecord>,
    reference_uncertain: bool,
    observed_reverse: Option<bool>,
}
impl NativeAccessoryBackend {
    /// Validates configuration without discovery, process launch, or device I/O.
    /// Camera sources must use regain-core's camera Session instead.
    pub fn new(config: &SourceConfig, runtime: NativeRuntime) -> Result<Self, SourceError> {
        let SourceBackend::Native {
            device,
            identity,
            filter_wheel,
        } = &config.backend
        else {
            return Err(invalid("Expected a native source"));
        };
        worker_arguments(*device)?;
        if filter_wheel
            .as_ref()
            .is_some_and(|metadata| *device != NativeDevice::Efw || !metadata.validate().is_empty())
        {
            return Err(invalid("Invalid direct EFW filter metadata"));
        }
        if identity.trim().is_empty()
            || identity.chars().count() > 200
            || identity.chars().any(char::is_control)
            || !config.polling.validate_timing().is_empty()
        {
            return Err(invalid("Invalid native identity or polling settings"));
        }
        Ok(Self {
            runtime: runtime.clone(),
            device: *device,
            identity: identity.clone(),
            filter_wheel: filter_wheel.clone(),
            deadline: Duration::from_secs_f64(config.polling.request_timeout_seconds),
            worker: None,
            verified_identity: None,
            reference_key: ReferenceKey {
                source: config.id,
                device: *device,
                identity: identity.to_ascii_lowercase(),
                simulated: runtime.simulate,
            },
            reference: None,
            reference_uncertain: false,
            observed_reverse: None,
        })
    }
    pub fn device_type(&self) -> DeviceType {
        self.device.device_type()
    }
    pub fn simulated(&self) -> bool {
        self.runtime.simulate
    }
    async fn request(&mut self, request: Value, write: bool) -> Result<Value, SourceError> {
        let worker = self.worker.as_mut().ok_or_else(|| {
            SourceError::new(ErrorKind::Disconnected, "Native source is disconnected")
        })?;
        worker
            .request_with_timeout(request, self.deadline)
            .await
            .map_err(|error| worker_error(error, write))
    }
    async fn status(&mut self) -> Result<Value, SourceError> {
        let status = self.request(json!({"command":"status"}), false).await?;
        if !status.is_object() {
            return Err(bad_status());
        }
        if self.device != NativeDevice::Ofp2 && status["error"].as_i64() != Some(0)
            || !status["fault"].is_null()
            || !status["motion_error"].is_null()
        {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Native device reported a fault; inspect it before reconnecting",
            ));
        }
        Ok(status)
    }
    async fn samples(&mut self) -> Result<SampleBatch, SourceError> {
        let reverse = if self.device_type() == DeviceType::Rotator {
            let settings = self.request(json!({"command":"settings"}), false).await?;
            let reverse = settings["reverse"].as_bool().ok_or_else(bad_status)?;
            if self.observed_reverse.is_some_and(|saved| saved != reverse) {
                self.reference_uncertain = true;
            } else if self.observed_reverse.is_none() {
                self.observed_reverse = Some(reverse);
            }
            if let Some(ReferenceRecord {
                state: ReferenceState::Known { reverse: saved, .. },
                ..
            }) = &self.reference
                && *saved != reverse
            {
                self.reference_uncertain = true;
            }
            Some(reverse)
        } else {
            None
        };
        let status = self.status().await?;
        let mut batch = SampleBatch::default();
        if self.device == NativeDevice::Efw {
            let slots = status["slots"]
                .as_u64()
                .and_then(|slots| usize::try_from(slots).ok())
                .filter(|slots| (1..=crate::filterwheel::MAX_FILTER_SLOTS).contains(slots))
                .ok_or_else(bad_status)?;
            let (names, offsets) = if let Some(metadata) = &self.filter_wheel {
                metadata.arrays(slots)?
            } else {
                (
                    json!(
                        (1..=slots)
                            .map(|slot| format!("Filter {slot}"))
                            .collect::<Vec<_>>()
                    ),
                    json!(vec![0i32; slots]),
                )
            };
            batch.values.insert("names".into(), names);
            batch.values.insert("focusoffsets".into(), offsets);
        }
        for (member, field, boolean) in properties(self.device) {
            if self.reference_uncertain && matches!(*member, "position" | "targetposition") {
                batch.errors.insert((*member).into(), unknown_reference());
                continue;
            }
            let value = match (*member, self.device) {
                ("canreverse", NativeDevice::Caa | NativeDevice::Falcon) => json!(true),
                ("stepsize", NativeDevice::Caa) => json!(0.02),
                ("stepsize", NativeDevice::Falcon) => json!(0.01),
                ("reverse", NativeDevice::Caa | NativeDevice::Falcon) => json!(reverse.unwrap()),
                ("position", NativeDevice::Efw) if status["moving"] == true => json!(-1),
                ("coverstate", NativeDevice::Ofp2) => match status["cover"].as_str() {
                    Some("closed") => json!(1),
                    Some("moving") => json!(2),
                    Some("open") => json!(3),
                    Some("unknown") => json!(4),
                    _ => Value::Null,
                },
                ("calibratorstate", NativeDevice::Ofp2) => {
                    match status["calibrator_on"].as_bool() {
                        Some(true) => json!(3),
                        Some(false) => json!(1),
                        None => Value::Null,
                    }
                }
                ("brightness", NativeDevice::Ofp2) if status["calibrator_on"] == false => json!(0),
                _ => status[*field].clone(),
            };
            let value = if matches!(
                *member,
                "position" | "mechanicalposition" | "targetposition"
            ) && self.device_type() == DeviceType::Rotator
            {
                value
                    .as_f64()
                    .map(|angle| json!(angle.rem_euclid(360.0)))
                    .unwrap_or(Value::Null)
            } else {
                value
            };
            if if *boolean {
                value.is_boolean()
            } else {
                value.as_f64().is_some_and(f64::is_finite)
            } {
                batch.values.insert((*member).into(), value);
            } else {
                batch.errors.insert(
                    (*member).into(),
                    if *member == "temperature" && status[*field].is_null() {
                        SourceError::new(
                            ErrorKind::Unavailable,
                            "Native temperature sensor is not available",
                        )
                    } else {
                        bad_status()
                    },
                );
            }
        }
        if self.device_type() == DeviceType::Focuser {
            for (member, value) in [
                ("absolute", true),
                ("tempcompavailable", false),
                ("tempcomp", false),
            ] {
                batch.values.insert(member.into(), json!(value));
            }
            if self.device == NativeDevice::Eta {
                batch.values.insert("stepsize".into(), json!(1.0));
                batch.errors.insert("temperature".into(), unsupported());
            } else {
                batch.errors.insert("stepsize".into(), unsupported());
            }
        }
        Ok(batch)
    }
}
impl Backend for NativeAccessoryBackend {
    fn simulated(&self) -> bool {
        self.runtime.simulate
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.worker.is_some() && self.verified_identity.is_some() {
                return Ok(());
            }
            self.reset();
            if self.device_type() == DeviceType::Rotator
                && let Some(store) = &self.runtime.references
            {
                self.reference = store.load(self.reference_key.clone()).await?;
                self.reference_uncertain = self
                    .reference
                    .as_ref()
                    .is_some_and(|record| matches!(record.state, ReferenceState::Uncertain));
            }
            let (vendor, device) = worker_arguments(self.device)?;
            let path = self
                .runtime
                .directory
                .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX));
            let mut command = Command::new(path);
            command
                .args([vendor, device, "serve", "--serial", &self.identity])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if self.runtime.simulate {
                command.arg("--simulate");
            }
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            let child = command.spawn().map_err(|_| {
                SourceError::new(
                    ErrorKind::Permanent,
                    "Could not start the native device worker",
                )
            })?;
            self.worker = Some(AccessoryWorker::new(child).map_err(|_| {
                SourceError::new(
                    ErrorKind::Permanent,
                    "Could not establish native worker ownership",
                )
            })?);
            let mut identity = match self.request(json!({"command":"identity"}), false).await {
                Ok(identity) => identity,
                Err(error) => {
                    self.reset();
                    return Err(error);
                }
            };
            if !identity["serial"]
                .as_str()
                .is_some_and(|serial| serial.eq_ignore_ascii_case(&self.identity))
            {
                self.reset();
                return Err(SourceError::new(
                    ErrorKind::Permanent,
                    "Native hardware identity changed",
                ));
            }
            identity["simulation"] = json!(self.runtime.simulate);
            if let Some(ReferenceState::Known { offset, reverse }) =
                self.reference.as_ref().map(|record| record.state.clone())
            {
                let restore = async {
                    let settings = self.request(json!({"command":"settings"}), false).await?;
                    let actual = settings["reverse"].as_bool().ok_or_else(bad_status)?;
                    if actual != reverse {
                        // An external direction change invalidates the saved
                        // transform. Connect must never mutate hardware to fix it.
                        self.reference_uncertain = true;
                    } else {
                        let reply = self
                            .request(
                                json!({"command":"restore-reference", "offset":offset}),
                                false,
                            )
                            .await?;
                        if reply["accepted"] != true {
                            return Err(bad_status());
                        }
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = restore {
                    self.reset();
                    return Err(error);
                }
            }
            self.verified_identity = Some(identity);
            Ok(())
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.verified_identity = None;
            if let Some(worker) = self.worker.take() {
                worker.close().await;
            }
            Ok(())
        })
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            if !parameters.is_empty() {
                return Err(invalid("This native property takes no parameters"));
            }
            if member == "identity" {
                return self.verified_identity.clone().ok_or_else(|| {
                    SourceError::new(ErrorKind::Disconnected, "Native source is disconnected")
                });
            }
            if self.device_type() == DeviceType::Focuser {
                if self.verified_identity.is_none() {
                    return Err(SourceError::new(
                        ErrorKind::Disconnected,
                        "Native source is disconnected",
                    ));
                }
                // These native protocols provide absolute coordinates and no
                // automatic temperature compensation. ETA coordinates are µm;
                // EAF/FC3 motor steps have no known optical travel conversion.
                match member.as_str() {
                    "absolute" => return Ok(json!(true)),
                    "tempcompavailable" | "tempcomp" => return Ok(json!(false)),
                    "stepsize" if self.device == NativeDevice::Eta => return Ok(json!(1.0)),
                    "stepsize" => return Err(unsupported()),
                    _ => {}
                }
            }
            if !properties(self.device)
                .iter()
                .any(|(name, _, _)| *name == member)
                && !(self.device == NativeDevice::Efw
                    && matches!(member.as_str(), "names" | "focusoffsets"))
            {
                return Err(unsupported());
            }
            let mut batch = self.samples().await?;
            if let Some(error) = batch.errors.remove(&member) {
                return Err(error);
            }
            batch.values.remove(&member).ok_or_else(bad_status)
        })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            let request = command_request(self.device, &member, &parameters)?;
            if self.device_type() == DeviceType::Rotator {
                let reference_change = matches!(member.as_str(), "sync" | "reverse");
                let preflight = if member != "halt" {
                    Some(self.samples().await?)
                } else {
                    None
                };
                if self.reference_uncertain
                    && matches!(member.as_str(), "move" | "moveabsolute" | "reverse")
                {
                    return Err(unknown_reference());
                }
                if reference_change {
                    let store = self.runtime.references.clone().ok_or_else(|| {
                        SourceError::new(
                            ErrorKind::Unavailable,
                            "Durable native reference storage is required",
                        )
                    })?;
                    let preflight = preflight.as_ref().unwrap();
                    if preflight
                        .values
                        .get("ismoving")
                        .and_then(Value::as_bool)
                        .ok_or_else(bad_status)?
                    {
                        return Err(SourceError::new(
                            ErrorKind::Busy,
                            "Native rotator is moving",
                        ));
                    }
                    let expected_angle = if member == "sync" {
                        parameters["Position"].as_f64().unwrap()
                    } else {
                        preflight
                            .values
                            .get("position")
                            .and_then(Value::as_f64)
                            .filter(|value| value.is_finite())
                            .ok_or_else(bad_status)?
                    };
                    let marker = store
                        .replace(
                            self.reference_key.clone(),
                            self.reference.as_ref().map(|record| record.revision),
                            ReferenceState::Uncertain,
                        )
                        .await?;
                    self.reference = Some(marker);
                    self.reference_uncertain = true;
                    // Everything after this point may have changed the source.
                    // A failed confirmation/save is uncertain, never replayable.
                    let result = async {
                        let reply = self.request(request, true).await?;
                        if reply["accepted"] != true {
                            return Err(SourceError::uncertain());
                        }
                        // CAA settings refreshes the worker's direction mapping.
                        // Read it before the logical coordinate confirmation.
                        let settings = self.request(json!({"command":"settings"}), false).await?;
                        let reverse = settings["reverse"].as_bool().ok_or_else(bad_status)?;
                        let status = self.status().await?;
                        if status["moving"] != false {
                            return Err(SourceError::uncertain());
                        }
                        let logical = status["logical_degrees"]
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(bad_status)?;
                        if ((logical - expected_angle + 180.0).rem_euclid(360.0) - 180.0).abs()
                            > 1e-6
                            || member == "reverse"
                                && parameters["Reverse"].as_bool() != Some(reverse)
                        {
                            return Err(SourceError::uncertain());
                        }
                        let offset = status["logical_offset"]
                            .as_f64()
                            .filter(|offset| offset.is_finite())
                            .ok_or_else(bad_status)?
                            .rem_euclid(360.0);
                        let record = store
                            .replace(
                                self.reference_key.clone(),
                                self.reference.as_ref().map(|record| record.revision),
                                ReferenceState::Known { offset, reverse },
                            )
                            .await?;
                        self.reference = Some(record);
                        self.reference_uncertain = false;
                        self.observed_reverse = Some(reverse);
                        Ok(Value::Null)
                    }
                    .await;
                    return result.map_err(|error| SourceError {
                        transport_lost: error.transport_lost,
                        ..SourceError::uncertain()
                    });
                }
            }
            if request["command"] == "move" {
                let status = self.status().await?;
                if self.device == NativeDevice::Efw {
                    let slots = status["slots"]
                        .as_u64()
                        .and_then(|slots| usize::try_from(slots).ok())
                        .filter(|slots| (1..=crate::filterwheel::MAX_FILTER_SLOTS).contains(slots))
                        .ok_or_else(bad_status)?;
                    if let Some(metadata) = &self.filter_wheel {
                        metadata.arrays(slots)?;
                    }
                }
                let maximum = if self.device == NativeDevice::Efw {
                    status["slots"]
                        .as_i64()
                        .and_then(|slots| slots.checked_sub(1))
                } else {
                    status["max_step"].as_i64()
                }
                .ok_or_else(bad_status)?;
                if request["position"].as_i64().unwrap() > maximum {
                    return Err(invalid("Position is outside the native device range"));
                }
            }
            self.request(request, true).await?;
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(self.samples().await?.values) })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(self.samples())
    }
    fn reset(&mut self) {
        self.worker.take();
        self.verified_identity = None;
        self.reference = None;
        self.reference_uncertain = false;
        self.observed_reverse = None;
    }
}
fn worker_arguments(device: NativeDevice) -> Result<(&'static str, &'static str), SourceError> {
    use NativeDevice::*;
    Ok(match device {
        Caa => ("zwo", "caa"),
        Efw => ("zwo", "efw"),
        Eaf => ("zwo", "eaf"),
        Fc3 => ("pegasus", "fc3"),
        Falcon => ("pegasus", "falcon"),
        Ofp2 => ("deepskydad", "ofp2"),
        Eta => ("wanderer", "eta"),
        CameraDirect | CameraSdk => {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Camera sources require the camera session supervisor",
            ));
        }
    })
}
pub(crate) fn properties(device: NativeDevice) -> &'static [(&'static str, &'static str, bool)] {
    use NativeDevice::*;
    match device {
        Eaf | Fc3 => &[
            ("position", "position", false),
            ("ismoving", "moving", true),
            ("temperature", "temperature_c", false),
            ("maxstep", "max_step", false),
            ("maxincrement", "max_step", false),
        ],
        Eta => &[
            ("position", "position", false),
            ("ismoving", "moving", true),
            ("maxstep", "max_step", false),
            ("maxincrement", "max_step", false),
        ],
        Efw => &[("position", "position", false), ("slots", "slots", false)],
        Caa => &[
            ("canreverse", "canreverse", true),
            ("reverse", "reverse", true),
            ("stepsize", "stepsize", false),
            ("position", "logical_degrees", false),
            ("mechanicalposition", "mechanical_degrees", false),
            ("targetposition", "target_degrees", false),
            ("ismoving", "moving", true),
            ("temperature", "temperature_c", false),
        ],
        Falcon => &[
            ("canreverse", "canreverse", true),
            ("stepsize", "stepsize", false),
            ("position", "logical_degrees", false),
            ("mechanicalposition", "mechanical_degrees", false),
            ("targetposition", "target_degrees", false),
            ("ismoving", "moving", true),
            ("reverse", "reverse", true),
        ],
        Ofp2 => &[
            ("brightness", "brightness", false),
            ("maxbrightness", "max_brightness", false),
            ("coverstate", "cover", false),
            ("calibratorstate", "calibrator_on", false),
        ],
        CameraDirect | CameraSdk => &[],
    }
}
fn command_request(
    device: NativeDevice,
    member: &str,
    parameters: &Values,
) -> Result<Value, SourceError> {
    use NativeDevice::*;
    let single = |key: &str| -> Result<&Value, SourceError> {
        if parameters.len() != 1 {
            return Err(invalid("Expected one command parameter"));
        }
        parameters
            .get(key)
            .ok_or_else(|| invalid("Required command parameter is missing"))
    };
    if matches!(device, Eaf | Fc3 | Eta) && member == "move"
        || device == Efw && member == "position"
    {
        let position = single("Position")?
            .as_i64()
            .filter(|position| (0..=i32::MAX as i64).contains(position))
            .ok_or_else(|| invalid("Position must be a nonnegative integer"))?;
        return Ok(json!({"command":"move", "position":position}));
    }
    if matches!(device, Caa | Falcon)
        && matches!(member, "move" | "moveabsolute" | "movemechanical" | "sync")
    {
        let position = single("Position")?
            .as_f64()
            .filter(|position| {
                position.is_finite()
                    && if member == "move" {
                        position.abs() <= 360.0
                    } else {
                        (0.0..360.0).contains(position)
                    }
            })
            .ok_or_else(|| invalid("Position is outside the ASCOM angle range"))?;
        return Ok(
            json!({"command":match member {"move"=>"move-relative", "moveabsolute"=>"move-to", "sync"=>"sync", _=>"move-mechanical"}, "degrees":position}),
        );
    }
    if matches!(device, Caa | Falcon) && member == "reverse" {
        let enabled = single("Reverse")?
            .as_bool()
            .ok_or_else(|| invalid("Reverse must be boolean"))?;
        return Ok(json!({"command":"reverse", "enabled":enabled}));
    }
    if device == Ofp2 && member == "calibratoron" {
        let brightness = single("Brightness")?
            .as_u64()
            .filter(|value| *value <= 4096)
            .ok_or_else(|| invalid("Brightness must be an integer from 0 to 4096"))?;
        return Ok(json!({"command":"on", "brightness":brightness}));
    }
    let command = match (device, member) {
        (Eaf | Fc3, "halt") => "halt",
        (Caa | Falcon, "halt") => "stop",
        (Ofp2, "calibratoroff") => "off",
        (Ofp2, "opencover") => "open",
        (Ofp2, "closecover") => "close",
        (Ofp2, "haltcover") => "halt",
        (Efw, "calibrate") => "calibrate",
        // ETA cannot stop an active movement. Do not invent a halt capability.
        _ => return Err(unsupported()),
    };
    if !parameters.is_empty() {
        return Err(invalid("This native command takes no parameters"));
    }
    Ok(json!({"command":command}))
}
fn worker_error(error: anyhow::Error, write: bool) -> SourceError {
    let transport_lost = !matches!(
        error.downcast_ref::<AccessoryError>(),
        Some(AccessoryError::InvalidRequest | AccessoryError::CommandFailed(_))
    );
    if write {
        SourceError {
            transport_lost,
            ..SourceError::uncertain()
        }
    } else {
        SourceError {
            transport_lost,
            ..SourceError::new(
                if transport_lost {
                    ErrorKind::Transient
                } else {
                    ErrorKind::Unavailable
                },
                "Native worker request failed",
            )
        }
    }
}
fn unsupported() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "This native member is not supported",
    )
}
fn invalid(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, message)
}
fn bad_status() -> SourceError {
    SourceError::new(ErrorKind::Permanent, "Invalid native device status")
}
fn unknown_reference() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Native rotator reference is uncertain; explicitly Sync before logical movement",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_worker_failures_do_not_prove_a_write_was_rejected_before_dispatch() {
        let error = worker_error(
            AccessoryError::CommandFailed("private driver detail".into()).into(),
            true,
        );
        assert_eq!(error.kind, ErrorKind::Uncertain);
        assert!(!error.transport_lost);
        assert!(!error.to_string().contains("private driver detail"));
        let error = worker_error(AccessoryError::Timeout.into(), true);
        assert_eq!(error.kind, ErrorKind::Uncertain);
        assert!(error.transport_lost);
        let error = worker_error(
            AccessoryError::CommandFailed("private driver detail".into()).into(),
            false,
        );
        assert_eq!(error.kind, ErrorKind::Unavailable);
        assert!(!error.transport_lost);
    }
    #[test]
    fn unsupported_capabilities_and_invalid_parameters_never_become_worker_commands() {
        for (device, member) in [
            (NativeDevice::Eta, "halt"),
            (NativeDevice::Efw, "halt"),
            (NativeDevice::Caa, "resetorigin"),
            (NativeDevice::Fc3, "connected"),
        ] {
            assert_eq!(
                command_request(device, member, &Values::new())
                    .unwrap_err()
                    .kind,
                ErrorKind::Unsupported
            );
        }
        for parameters in [
            Values::new(),
            Values::from([("Position".into(), json!(1.5))]),
            Values::from([("Position".into(), json!(1)), ("extra".into(), json!(true))]),
        ] {
            assert_eq!(
                command_request(NativeDevice::Fc3, "move", &parameters)
                    .unwrap_err()
                    .kind,
                ErrorKind::InvalidValue
            );
        }
    }
}
