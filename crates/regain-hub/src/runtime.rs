//! One set of output controllers per applied configuration. Client identities
//! and connection leases belong to the host, never to frontend-supplied IDs.
use crate::{
    config::{DeviceType, HubConfig, SafetyMember, VirtualDevice},
    factory::{CredentialProvider, build_sources},
    native::NativeRuntime,
    parameters::FieldError,
    safety::Clock,
    safety_output::SafetyOutput,
    source::{ErrorKind, SourceError, SourceRegistry, SourceSnapshot},
    switch::{SwitchOutput, SwitchSession},
    weather::{WeatherOutput, WeatherSession},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDescriptor {
    pub id: Uuid,
    pub number: u32,
    pub label: String,
    pub device_type: DeviceType,
}

enum Output {
    Safety {
        members: Vec<SafetyMember>,
        active: Mutex<Weak<SafetyOutput>>,
    },
    Switch(Arc<SwitchOutput>),
    Weather(Arc<WeatherOutput>),
}

pub struct HubRuntime {
    config: HubConfig,
    registry: Arc<SourceRegistry>,
    clock: Arc<dyn Clock>,
    outputs: BTreeMap<Uuid, Output>,
    activity: Arc<AtomicUsize>,
    lifecycle: Mutex<Lifecycle>,
    shutdown: tokio::sync::OnceCell<Result<(), Vec<(Uuid, SourceError)>>>,
}
struct Lifecycle {
    closed: bool,
    clients: Vec<Weak<ClientSession>>,
}
impl HubRuntime {
    /// Build controllers without connecting sources. No HTTP listener or ASCOM
    /// Platform is needed for native/network sources.
    pub fn build(
        config: HubConfig,
        native: &NativeRuntime,
        credentials: &dyn CredentialProvider,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        validate_outputs(&config)?;
        let registry = build_sources(&config, native, credentials, clock.clone())?;
        Self::from_registry(config, registry, clock)
    }

    /// Inject a registry for another host adapter or fault tests. The registry
    /// must describe exactly this configuration revision's sources.
    pub fn from_registry(
        config: HubConfig,
        registry: Arc<SourceRegistry>,
        clock: Arc<dyn Clock>,
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
        let mut outputs = BTreeMap::new();
        for (index, output) in config.outputs.iter().enumerate() {
            let mapped = match &output.device {
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
                VirtualDevice::Proxy { .. } => unreachable!("Validated output implementation"),
            };
            outputs.insert(output.id, mapped);
        }
        Ok(Arc::new(Self {
            config,
            registry,
            clock,
            outputs,
            activity: Arc::new(AtomicUsize::new(0)),
            lifecycle: Mutex::new(Lifecycle {
                closed: false,
                clients: Vec::new(),
            }),
            shutdown: tokio::sync::OnceCell::new(),
        }))
    }

    pub fn instance_id(&self) -> Uuid {
        self.config.instance_id
    }
    pub fn revision(&self) -> Uuid {
        self.config.revision
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
            })
            .collect()
    }
    pub fn source_snapshots(&self) -> Vec<SourceSnapshot> {
        self.registry.snapshots()
    }

    /// Counts pending connections and connections still retained by in-flight
    /// commands after client disconnect. Zero does not prove worker teardown;
    /// configuration replacement must separately drain the old registry.
    pub fn active_connections(&self) -> usize {
        self.activity.load(Ordering::SeqCst)
    }

    pub fn client(self: &Arc<Self>) -> Arc<ClientSession> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let client = Arc::new(ClientSession {
            id: Uuid::new_v4(),
            runtime: self.clone(),
            state: Mutex::new(ClientState {
                closed: lifecycle.closed,
                connections: BTreeMap::new(),
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
                for output in self.outputs.values() {
                    if let Output::Safety { active, .. } = output
                        && let Some(output) = active.lock().unwrap().upgrade()
                    {
                        output.shutdown();
                    }
                }
                self.registry.shutdown().await
            })
            .await
            .clone()
    }

    async fn open(&self, id: Uuid) -> Result<ConnectedDevice, SourceError> {
        if self.lifecycle.lock().unwrap().closed {
            return Err(disconnected());
        }
        match self.outputs.get(&id).ok_or_else(unknown_output)? {
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
        }
    }
}

fn validate_outputs(config: &HubConfig) -> Result<(), Vec<FieldError>> {
    let mut errors = config.validate();
    for (index, output) in config.outputs.iter().enumerate() {
        if matches!(output.device, VirtualDevice::Proxy { .. }) {
            errors.push(FieldError::new(
                format!("outputs[{index}].device"),
                "unsupported",
                "Proxy output controllers are not available in this runtime yet",
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

struct Activity(Arc<AtomicUsize>);
impl Activity {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

enum ConnectedDevice {
    Safety(Arc<SafetyOutput>),
    Switch {
        definition: Arc<SwitchOutput>,
        session: SwitchSession,
    },
    Weather(WeatherSession),
}
/// Hold this guard throughout a command. Its leases outlive a simultaneous
/// frontend disconnect; dropping a client does not imply motion rollback.
pub struct OutputConnection {
    device: ConnectedDevice,
    _activity: Activity,
}
impl OutputConnection {
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
}
pub struct ClientSession {
    id: Uuid,
    runtime: Arc<HubRuntime>,
    state: Mutex<ClientState>,
}
impl ClientSession {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub async fn connect(self: &Arc<Self>, output: Uuid) -> Result<(), SourceError> {
        if !self.runtime.outputs.contains_key(&output) {
            return Err(unknown_output());
        }
        let token = Uuid::new_v4();
        let (cancel, cancelled) = oneshot::channel();
        let mut pending = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(disconnected());
            }
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
            })),
        );
        pending.armed = false;
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
            std::mem::take(&mut state.connections)
        };
        drop(connections);
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
