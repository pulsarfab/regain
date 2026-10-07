//! One set of output controllers per applied configuration. Client identities
//! and connection leases belong to the host, never to frontend-supplied IDs.
#[cfg(test)]
#[path = "camera/runtime_tests.rs"]
mod camera_tests;

use crate::{
    activity::{Activity, ActivityCounter},
    camera::{
        acquisition::{AcquisitionStatus, AcquisitionTiming, CameraSession, CameraSupervisor},
        runtime::CameraResources,
    },
    config::{Bitness, DeviceType, HubConfig, SafetyMember, VirtualDevice},
    coordination::{HostedFocuserStatus, host::GroupCoordinator},
    covercalibrator::{CoverCalibratorController, CoverCalibratorSession},
    factory::{CredentialProvider, build_sources_bound},
    filterwheel::{FilterWheelController, FilterWheelSession},
    focuser::{FocuserController, FocuserSession},
    native::NativeRuntime,
    parameters::FieldError,
    rotator::{RotatorController, RotatorSession},
    safety::Clock,
    safety_output::SafetyOutput,
    source::{ErrorKind, SourceError, SourceRegistry, SourceSnapshot},
    switch::{SwitchOutput, SwitchSession},
    weather::{WeatherOutput, WeatherSession},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDescriptor {
    pub id: Uuid,
    pub number: u32,
    pub label: String,
    pub device_type: DeviceType,
    pub simulated: bool,
}

enum Output {
    Camera(Arc<CameraSupervisor>),
    Safety {
        members: Vec<SafetyMember>,
        active: Mutex<Weak<SafetyOutput>>,
    },
    Switch(Arc<SwitchOutput>),
    Weather(Arc<WeatherOutput>),
    Focuser(FocuserController),
    Rotator(RotatorController),
    FilterWheel(FilterWheelController),
    CoverCalibrator(CoverCalibratorController),
}

pub struct HubRuntime {
    com_architectures: Vec<Bitness>,
    native_camera_sources: bool,
    runtime_id: Uuid,
    config: HubConfig,
    registry: Arc<SourceRegistry>,
    clock: Arc<dyn Clock>,
    outputs: BTreeMap<Uuid, Output>,
    cameras: BTreeMap<Uuid, Arc<CameraSupervisor>>,
    activity: ActivityCounter,
    groups: GroupCoordinator,
    lifecycle: Mutex<Lifecycle>,
    shutdown: tokio::sync::OnceCell<Result<(), Vec<(Uuid, SourceError)>>>,
}
struct Lifecycle {
    closed: bool,
    frozen: bool,
    clients: Vec<Weak<ClientSession>>,
}
impl HubRuntime {
    /// A dedicated image stream borrows an already connected control client's
    /// lease. It never connects equipment, starts a capture or downloads again.
    pub(crate) fn image_connection(
        &self,
        client: Uuid,
        output: Uuid,
    ) -> Result<Arc<OutputConnection>, SourceError> {
        let lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.closed {
            return Err(disconnected());
        }
        lifecycle
            .clients
            .iter()
            .filter_map(Weak::upgrade)
            .find(|entry| entry.id() == client)
            .ok_or_else(disconnected)?
            .connection(output)
    }
    /// Build controllers without connecting sources. No HTTP listener or ASCOM
    /// Platform is needed for native/network sources.
    pub fn build(
        config: HubConfig,
        native: &NativeRuntime,
        credentials: &dyn CredentialProvider,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        let resources = native
            .cameras
            .as_ref()
            .map(|cameras| cameras.resources.clone())
            .unwrap_or_default();
        Self::build_with_camera_resources(config, native, credentials, clock, resources)
    }

    /// An embedding host can share resources across proxy-only revisions without
    /// providing SDK configuration. Native owners must use these same resources.
    pub fn build_with_camera_resources(
        config: HubConfig,
        native: &NativeRuntime,
        credentials: &dyn CredentialProvider,
        clock: Arc<dyn Clock>,
        resources: CameraResources,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        validate_outputs(&config)?;
        let binding = Arc::new(std::sync::OnceLock::new());
        let registry = build_sources_bound(
            &config,
            native,
            credentials,
            clock.clone(),
            Some(binding.clone()),
        )?;
        let mut runtime =
            Self::from_registry_with_camera_resources(config, registry, clock, resources)?;
        let unpublished = Arc::get_mut(&mut runtime).expect("Unpublished runtime");
        unpublished.com_architectures = crate::com::available_architectures(native);
        unpublished.native_camera_sources = native.cameras.is_some();
        binding
            .set(Arc::downgrade(&runtime))
            .expect("New runtime binding");
        Ok(runtime)
    }

    /// Inject a registry for another host adapter or fault tests. The registry
    /// must describe exactly this configuration revision's sources.
    pub fn from_registry(
        config: HubConfig,
        registry: Arc<SourceRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        // Injected native actors already own accounting. Adopt the first one,
        // then require every other native camera to belong to that same host.
        let resources = config
            .sources
            .iter()
            .find_map(|source| {
                registry
                    .get(source.id)
                    .ok()
                    .and_then(|source| source.native_camera_resources().cloned())
            })
            .unwrap_or_default();
        Self::from_registry_with_camera_resources(config, registry, clock, resources)
    }

    /// Hosts retain these resources across revisions, including proxy-only
    /// cameras and readers that still pin images from a retired runtime.
    pub fn from_registry_with_camera_resources(
        config: HubConfig,
        registry: Arc<SourceRegistry>,
        clock: Arc<dyn Clock>,
        resources: CameraResources,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        validate_outputs(&config)?;
        let snapshots = registry.snapshots();
        if snapshots.len() != config.sources.len()
            || snapshots.iter().any(|state| {
                state.revision != config.revision
                    || !config.sources.iter().any(|s| s.id == state.source)
            })
        {
            return Err(vec![FieldError::new(
                "sources",
                "revision",
                "Source registry does not match this configuration revision",
            )]);
        }
        let mut cameras = BTreeMap::new();
        for (index, source_config) in config.sources.iter().enumerate() {
            let source = registry
                .get(source_config.id)
                .expect("Validated source registry");
            if source
                .native_camera_resources()
                .is_some_and(|native| !native.shares(&resources))
            {
                return Err(vec![FieldError::new(
                    format!("sources[{index}].backend"),
                    "cameraResources",
                    "Native cameras must share the host image budget and activity counter",
                )]);
            }
            if config.source_type(source_config.id) == Some(DeviceType::Camera) {
                let supervisor = CameraSupervisor::new(
                    source,
                    resources.image_budget(),
                    AcquisitionTiming {
                        connection_timeout: std::time::Duration::from_secs_f64(
                            source_config.polling.connection_timeout_seconds,
                        ),
                        ..Default::default()
                    },
                    resources.activity(),
                )
                .map_err(|error| {
                    vec![FieldError::new(
                        format!("sources[{index}].polling"),
                        "cameraTiming",
                        error.message,
                    )]
                })?;
                cameras.insert(source_config.id, supervisor);
            }
        }
        let mut outputs = BTreeMap::new();
        for (index, output) in config.outputs.iter().enumerate() {
            let mapped = match &output.device {
                VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Camera,
                } => Output::Camera(cameras[source].clone()),
                VirtualDevice::Safety { members } => Output::Safety {
                    members: members.clone(),
                    active: Mutex::new(Weak::new()),
                },
                VirtualDevice::Switch { .. } => Output::Switch(
                    SwitchOutput::new(&config, output.id, registry.clone(), clock.clone())
                        .map_err(|e| {
                            vec![FieldError::new(
                                format!("outputs[{index}]"),
                                "output",
                                e.message,
                            )]
                        })?,
                ),
                VirtualDevice::Weather { .. } => Output::Weather(
                    WeatherOutput::new(&config, output.id, registry.clone(), clock.clone())
                        .map_err(|e| {
                            vec![FieldError::new(
                                format!("outputs[{index}]"),
                                "output",
                                e.message,
                            )]
                        })?,
                ),
                VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Focuser,
                } => {
                    let source_config = config
                        .sources
                        .iter()
                        .find(|entry| entry.id == *source)
                        .unwrap();
                    Output::Focuser(
                        FocuserController::new(
                            registry.get(*source).unwrap(),
                            std::time::Duration::from_secs_f64(
                                source_config.polling.connection_timeout_seconds,
                            ),
                        )
                        .expect("Validated connection deadline"),
                    )
                }
                VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Rotator,
                } => Output::Rotator(
                    RotatorController::new(
                        registry.get(*source).unwrap(),
                        std::time::Duration::from_secs_f64(
                            config
                                .sources
                                .iter()
                                .find(|entry| entry.id == *source)
                                .unwrap()
                                .polling
                                .connection_timeout_seconds,
                        ),
                    )
                    .expect("Validated connection deadline"),
                ),
                VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::FilterWheel,
                } => Output::FilterWheel(
                    FilterWheelController::new(
                        registry.get(*source).unwrap(),
                        std::time::Duration::from_secs_f64(
                            config
                                .sources
                                .iter()
                                .find(|entry| entry.id == *source)
                                .unwrap()
                                .polling
                                .connection_timeout_seconds,
                        ),
                    )
                    .expect("Validated connection deadline"),
                ),
                VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::CoverCalibrator,
                } => Output::CoverCalibrator(
                    CoverCalibratorController::new(
                        registry.get(*source).unwrap(),
                        std::time::Duration::from_secs_f64(
                            config
                                .sources
                                .iter()
                                .find(|entry| entry.id == *source)
                                .unwrap()
                                .polling
                                .connection_timeout_seconds,
                        ),
                    )
                    .expect("Validated connection deadline"),
                ),
                VirtualDevice::Proxy { .. } => unreachable!("Validated output implementation"),
            };
            outputs.insert(output.id, mapped);
        }
        Ok(Arc::new(Self {
            groups: GroupCoordinator::new(&config, &registry, resources.activity()),
            com_architectures: Vec::new(),
            native_camera_sources: false,
            runtime_id: Uuid::new_v4(),
            config,
            registry,
            clock,
            outputs,
            cameras,
            activity: resources.activity(),
            lifecycle: Mutex::new(Lifecycle {
                closed: false,
                frozen: false,
                clients: Vec::new(),
            }),
            shutdown: tokio::sync::OnceCell::new(),
        }))
    }

    pub fn instance_id(&self) -> Uuid {
        self.config.instance_id
    }
    pub fn runtime_id(&self) -> Uuid {
        self.runtime_id
    }
    pub(crate) fn configuration(&self) -> &HubConfig {
        &self.config
    }
    pub(crate) fn configuration_capabilities(&self) -> Vec<&'static str> {
        let mut capabilities = vec![
            "nativeSources",
            "alpacaSources",
            "virtualSources",
            "writeReadout",
            "simulation",
            "cameraSimulation",
            "proxyOutputs",
            "cameraOutputs",
            "focuserOutputs",
            "focuserGroups",
            "rotatorOutputs",
            "filterWheelOutputs",
            "coverCalibratorOutputs",
        ];
        if self.native_camera_sources {
            capabilities.push("nativeCameraSources");
        }
        if !self.com_architectures.is_empty() {
            capabilities.push("comSources");
            capabilities.push("cameraComSources");
        }
        if self.com_architectures.contains(&Bitness::X86) {
            capabilities.push("comX86Sources");
        }
        if self.com_architectures.contains(&Bitness::X64) {
            capabilities.push("comX64Sources");
        }
        capabilities
    }
    pub(crate) fn contains_output(&self, id: Uuid) -> bool {
        self.outputs.contains_key(&id)
    }
    pub fn source_snapshot(&self, source: Uuid) -> Result<SourceSnapshot, SourceError> {
        self.registry.get(source).map(|source| source.snapshot())
    }
    pub(crate) fn camera_operation_timing(
        &self,
        host: Uuid,
        client: Uuid,
        output: Uuid,
        base: std::time::Duration,
    ) -> Result<Option<crate::camera::ipc_timing::CameraOperationTiming>, SourceError> {
        match self.outputs.get(&output) {
            Some(Output::Camera(camera)) => camera
                .operation_timing(host, self.revision(), client, output, base)
                .map(Some),
            Some(_) => Ok(None),
            None => Err(SourceError::new(
                ErrorKind::InvalidValue,
                "Unknown output ID",
            )),
        }
    }
    pub(crate) fn camera_capture_timing(
        &self,
        host: Uuid,
        client: Uuid,
        output: Uuid,
        duration_seconds: f64,
    ) -> Result<crate::camera::ipc_timing::CameraCaptureTiming, SourceError> {
        match self.outputs.get(&output) {
            Some(Output::Camera(camera)) => {
                camera.capture_timing(host, self.revision(), client, output, duration_seconds)
            }
            Some(_) => Err(SourceError::new(
                ErrorKind::Unsupported,
                "Output is not a camera",
            )),
            None => Err(SourceError::new(
                ErrorKind::InvalidValue,
                "Unknown output ID",
            )),
        }
    }
    /// Cached acquisition diagnostics only. This does not connect, admit an
    /// exposure, expose pixels, or authorize a camera output.
    pub fn camera_acquisition_status(
        &self,
        source: Uuid,
    ) -> Result<AcquisitionStatus, SourceError> {
        self.cameras
            .get(&source)
            .map(|camera| camera.status())
            .ok_or_else(|| SourceError::new(ErrorKind::InvalidValue, "Unknown camera source ID"))
    }
    /// Cached setup observation only: no connection/control leases or source I/O.
    /// The IPC dispatcher fences this read against the editor's saved revision.
    pub fn output_status(
        &self,
        output: Uuid,
        start: u32,
        limit: u32,
    ) -> Result<crate::diagnostics::OutputStatus, SourceError> {
        use crate::diagnostics::{Diagnostics, OutputStatus, SafetyMember, SourceHealth, page};
        let controller = self
            .outputs
            .get(&output)
            .ok_or_else(|| SourceError::new(ErrorKind::InvalidValue, "Unknown output ID"))?;
        let config = self
            .config
            .outputs
            .iter()
            .find(|entry| entry.id == output)
            .unwrap();
        let total = match controller {
            Output::Camera(_) => 1,
            Output::Safety { members, .. } => members.len() as u32,
            Output::Switch(switch) => switch.max_switch(),
            Output::Weather(_) => match &config.device {
                VirtualDevice::Weather { measurements } => measurements.len() as u32,
                _ => unreachable!("Validated weather controller"),
            },
            Output::Focuser(_) => crate::focuser::FocuserProperty::ALL.len() as u32,
            Output::Rotator(_) => crate::rotator::RotatorProperty::ALL.len() as u32,
            Output::FilterWheel(_) => crate::filterwheel::FilterWheelProperty::ALL.len() as u32,
            Output::CoverCalibrator(_) => {
                crate::covercalibrator::CoverCalibratorProperty::ALL.len() as u32
            }
        };
        let end = page(start, limit, total)?;
        let now = self.clock.now();
        let diagnostics = match controller {
            Output::Camera(camera) => {
                let (source, status) = camera.status_with_source();
                Diagnostics::Camera {
                    health: SourceHealth::from(&source),
                    acquisition: (start < end).then(|| Box::new(status)),
                }
            }
            Output::Safety { members, active } => {
                // Upgrade only an already running controller. Constructing a
                // SafetyOutput here would acquire leases and seed live policy.
                let running = active.lock().unwrap().upgrade();
                let snapshot = running.as_ref().map(|controller| controller.snapshot());
                let mut observations = Vec::new();
                for member in members
                    .iter()
                    .skip(start as usize)
                    .take((end - start) as usize)
                {
                    let state = self
                        .registry
                        .get(member.source)?
                        .with_snapshot(|state| SourceHealth::from(state));
                    let decision = if !member.enabled {
                        None
                    } else if let Some(snapshot) = &snapshot {
                        snapshot.endpoints.get(&member.source).cloned()
                    } else {
                        // Pure policy construction has no event consumers or
                        // transport ownership. Cached source safe never feeds it.
                        let mut endpoint = crate::safety::Endpoint::new(
                            member.policy.clone(),
                            crate::safety::Fence {
                                revision: state.revision,
                                generation: state.generation,
                            },
                        )
                        .expect("Validated safety membership");
                        Some(endpoint.snapshot(now))
                    };
                    observations.push(SafetyMember {
                        source: member.source,
                        enabled: member.enabled,
                        policy: member.policy.clone(),
                        decision,
                        health: state,
                    });
                }
                Diagnostics::Safety {
                    controller_active: running.is_some(),
                    is_safe: snapshot.is_some_and(|snapshot| snapshot.is_safe),
                    members: observations,
                }
            }
            Output::Switch(switch) => Diagnostics::Switch {
                channels: switch.diagnostics(start, end, now)?,
            },
            Output::Weather(weather) => {
                let (average_period_hours, measurements) = weather.diagnostics(start, end, now);
                Diagnostics::Weather {
                    average_period_hours,
                    measurements,
                }
            }
            Output::Focuser(focuser) => {
                let state = focuser.source().snapshot();
                Diagnostics::Focuser {
                    health: SourceHealth::from(&state),
                    properties: crate::focuser::FocuserProperty::ALL
                        .iter()
                        .skip(start as usize)
                        .take((end - start) as usize)
                        .map(|property| crate::diagnostics::FocuserProperty {
                            property: *property,
                            sample: crate::focuser::cached_property(&state, *property, now).into(),
                        })
                        .collect(),
                }
            }
            Output::Rotator(rotator) => {
                let state = rotator.source().snapshot();
                Diagnostics::Rotator {
                    health: SourceHealth::from(&state),
                    properties: crate::rotator::RotatorProperty::ALL
                        .iter()
                        .skip(start as usize)
                        .take((end - start) as usize)
                        .map(|property| crate::diagnostics::RotatorProperty {
                            property: *property,
                            sample: crate::rotator::cached_property(&state, *property, now).into(),
                        })
                        .collect(),
                }
            }
            Output::FilterWheel(wheel) => {
                let state = wheel.source().snapshot();
                Diagnostics::FilterWheel {
                    health: SourceHealth::from(&state),
                    properties: crate::filterwheel::FilterWheelProperty::ALL
                        .iter()
                        .skip(start as usize)
                        .take((end - start) as usize)
                        .map(|property| crate::diagnostics::FilterWheelProperty {
                            property: *property,
                            sample: crate::filterwheel::cached_property(&state, *property, now)
                                .into(),
                        })
                        .collect(),
                }
            }
            Output::CoverCalibrator(panel) => {
                let state = panel.source().snapshot();
                Diagnostics::CoverCalibrator {
                    health: SourceHealth::from(&state),
                    properties: crate::covercalibrator::CoverCalibratorProperty::ALL
                        .iter()
                        .skip(start as usize)
                        .take((end - start) as usize)
                        .map(|property| crate::diagnostics::CoverCalibratorProperty {
                            property: *property,
                            sample: crate::covercalibrator::cached_property(&state, *property, now)
                                .into(),
                        })
                        .collect(),
                }
            }
        };
        Ok(OutputStatus {
            purpose: "cachedDiagnostics",
            output,
            configuration_revision: self.config.revision,
            observed_seconds: now.as_secs_f64(),
            device_type: config.device.device_type(),
            simulated: config.device.sources().iter().any(|source| {
                self.registry
                    .get(*source)
                    .is_ok_and(|source| source.with_snapshot(|state| state.simulated))
            }),
            start,
            limit,
            total,
            next_start: (end < total).then_some(end),
            diagnostics,
        })
    }
    /// Setup probing owns a temporary connection and participates in apply's
    /// quiescence check just like a pending output connection.
    pub async fn inspect_source(
        &self,
        source: Uuid,
        start: u32,
        limit: u32,
    ) -> Result<crate::capabilities::Inspection, SourceError> {
        let config = self
            .config
            .sources
            .iter()
            .find(|entry| entry.id == source)
            .ok_or_else(|| SourceError::new(ErrorKind::InvalidValue, "Unknown source ID"))?;
        let _activity = {
            let lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.closed {
                return Err(disconnected());
            }
            if lifecycle.frozen {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "Configuration is being applied",
                ));
            }
            Activity::new(self.activity.clone())
        };
        crate::capabilities::inspect(
            config,
            self.config.source_type(source).expect("Validated source"),
            self.registry.get(source)?,
            &*self.clock,
            start,
            limit,
        )
        .await
    }
    pub fn revision(&self) -> Uuid {
        self.config.revision
    }
    pub async fn update_simulation(
        &self,
        source: Uuid,
        update: crate::simulated::SimulationUpdate,
    ) -> Result<crate::simulated::SimulationStatus, SourceError> {
        if !self.config.sources.iter().any(|entry| {
            entry.id == source
                && matches!(
                    entry.backend,
                    crate::config::SourceBackend::Simulated { .. }
                )
        }) {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Select an explicitly simulated source",
            ));
        }
        let _activity = {
            let lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.closed {
                return Err(disconnected());
            }
            if lifecycle.frozen {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "Configuration is being applied",
                ));
            }
            Activity::new(self.activity.clone())
        };
        let lease = crate::readout::SourceLease::acquire(self.registry.get(source)?).await?;
        lease.source.control(lease.id, true).await?;
        let result = lease.source.update_simulation(lease.id, update).await;
        // Acknowledgement includes releasing local control. Drop releases the
        // remaining connection lease asynchronously; an immediate camera or
        // accessory command must not race that cleanup for control ownership.
        let release = lease.source.control(lease.id, false).await;
        let status = result?;
        release?;
        Ok(status)
    }
    pub fn outputs(&self) -> Vec<OutputDescriptor> {
        self.config
            .outputs
            .iter()
            .map(|output| OutputDescriptor {
                id: output.id,
                number: output.number,
                label: output.label.clone(),
                device_type: output.device.device_type(),
                simulated: output.device.sources().iter().any(|source| {
                    self.registry
                        .get(*source)
                        .is_ok_and(|source| source.with_snapshot(|state| state.simulated))
                }),
            })
            .collect()
    }
    pub fn source_snapshots(&self) -> Vec<SourceSnapshot> {
        self.registry.snapshots()
    }

    /// Counts setup inspections, pending connections and connections still retained by in-flight
    /// commands after client disconnect. Zero does not prove worker teardown;
    /// configuration replacement must separately drain the old registry.
    pub fn active_connections(&self) -> usize {
        self.activity.active()
    }
    pub fn start_focuser_group(
        &self,
        host: Uuid,
        expected_revision: Uuid,
        group: Uuid,
        target: i32,
    ) -> Result<HostedFocuserStatus, SourceError> {
        let lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.closed {
            return Err(disconnected());
        }
        if lifecycle.frozen {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Configuration replacement is in progress",
            ));
        }
        self.group_revision(expected_revision)?;
        // Reserve retained activity while holding the same lifecycle lock used
        // by quiesce. EOF of the admitting client never releases that activity.
        self.groups.start(host, self.revision(), group, target)
    }
    pub fn focuser_group_status(
        &self,
        expected_revision: Uuid,
        group: Uuid,
        operation: Option<Uuid>,
    ) -> Result<HostedFocuserStatus, SourceError> {
        self.group_revision(expected_revision)?;
        self.groups.status(group, operation)
    }
    pub fn cancel_focuser_group(
        &self,
        expected_revision: Uuid,
        group: Uuid,
        operation: Uuid,
    ) -> Result<HostedFocuserStatus, SourceError> {
        self.group_revision(expected_revision)?;
        self.groups.cancel(group, operation)
    }
    fn group_revision(&self, expected_revision: Uuid) -> Result<(), SourceError> {
        if expected_revision != self.revision() {
            return Err(SourceError::new(
                ErrorKind::InvalidValue,
                "Configuration revision changed",
            ));
        }
        Ok(())
    }

    pub fn client(self: &Arc<Self>) -> Arc<ClientSession> {
        self.client_with_id(Uuid::new_v4())
    }
    pub(crate) fn client_with_id(self: &Arc<Self>, id: Uuid) -> Arc<ClientSession> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let client = Arc::new(ClientSession {
            id,
            runtime: self.clone(),
            state: Mutex::new(ClientState {
                closed: lifecycle.closed,
                connections: BTreeMap::new(),
                change: None,
                connection_errors: BTreeMap::new(),
            }),
        });
        lifecycle
            .clients
            .retain(|client| client.strong_count() != 0);
        if !lifecycle.closed {
            lifecycle.clients.push(Arc::downgrade(&client));
        }
        client
    }

    /// Atomically exclude new connection reservations while proving no pending
    /// or retained operation exists. No mutex is held during staging/device I/O.
    pub(crate) fn quiesce(self: &Arc<Self>) -> Result<Quiescent, SourceError> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.closed {
            return Err(disconnected());
        }
        if lifecycle.frozen || self.active_connections() != 0 {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Disconnect all outputs before applying configuration",
            ));
        }
        lifecycle.frozen = true;
        Ok(Quiescent(self.clone()))
    }

    /// Permanently stop admission, revoke safety, cancel pending clients, and
    /// drain every source. Retain cleanup errors instead of retrying ambiguous
    /// disconnects. Cancellation can resume shutdown, never reopen this runtime.
    pub async fn shutdown(&self) -> Result<(), Vec<(Uuid, SourceError)>> {
        self.shutdown
            .get_or_init(|| async {
                let clients = {
                    let mut lifecycle = self.lifecycle.lock().unwrap();
                    lifecycle.closed = true;
                    std::mem::take(&mut lifecycle.clients)
                };
                for client in clients.into_iter().filter_map(|client| client.upgrade()) {
                    client.close();
                }
                self.groups.stop().await;
                for output in self.outputs.values() {
                    if let Output::Safety { active, .. } = output
                        && let Some(output) = active.lock().unwrap().upgrade()
                    {
                        output.shutdown();
                    }
                }
                let result = self.registry.shutdown().await;
                for camera in self.cameras.values() {
                    camera.retire_after_source_shutdown().await;
                }
                result
            })
            .await
            .clone()
    }

    async fn open(&self, id: Uuid) -> Result<ConnectedDevice, SourceError> {
        if self.lifecycle.lock().unwrap().closed {
            return Err(disconnected());
        }
        match self.outputs.get(&id).ok_or_else(unknown_output)? {
            Output::Camera(camera) => Ok(ConnectedDevice::Camera(camera.connect().await?)),
            Output::Safety { members, active } => {
                // This lock only creates/subscribes policy tasks; it never waits
                // for source I/O. All clients of this output share one policy.
                let mut cached = active.lock().unwrap();
                let output = match cached.upgrade() {
                    Some(output) => output,
                    None => {
                        let output = Arc::new(
                            SafetyOutput::new(members, &self.registry, self.clock.clone())
                                .map_err(|_| {
                                    SourceError::new(
                                        ErrorKind::InvalidValue,
                                        "Invalid safety output",
                                    )
                                })?,
                        );
                        *cached = Arc::downgrade(&output);
                        output
                    }
                };
                Ok(ConnectedDevice::Safety(output))
            }
            Output::Switch(output) => Ok(ConnectedDevice::Switch {
                definition: output.clone(),
                session: output.connect().await?,
            }),
            Output::Weather(output) => Ok(ConnectedDevice::Weather(output.connect().await?)),
            Output::Focuser(output) => Ok(ConnectedDevice::Focuser(output.connect().await?)),
            Output::Rotator(output) => Ok(ConnectedDevice::Rotator(output.connect_modern().await?)),
            Output::FilterWheel(output) => {
                Ok(ConnectedDevice::FilterWheel(output.connect().await?))
            }
            Output::CoverCalibrator(output) => {
                Ok(ConnectedDevice::CoverCalibrator(output.connect().await?))
            }
        }
    }
}

fn validate_outputs(config: &HubConfig) -> Result<(), Vec<FieldError>> {
    let mut errors = config.validate();
    for (index, output) in config.outputs.iter().enumerate() {
        if matches!(output.device, VirtualDevice::Proxy { device_type, .. } if !matches!(device_type, DeviceType::Camera | DeviceType::Focuser | DeviceType::Rotator | DeviceType::FilterWheel | DeviceType::CoverCalibrator))
        {
            errors.push(FieldError::new(
                format!("outputs[{index}].device"),
                "unsupported",
                "This proxy device class is not available in this runtime yet",
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
fn unknown_output() -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, "Unknown output ID")
}
fn disconnected() -> SourceError {
    SourceError::new(
        ErrorKind::Disconnected,
        "This client is not connected to the output",
    )
}
fn wrong_type() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "Operation does not match the output's device class",
    )
}

pub(crate) struct Quiescent(Arc<HubRuntime>);
impl Drop for Quiescent {
    fn drop(&mut self) {
        self.0.lifecycle.lock().unwrap().frozen = false;
    }
}

enum ConnectedDevice {
    Camera(CameraSession),
    Safety(Arc<SafetyOutput>),
    Switch {
        definition: Arc<SwitchOutput>,
        session: SwitchSession,
    },
    Weather(WeatherSession),
    Focuser(FocuserSession),
    Rotator(RotatorSession),
    FilterWheel(FilterWheelSession),
    CoverCalibrator(CoverCalibratorSession),
}
/// Hold this guard throughout a command. Its leases outlive a simultaneous
/// frontend disconnect; dropping a client does not imply motion rollback.
pub struct OutputConnection {
    device: ConnectedDevice,
    _activity: Activity,
    // DeviceState uses the same monotonic clock as the source observations.
    clock: Arc<dyn Clock>,
}
impl OutputConnection {
    pub fn connected(&self) -> bool {
        match &self.device {
            ConnectedDevice::Camera(session) => session.connected(),
            ConnectedDevice::Focuser(session) => session.connected(),
            ConnectedDevice::Rotator(session) => session.connected(),
            ConnectedDevice::FilterWheel(session) => session.connected(),
            ConnectedDevice::CoverCalibrator(session) => session.connected(),
            _ => true,
        }
    }
    pub fn rotator(&self) -> Result<&RotatorSession, SourceError> {
        match &self.device {
            ConnectedDevice::Rotator(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub fn camera(&self) -> Result<&CameraSession, SourceError> {
        match &self.device {
            ConnectedDevice::Camera(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub fn filterwheel(&self) -> Result<&FilterWheelSession, SourceError> {
        match &self.device {
            ConnectedDevice::FilterWheel(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub fn covercalibrator(&self) -> Result<&CoverCalibratorSession, SourceError> {
        match &self.device {
            ConnectedDevice::CoverCalibrator(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub fn focuser(&self) -> Result<&FocuserSession, SourceError> {
        match &self.device {
            ConnectedDevice::Focuser(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub(crate) fn now(&self) -> std::time::Duration {
        self.clock.now()
    }
    pub fn safety(&self) -> Result<&SafetyOutput, SourceError> {
        match &self.device {
            ConnectedDevice::Safety(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
    pub fn switch(&self) -> Result<&SwitchSession, SourceError> {
        match &self.device {
            ConnectedDevice::Switch { session, .. } => Ok(session),
            _ => Err(wrong_type()),
        }
    }
    pub fn switch_definition(&self) -> Result<&SwitchOutput, SourceError> {
        match &self.device {
            ConnectedDevice::Switch { definition, .. } => Ok(definition),
            _ => Err(wrong_type()),
        }
    }
    pub fn weather(&self) -> Result<&WeatherSession, SourceError> {
        match &self.device {
            ConnectedDevice::Weather(value) => Ok(value),
            _ => Err(wrong_type()),
        }
    }
}

enum ClientConnection {
    Pending {
        token: Uuid,
        _cancel: oneshot::Sender<()>,
    },
    Ready(Arc<OutputConnection>),
}
struct ClientState {
    closed: bool,
    connections: BTreeMap<Uuid, ClientConnection>,
    change: Option<(Uuid, Uuid)>, // operation token, output
    connection_errors: BTreeMap<Uuid, SourceError>,
}
pub struct ClientSession {
    id: Uuid,
    runtime: Arc<HubRuntime>,
    state: Mutex<ClientState>,
}
impl ClientSession {
    pub(crate) fn runtime_id(&self) -> Uuid {
        self.runtime.runtime_id()
    }
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub async fn connect(self: &Arc<Self>, output: Uuid) -> Result<(), SourceError> {
        self.connect_inner(output, None).await
    }
    async fn connect_inner(
        self: &Arc<Self>,
        output: Uuid,
        change: Option<Uuid>,
    ) -> Result<(), SourceError> {
        if !self.runtime.outputs.contains_key(&output) {
            return Err(unknown_output());
        }
        let token = Uuid::new_v4();
        let (cancel, cancelled) = oneshot::channel();
        let mut pending = {
            let lifecycle = self.runtime.lifecycle.lock().unwrap();
            if lifecycle.closed {
                return Err(disconnected());
            }
            if lifecycle.frozen {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "Hub configuration is being applied",
                ));
            }
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(disconnected());
            }
            if state.change.map(|(token, _)| token) != change {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "A client connection change is in progress",
                ));
            }
            state.connection_errors.remove(&output);
            match state.connections.get(&output) {
                Some(ClientConnection::Ready(_)) => return Ok(()),
                Some(ClientConnection::Pending { .. }) => {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Output connection is already in progress",
                    ));
                }
                None => {}
            }
            state.connections.insert(
                output,
                ClientConnection::Pending {
                    token,
                    _cancel: cancel,
                },
            );
            Pending {
                client: Arc::downgrade(self),
                output,
                token,
                activity: Some(Activity::new(self.runtime.activity.clone())),
                armed: true,
            }
        };
        let device = tokio::select! {
            biased;
            _ = cancelled => return Err(disconnected()),
            result = self.runtime.open(output) => result?,
        };
        let mut state = self.state.lock().unwrap();
        if state.closed
            || !matches!(state.connections.get(&output), Some(ClientConnection::Pending { token: current, .. }) if *current == token)
        {
            return Err(disconnected());
        }
        state.connections.insert(
            output,
            ClientConnection::Ready(Arc::new(OutputConnection {
                device,
                _activity: pending.activity.take().expect("Connection reservation"),
                clock: self.runtime.clock.clone(),
            })),
        );
        pending.armed = false;
        Ok(())
    }

    /// One admitted, supervised connection operation per client. Reserve before
    /// spawning so Connecting and apply quiescence cannot miss a queued change.
    /// Dropping the waiter does not replay/cancel an accepted change; EOF closes
    /// the client and cancels its pending connection through the existing fence.
    pub async fn change_connection(
        self: &Arc<Self>,
        output: Uuid,
        connected: bool,
        asynchronous: bool,
    ) -> Result<(), SourceError> {
        if !self.runtime.contains_output(output) {
            return Err(unknown_output());
        }
        let token = Uuid::new_v4();
        let activity = {
            let lifecycle = self.runtime.lifecycle.lock().unwrap();
            if lifecycle.closed {
                return Err(disconnected());
            }
            if lifecycle.frozen {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "Hub configuration is being applied",
                ));
            }
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(disconnected());
            }
            if state.change.is_some()
                || state
                    .connections
                    .values()
                    .any(|connection| matches!(connection, ClientConnection::Pending { .. }))
            {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "A client connection change is in progress",
                ));
            }
            state.connection_errors.remove(&output);
            if connected
                == matches!(
                    state.connections.get(&output),
                    Some(ClientConnection::Ready(_))
                )
            {
                return Ok(());
            }
            state.change = Some((token, output));
            Activity::new(self.runtime.activity.clone())
        };
        let mut guard = ConnectionChange {
            client: self.clone(),
            token,
            output,
            _activity: activity,
            finished: false,
        };
        let task = tokio::spawn(async move {
            // Async admission returns before IPC's outer operation deadline.
            // Keep the accepted task bounded independently of its waiter.
            let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                if connected {
                    guard.client.connect_inner(output, Some(token)).await
                } else {
                    guard.client.disconnect(output);
                    Ok(())
                }
            })
            .await
            .unwrap_or_else(|_| Err(SourceError::uncertain()));
            guard.finish(&result);
            result
        });
        if asynchronous {
            Ok(())
        } else {
            task.await.unwrap_or_else(|_| Err(SourceError::uncertain()))
        }
    }
    pub fn connecting(&self, output: Uuid) -> Result<bool, SourceError> {
        if !self.runtime.contains_output(output) {
            return Err(unknown_output());
        }
        let state = self.state.lock().unwrap();
        if state.closed {
            return Err(disconnected());
        }
        if let Some(error) = state.connection_errors.get(&output) {
            return Err(error.clone());
        }
        Ok(state.change.is_some_and(|(_, changing)| changing == output)
            || matches!(
                state.connections.get(&output),
                Some(ClientConnection::Pending { .. })
            ))
    }
    pub(crate) fn disconnect_checked(&self, output: Uuid) -> Result<(), SourceError> {
        let mut state = self.state.lock().unwrap();
        if state.change.is_some() {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "A client connection change is in progress",
            ));
        }
        state.connection_errors.remove(&output);
        let connection = state.connections.remove(&output);
        drop(state);
        drop(connection);
        Ok(())
    }

    pub fn connection(&self, output: Uuid) -> Result<Arc<OutputConnection>, SourceError> {
        match self.state.lock().unwrap().connections.get(&output) {
            Some(ClientConnection::Ready(value)) => Ok(value.clone()),
            Some(ClientConnection::Pending { .. }) => Err(SourceError::new(
                ErrorKind::Connecting,
                "Output connection is in progress",
            )),
            None => Err(disconnected()),
        }
    }
    pub fn disconnect(&self, output: Uuid) {
        // Drop outside the lock: cleanup may schedule source tasks.
        let connection = self.state.lock().unwrap().connections.remove(&output);
        drop(connection);
    }
    /// EOF invalidates this client permanently, including pending connects.
    /// Commands already holding an OutputConnection finish under their own
    /// deadlines and release their leases when those guards are dropped.
    pub fn close(&self) {
        let connections = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.connection_errors.clear();
            std::mem::take(&mut state.connections)
        };
        drop(connections);
    }
}

struct ConnectionChange {
    client: Arc<ClientSession>,
    token: Uuid,
    output: Uuid,
    _activity: Activity,
    finished: bool,
}
impl ConnectionChange {
    fn finish(&mut self, result: &Result<(), SourceError>) {
        let mut state = self.client.state.lock().unwrap();
        if state.change == Some((self.token, self.output)) {
            state.change = None;
            if !state.closed
                && let Err(error) = result
            {
                state.connection_errors.insert(self.output, error.clone());
            }
        }
        self.finished = true;
    }
}
impl Drop for ConnectionChange {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(&Err(SourceError::uncertain()));
        }
    }
}

struct Pending {
    client: Weak<ClientSession>,
    output: Uuid,
    token: Uuid,
    activity: Option<Activity>,
    armed: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.armed
            && let Some(client) = self.client.upgrade()
        {
            let mut state = client.state.lock().unwrap();
            if matches!(state.connections.get(&self.output), Some(ClientConnection::Pending { token, .. }) if *token == self.token)
            {
                state.connections.remove(&self.output);
            }
        }
    }
}
