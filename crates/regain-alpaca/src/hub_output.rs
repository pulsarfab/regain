//! Alpaca scalar outputs over private hub IPC. HTTP never owns source actors.
use crate::device::{Params, error, unsupported};
use anyhow::{Result, ensure};
use regain_hub::{
    client::{Client, ClientError, ClientLimits},
    config::DeviceType,
    endpoint::Endpoint,
    ipc::{Command, Get, Put},
    runtime::OutputDescriptor,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use uuid::Uuid;

// Leave room within the host's 32-connection bound for native frontends/setup.
const MAX_CLIENTS: usize = 24;
struct Session {
    client: OnceCell<Client>,
    gate: Arc<AsyncMutex<()>>,
    outputs: Mutex<BTreeSet<Uuid>>,
    retired: AtomicBool,
    progress: Mutex<Option<ConnectionProgress>>,
}
struct ConnectionProgress {
    output: Uuid,
    error: Option<(i32, String)>,
}
impl Session {
    fn new() -> Self {
        Self {
            client: OnceCell::new(),
            gate: Arc::new(AsyncMutex::new(())),
            outputs: Mutex::new(BTreeSet::new()),
            retired: AtomicBool::new(false),
            progress: Mutex::new(None),
        }
    }
    fn close(&self) {
        self.retired.store(true, Ordering::SeqCst);
        if let Some(client) = self.client.get() {
            client.close();
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
struct State {
    closed: bool,
    clients: HashMap<u32, Arc<Session>>,
}
pub struct Publisher {
    endpoint: Endpoint,
    instance: Uuid,
    catalog: Client,
    setup_client: Mutex<Client>,
    setup_gate: AsyncMutex<()>,
    state: Mutex<State>,
}
impl Publisher {
    /// Setup requests retain the host's structured validation and uncertainty.
    /// This private catalog session never acquires output connection leases.
    pub(crate) async fn setup(&self, command: Command) -> Result<Value, ClientError> {
        if !matches!(
            &command,
            Command::DescribeConfig {}
                | Command::GetConfig {}
                | Command::HostStatus {}
                | Command::SourceStatus { .. }
                | Command::InspectSource { .. }
                | Command::ValidateConfig { .. }
                | Command::ApplyConfig { .. }
                | Command::UpdateSimulation { .. }
                | Command::CreateCredential { .. }
                | Command::CredentialStatus { .. }
                | Command::DeleteCredential { .. }
        ) {
            return Err(ClientError::InvalidRequest);
        }
        let _gate = self.setup_gate.try_lock().map_err(|_| ClientError::Busy)?;
        if self.state.lock().unwrap().closed {
            return Err(ClientError::Disconnected);
        }
        let client = self.setup_client.lock().unwrap().clone();
        client.request(command).await
    }
    /// Explicit setup reattachment only. Never replace the catalog, reacquire
    /// output leases, start a host or replay a previous setup request.
    pub(crate) async fn reload_setup(&self) -> Result<Value, ClientError> {
        let _gate = self.setup_gate.try_lock().map_err(|_| ClientError::Busy)?;
        if self.state.lock().unwrap().closed {
            return Err(ClientError::Disconnected);
        }
        let client = Client::connect(
            &self.endpoint,
            self.instance,
            Duration::from_secs(10),
            ClientLimits::default(),
        )
        .await?;
        let state = self.state.lock().unwrap();
        if state.closed {
            client.close();
            return Err(ClientError::Disconnected);
        }
        let result = json!({"instanceId":client.hello().instance_id,"hostInstance":client.hello().host_instance});
        let old = std::mem::replace(&mut *self.setup_client.lock().unwrap(), client);
        old.close();
        Ok(result)
    }
    /// The shared host must already be attached. No automatic restart/replay.
    pub async fn connect(endpoint: Endpoint, instance: Uuid) -> Result<Arc<Self>> {
        let catalog = Client::connect(
            &endpoint,
            instance,
            Duration::from_secs(10),
            ClientLimits::default(),
        )
        .await
        .map_err(translate)?;
        let setup_client = Client::connect(
            &endpoint,
            instance,
            Duration::from_secs(10),
            ClientLimits::default(),
        )
        .await
        .map_err(translate)?;
        ensure!(
            setup_client.hello().host_instance == catalog.hello().host_instance,
            "Hub changed during publisher initialization; start again"
        );
        Ok(Arc::new(Self {
            endpoint,
            instance,
            catalog,
            setup_client: Mutex::new(setup_client),
            setup_gate: AsyncMutex::new(()),
            state: Mutex::new(State {
                closed: false,
                clients: HashMap::new(),
            }),
        }))
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        for session in state.clients.drain().map(|(_, session)| session) {
            session.close();
        }
        self.catalog.close();
        self.setup_client.lock().unwrap().close();
    }
    pub async fn devices(&self) -> Result<Vec<OutputDescriptor>> {
        let value = self
            .catalog
            .request(Command::ListDevices {})
            .await
            .map_err(translate)?;
        let devices: Vec<OutputDescriptor> = serde_json::from_value(value)
            .map_err(|_| error(0x500, "Invalid hub device catalog"))?;
        ensure!(
            devices.iter().all(|d| matches!(
                d.device_type,
                DeviceType::Switch | DeviceType::SafetyMonitor | DeviceType::ObservingConditions
            )),
            error(
                0x400,
                "Hub output class is not yet supported by this frontend"
            )
        );
        Ok(devices)
    }
    pub async fn configured(&self) -> Result<Vec<Value>> {
        Ok(self.devices().await?.iter().map(|d| json!({"DeviceName":label(d),"DeviceType":class_name(d.device_type),"DeviceNumber":d.number,"UniqueID":d.id})).collect())
    }
    fn existing(&self, id: u32) -> Option<Arc<Session>> {
        self.state.lock().unwrap().clients.get(&id).cloned()
    }
    fn reserve(&self, id: u32) -> Result<Arc<Session>> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            !state.closed && self.catalog.is_connected(),
            error(0x407, "Hub is disconnected; attach the server again")
        );
        if let Some(session) = state.clients.get(&id) {
            if !session.retired.load(Ordering::SeqCst) {
                return Ok(session.clone());
            }
            let replacement = Arc::new(Session::new());
            state.clients.insert(id, replacement.clone());
            return Ok(replacement);
        }
        ensure!(
            state.clients.len() < MAX_CLIENTS,
            error(
                0x40b,
                "Hub HTTP client limit reached; disconnect an unused client"
            )
        );
        let session = Arc::new(Session::new());
        state.clients.insert(id, session.clone());
        Ok(session)
    }
    fn retire(&self, id: u32, session: &Arc<Session>) {
        session.close();
        let mut state = self.state.lock().unwrap();
        if state
            .clients
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            state.clients.remove(&id);
        }
    }
    async fn connection(
        self: &Arc<Self>,
        id: u32,
        output: Uuid,
        on: bool,
        asynchronous: bool,
    ) -> Result<()> {
        // Supervise accepted connection changes through HTTP caller cancellation.
        // No global lock is held across I/O; other clients and cached safety run.
        let session = if on {
            self.reserve(id)?
        } else {
            match self.existing(id) {
                Some(session) if session.retired.load(Ordering::SeqCst) => {
                    self.retire(id, &session);
                    return Ok(());
                }
                Some(session) => session,
                None => return Ok(()),
            }
        };
        // Admit before spawning. A burst cannot create unbounded queued tasks.
        let gate = session
            .gate
            .clone()
            .try_lock_owned()
            .map_err(|_| error(0x40b, "This client's hub connection is changing"))?;
        let owner = self.clone();
        *session.progress.lock().unwrap() = Some(ConnectionProgress {
            output,
            error: None,
        });
        let mut progress = ConnectionGuard {
            owner: owner.clone(),
            session: session.clone(),
            id,
            output,
            asynchronous,
            finished: false,
        };
        let task = tokio::spawn(async move {
            let _gate = gate;
            let result = async {
                ensure!(
                    !session.retired.load(Ordering::SeqCst),
                    error(
                        0x407,
                        "Hub client was disconnected; connect explicitly again"
                    )
                );
                let client = session
                    .client
                    .get_or_try_init(|| {
                        Client::connect(
                            &owner.endpoint,
                            owner.instance,
                            Duration::from_secs(10),
                            ClientLimits::default(),
                        )
                    })
                    .await
                    .map_err(translate)?;
                if session.retired.load(Ordering::SeqCst) {
                    client.close();
                    return Err(error(0x407, "Hub frontend is shutting down"));
                }
                // A live catalog and the new stream must refer to the same host.
                ensure!(
                    client.hello().host_instance == owner.catalog.hello().host_instance,
                    error(0x407, "Hub host changed; attach the server again")
                );
                let modern = client
                    .hello()
                    .capabilities
                    .iter()
                    .any(|capability| capability == "asyncOutputConnection");
                client
                    .request(if modern {
                        // The HTTP wrapper covers its own pipe initialization;
                        // the host owns the operation and its retained result.
                        Command::ChangeConnection {
                            output,
                            connected: on,
                            asynchronous: false,
                        }
                    } else if on {
                        Command::Connect { output }
                    } else {
                        Command::Disconnect { output }
                    })
                    .await
                    .map_err(translate)?;
                let mut outputs = session.outputs.lock().unwrap();
                if on {
                    outputs.insert(output);
                } else {
                    outputs.remove(&output);
                }
                Ok(())
            }
            .await;
            progress.finish(&result);
            result
        });
        if asynchronous {
            Ok(())
        } else {
            task.await
                .map_err(|_| error(0x500, "Hub connection task stopped; outcome is uncertain"))?
        }
    }
    pub async fn request(
        self: &Arc<Self>,
        device: &OutputDescriptor,
        member: &str,
        put: bool,
        params: &Params,
    ) -> Result<Value> {
        let id = params.optional_id("ClientID")?;
        if (put && matches!(member, "connect" | "disconnect")) || (!put && member == "connecting") {
            ensure!(
                self.catalog
                    .hello()
                    .capabilities
                    .iter()
                    .any(|capability| capability == "asyncOutputConnection"),
                unsupported(member)
            );
            if put {
                self.connection(id, device.id, member == "connect", true)
                    .await?;
                return Ok(Value::Null);
            }
            let Some(session) = self.existing(id) else {
                return Ok(json!(false));
            };
            if let Some(progress) = &*session.progress.lock().unwrap()
                && progress.output == device.id
            {
                if let Some((code, message)) = &progress.error {
                    return Err(error(*code, message.clone()));
                }
                return Ok(json!(true));
            }
            return Ok(json!(false));
        }
        if member == "connected" {
            if put {
                self.connection(id, device.id, params.boolean("Connected")?, false)
                    .await?;
                return Ok(Value::Null);
            }
            let Some(session) = self.existing(id) else {
                return Ok(json!(false));
            };
            let Some(client) = session.client.get() else {
                return Ok(json!(false));
            };
            if !client.is_connected() || session.retired.load(Ordering::SeqCst) {
                return Ok(json!(false));
            }
            return client
                .request(Command::Get {
                    output: device.id,
                    property: Get::Connected {},
                })
                .await
                .map_err(translate);
        }
        if !put {
            match member {
                "name" => return Ok(json!(label(device))),
                "description" => {
                    return Ok(json!(format!(
                        "Regain Hub {}",
                        class_name(device.device_type)
                    )));
                }
                "driverinfo" => {
                    return Ok(json!(
                        "PulsarFab Regain shared hub; source status and policies are managed by the local host"
                    ));
                }
                "driverversion" => {
                    return Ok(json!(concat!(
                        env!("CARGO_PKG_VERSION_MAJOR"),
                        ".",
                        env!("CARGO_PKG_VERSION_MINOR")
                    )));
                }
                "interfaceversion" => {
                    let capabilities = &self.catalog.hello().capabilities;
                    let modern =
                        ["scalarDeviceState", "asyncOutputConnection"]
                            .iter()
                            .all(|required| {
                                capabilities.iter().any(|capability| capability == required)
                            });
                    return Ok(json!(match device.device_type {
                        DeviceType::Switch
                            if modern
                                && capabilities
                                    .iter()
                                    .any(|capability| capability == "switchAsyncContract") =>
                            3,
                        DeviceType::Switch => 2,
                        DeviceType::SafetyMonitor if modern => 3,
                        DeviceType::ObservingConditions if modern => 2,
                        _ => 1,
                    }));
                }
                "supportedactions" => return Ok(json!([])),
                _ => (),
            }
        }
        if matches!(
            member,
            "action" | "commandblind" | "commandbool" | "commandstring" | "setswitchname"
        ) {
            return Err(unsupported(member));
        }
        let command = operation(device, member, put, params)?;
        let session = self
            .existing(id)
            .ok_or_else(|| error(0x407, "Connect this Alpaca client first"))?;
        ensure!(
            !session.retired.load(Ordering::SeqCst),
            error(0x407, "Hub client is disconnected")
        );
        let client = session
            .client
            .get()
            .ok_or_else(|| error(0x407, "Hub client is not connected"))?;
        let capability = match &command {
            Command::Get {
                property: Get::SensorDescription { .. },
                ..
            } => Some("weatherSensorDescription"),
            Command::Get {
                property: Get::DeviceState {},
                ..
            } => Some("scalarDeviceState"),
            Command::Get {
                property: Get::CanAsync { .. } | Get::StateChangeComplete { .. },
                ..
            }
            | Command::Put {
                property: Put::SetAsync { .. } | Put::SetAsyncValue { .. } | Put::CancelAsync { .. },
                ..
            } => Some("switchAsyncContract"),
            _ => None,
        };
        if capability.is_some_and(|required| {
            !client
                .hello()
                .capabilities
                .iter()
                .any(|available| available == required)
        }) {
            return Err(unsupported(
                "This member requires an updated shared hub host",
            ));
        }
        let value = client.request(command).await.map_err(translate)?;
        if device.device_type == DeviceType::ObservingConditions
            && !put
            && serde_json::from_value::<regain_hub::config::WeatherMetric>(json!(member)).is_ok()
        {
            // IPC preserves sample provenance/age; the standard property is a number.
            return value
                .get("value")
                .filter(|v| v.as_f64().is_some())
                .cloned()
                .ok_or_else(|| error(0x500, "Invalid hub weather reading"));
        }
        Ok(value)
    }
}
struct ConnectionGuard {
    owner: Arc<Publisher>,
    session: Arc<Session>,
    id: u32,
    output: Uuid,
    asynchronous: bool,
    finished: bool,
}
impl ConnectionGuard {
    fn finish(&mut self, result: &Result<()>) {
        if let Err(failure) = result {
            let (code, message) = failure
                .downcast_ref::<crate::device::Error>()
                .map(|failure| (failure.0, failure.1.clone()))
                .unwrap_or((0x500, "Hub connection failed; outcome is uncertain".into()));
            *self.session.progress.lock().unwrap() = Some(ConnectionProgress {
                output: self.output,
                error: Some((code, message)),
            });
            // Revoke every private lease on failure. Retain an asynchronous
            // failure in the bounded client slot until explicit reconciliation,
            // so polling Connecting cannot mistake failure for success.
            self.session.close();
            self.session.outputs.lock().unwrap().clear();
            if !self.asynchronous {
                self.owner.retire(self.id, &self.session);
            }
        } else {
            *self.session.progress.lock().unwrap() = None;
            if self.session.outputs.lock().unwrap().is_empty() {
                self.owner.retire(self.id, &self.session);
            }
        }
        self.finished = true;
    }
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(&Err(error(
                0x500,
                "Hub connection task stopped; outcome is uncertain",
            )));
        }
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.close();
    }
}
pub fn class_name(kind: DeviceType) -> &'static str {
    match kind {
        DeviceType::Switch => "Switch",
        DeviceType::SafetyMonitor => "SafetyMonitor",
        DeviceType::ObservingConditions => "ObservingConditions",
        _ => "Unsupported",
    }
}
fn label(device: &OutputDescriptor) -> String {
    if device.simulated {
        format!("{} (Simulation)", device.label)
    } else {
        device.label.clone()
    }
}
fn channel(params: &Params) -> Result<u32> {
    u32::try_from(params.integer("Id")?).map_err(|_| error(0x401, "Invalid switch channel"))
}
fn operation(device: &OutputDescriptor, member: &str, put: bool, p: &Params) -> Result<Command> {
    let output = device.id;
    if put {
        let property = match (device.device_type, member) {
            (DeviceType::Switch, "setswitch") => Put::SetSwitch {
                id: channel(p)?,
                state: p.boolean("State")?,
            },
            (DeviceType::Switch, "setswitchvalue") => Put::SetSwitchValue {
                id: channel(p)?,
                value: p.number("Value")?,
            },
            (DeviceType::ObservingConditions, "averageperiod") => Put::AveragePeriod {
                hours: p.number("AveragePeriod")?,
            },
            (DeviceType::ObservingConditions, "refresh") => Put::Refresh {},
            (DeviceType::Switch, "setasync") => Put::SetAsync {
                id: channel(p)?,
                state: p.boolean("State")?,
            },
            (DeviceType::Switch, "setasyncvalue") => Put::SetAsyncValue {
                id: channel(p)?,
                value: p.number("Value")?,
            },
            (DeviceType::Switch, "cancelasync") => Put::CancelAsync { id: channel(p)? },
            _ => return Err(unsupported(member)),
        };
        return Ok(Command::Put { output, property });
    }
    let property = match (device.device_type, member) {
        (_, "devicestate") => Get::DeviceState {},
        (DeviceType::SafetyMonitor, "issafe") => Get::IsSafe {},
        (DeviceType::Switch, "maxswitch") => Get::MaxSwitch {},
        (DeviceType::Switch, "getswitch") => Get::GetSwitch { id: channel(p)? },
        (DeviceType::Switch, "getswitchvalue") => Get::GetSwitchValue { id: channel(p)? },
        (DeviceType::Switch, "getswitchname") => Get::GetSwitchName { id: channel(p)? },
        (DeviceType::Switch, "getswitchdescription") => {
            Get::GetSwitchDescription { id: channel(p)? }
        }
        (DeviceType::Switch, "canwrite") => Get::CanWrite { id: channel(p)? },
        (DeviceType::Switch, "canasync") => Get::CanAsync { id: channel(p)? },
        (DeviceType::Switch, "statechangecomplete") => Get::StateChangeComplete { id: channel(p)? },
        (DeviceType::Switch, "minswitchvalue") => Get::MinSwitchValue { id: channel(p)? },
        (DeviceType::Switch, "maxswitchvalue") => Get::MaxSwitchValue { id: channel(p)? },
        (DeviceType::Switch, "switchstep") => Get::SwitchStep { id: channel(p)? },
        (DeviceType::ObservingConditions, "averageperiod") => Get::AveragePeriod {},
        (DeviceType::ObservingConditions, "timesincelastupdate") => Get::TimeSinceLastUpdate {
            sensor: p.string("SensorName")?.into(),
        },
        (DeviceType::ObservingConditions, "sensordescription") => Get::SensorDescription {
            sensor: p.string("SensorName")?.into(),
        },
        (DeviceType::ObservingConditions, _) => Get::Measurement {
            metric: serde_json::from_value(json!(member)).map_err(|_| unsupported(member))?,
        },
        _ => return Err(unsupported(member)),
    };
    Ok(Command::Get { output, property })
}
fn translate(failure: ClientError) -> anyhow::Error {
    let code = match &failure {
        ClientError::Disconnected => 0x407,
        ClientError::Busy => 0x40b,
        ClientError::InvalidRequest => 0x401,
        ClientError::Remote(remote) => match remote.code.as_str() {
            "unsupported" => 0x400,
            "invalidValue" => 0x401,
            "disconnected" | "connecting" => 0x407,
            "busy" => 0x40b,
            "unavailable" => 0x402,
            _ => 0x500,
        },
        _ => 0x500,
    };
    // Export only Regain-controlled diagnostic text, not upstream driver strings.
    let message = match &failure {
        ClientError::Remote(remote) => format!(
            "Hub request failed ({})",
            match remote.code.as_str() {
                "unsupported" => "unsupported",
                "invalidValue" => "invalid value",
                "disconnected" => "disconnected",
                "connecting" => "connecting",
                "busy" => "busy",
                "unavailable" => "unavailable",
                "uncertain" => "uncertain outcome; do not replay",
                _ => "source failure",
            }
        ),
        _ => failure.to_string(),
    };
    error(code, message)
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    #[tokio::test]
    async fn explicit_setup_reload_recovers_only_setup_and_respects_admission_and_shutdown() {
        use regain_hub::{
            config::{ConfigStore, HubConfig},
            factory::NoCredentials,
            host,
            ipc::Limits,
            native::NativeRuntime,
            runtime::HubRuntime,
            safety::MonotonicClock,
            service::HubService,
        };
        let directory = tempfile::tempdir().unwrap();
        let config = HubConfig::empty();
        let path = directory.path().join("hub.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let endpoint = Endpoint::for_config(&path).unwrap();
        let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
        let service = HubService::persistent(
            ConfigStore::load(&path).unwrap(),
            Arc::new(|config| {
                HubRuntime::build(
                    config,
                    &NativeRuntime {
                        directory: "unused-fixture".into(),
                        simulate: false,
                    },
                    &NoCredentials,
                    Arc::new(MonotonicClock::default()),
                )
            }),
        )
        .unwrap();
        let stop = regain_core::CancellationToken::new();
        let host = tokio::spawn(host::serve_service(
            listener,
            service,
            Limits::default(),
            stop.clone(),
        ));
        let publisher = Publisher::connect(endpoint, config.instance_id)
            .await
            .unwrap();
        let host_id = publisher.catalog.hello().host_instance;
        publisher.setup_client.lock().unwrap().close();
        assert!(matches!(
            publisher.setup(Command::GetConfig {}).await,
            Err(ClientError::Disconnected)
        ));
        assert!(publisher.devices().await.unwrap().is_empty()); // Catalog survived.
        let gate = publisher.setup_gate.lock().await;
        assert!(matches!(
            publisher.reload_setup().await,
            Err(ClientError::Busy)
        ));
        assert!(matches!(
            publisher.setup(Command::GetConfig {}).await,
            Err(ClientError::Busy)
        ));
        drop(gate);
        assert_eq!(
            publisher.reload_setup().await.unwrap()["hostInstance"],
            json!(host_id)
        );
        assert_eq!(
            publisher.setup(Command::GetConfig {}).await.unwrap()["revision"],
            json!(config.revision)
        );
        publisher.close();
        assert!(matches!(
            publisher.reload_setup().await,
            Err(ClientError::Disconnected)
        ));
        assert!(matches!(
            publisher.setup(Command::GetConfig {}).await,
            Err(ClientError::Disconnected)
        ));
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), host)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
