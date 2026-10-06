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
}
impl Session {
    fn new() -> Self {
        Self {
            client: OnceCell::new(),
            gate: Arc::new(AsyncMutex::new(())),
            outputs: Mutex::new(BTreeSet::new()),
            retired: AtomicBool::new(false),
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
    state: Mutex<State>,
}
impl Publisher {
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
        Ok(Arc::new(Self {
            endpoint,
            instance,
            catalog,
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
            return Ok(session.clone());
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
    async fn connection(self: &Arc<Self>, id: u32, output: Uuid, on: bool) -> Result<()> {
        // Supervise accepted connection changes through HTTP caller cancellation.
        // No global lock is held across I/O; other clients and cached safety run.
        let session = if on {
            self.reserve(id)?
        } else {
            match self.existing(id) {
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
        tokio::spawn(async move {
            let _gate = gate;
            ensure!(
                !session.retired.load(Ordering::SeqCst),
                error(
                    0x407,
                    "Hub client was disconnected; connect explicitly again"
                )
            );
            let result = async {
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
                client
                    .request(if on {
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
            // Failed changes close the whole session: do not conceal an uncertain
            // connection or leave its leases unreachable after caller cancellation.
            if result.is_err() || session.outputs.lock().unwrap().is_empty() {
                owner.retire(id, &session);
            }
            result
        })
        .await
        .map_err(|_| error(0x500, "Hub connection task stopped; outcome is uncertain"))?
    }
    pub async fn request(
        self: &Arc<Self>,
        device: &OutputDescriptor,
        member: &str,
        put: bool,
        params: &Params,
    ) -> Result<Value> {
        let id = params.optional_id("ClientID")?;
        if member == "connected" {
            if put {
                self.connection(id, device.id, params.boolean("Connected")?)
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
                "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
                // Synchronous Connected contract until modern connection/state
                // operations are implemented and checked for each class.
                "interfaceversion" => {
                    return Ok(json!(if device.device_type == DeviceType::Switch {
                        2
                    } else {
                        1
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
        if matches!(
            &command,
            Command::Get {
                property: Get::SensorDescription { .. },
                ..
            }
        ) && !client
            .hello()
            .capabilities
            .iter()
            .any(|capability| capability == "weatherSensorDescription")
        {
            return Err(unsupported(
                "SensorDescription requires an updated shared hub host",
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
            _ => return Err(unsupported(member)),
        };
        return Ok(Command::Put { output, property });
    }
    let property = match (device.device_type, member) {
        (DeviceType::SafetyMonitor, "issafe") => Get::IsSafe {},
        (DeviceType::Switch, "maxswitch") => Get::MaxSwitch {},
        (DeviceType::Switch, "getswitch") => Get::GetSwitch { id: channel(p)? },
        (DeviceType::Switch, "getswitchvalue") => Get::GetSwitchValue { id: channel(p)? },
        (DeviceType::Switch, "getswitchname") => Get::GetSwitchName { id: channel(p)? },
        (DeviceType::Switch, "getswitchdescription") => {
            Get::GetSwitchDescription { id: channel(p)? }
        }
        (DeviceType::Switch, "canwrite") => Get::CanWrite { id: channel(p)? },
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
