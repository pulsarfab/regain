//! Filter-wheel and focuser Alpaca endpoints, sharing the native USB worker.
use crate::{
    device::{Params, error, unsupported},
    rotator::Worker,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, io::Write, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::BufReader, process::Command, sync::Mutex};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub serial: Option<String>,
    pub unique_id: String,
    pub names: Vec<String>,
    pub focus_offsets: Vec<i32>,
    pub unidirectional: bool,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            serial: None,
            unique_id: uuid::Uuid::new_v4().to_string(),
            names: vec![],
            focus_offsets: vec![],
            unidirectional: false,
        }
    }
}
struct State {
    profile: Profile,
    loaded: bool,
    worker: Option<Worker>,
    clients: HashSet<u32>,
}
pub struct Accessory {
    kind: &'static str,
    path: Option<PathBuf>,
    executable: PathBuf,
    simulate: bool,
    state: Mutex<State>,
}
impl Accessory {
    pub fn new(
        kind: &'static str,
        path: Option<PathBuf>,
        directory: PathBuf,
        simulate: bool,
    ) -> Self {
        Self {
            kind,
            path,
            executable: directory.join(format!("regain-device{}", std::env::consts::EXE_SUFFIX)),
            simulate,
            state: Mutex::new(State {
                profile: Profile::default(),
                loaded: false,
                worker: None,
                clients: HashSet::new(),
            }),
        }
    }
    fn name(&self) -> &'static str {
        if self.kind == "efw" {
            "PulsarFab regain EFW Filter Wheel"
        } else if self.kind == "eta" {
            "PulsarFab regain Wanderer Astro ETA M54"
        } else if self.kind == "fc3" {
            "PulsarFab regain Pegasus FocusCube3"
        } else {
            "PulsarFab regain EAF Focuser"
        }
    }
    fn load(&self, s: &mut State) -> Result<()> {
        if !s.loaded {
            if let Some(path) = &self.path
                && path.exists()
            {
                s.profile = serde_json::from_slice(&std::fs::read(path)?)?;
            }
            ensure!(
                uuid::Uuid::parse_str(&s.profile.unique_id).is_ok(),
                "invalid accessory UUID"
            );
            self.validate(&s.profile)?;
            s.loaded = true;
        }
        Ok(())
    }
    fn validate(&self, p: &Profile) -> Result<()> {
        ensure!(
            p.serial.as_ref().is_none_or(|s| if self.kind == "eta" {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            } else if self.kind == "fc3" {
                s.len() == 17
                    && s.split(':').count() == 6
                    && s.split(':')
                        .all(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            } else {
                s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
            }),
            error(0x401, "Select a device by serial")
        );
        ensure!(
            p.names.len() <= 16
                && p.names.len() == p.focus_offsets.len()
                && p.names
                    .iter()
                    .all(|n| !n.trim().is_empty() && n.len() <= 128)
                && (p.focus_offsets.is_empty() || p.focus_offsets.contains(&0)),
            error(
                0x401,
                "Use a name for each slot and at least one zero focus offset"
            )
        );
        Ok(())
    }
    fn save(&self, s: &State) -> Result<()> {
        if let Some(path) = &self.path {
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir)?;
            let mut temp = tempfile::NamedTempFile::new_in(dir)?;
            temp.write_all(&serde_json::to_vec_pretty(&s.profile)?)?;
            temp.as_file().sync_all()?;
            temp.persist(path)?;
        }
        Ok(())
    }
    fn command(&self, action: &str) -> Command {
        let mut c = Command::new(&self.executable);
        c.args([
            match self.kind {
                "fc3" => "pegasus",
                "eta" => "wanderer",
                _ => "zwo",
            },
            self.kind,
        ]);
        c.arg(action);
        if self.simulate {
            c.arg("--simulate");
        }
        c.kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        c.creation_flags(0x08000000);
        c
    }
    pub async fn setup(&self) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        Ok(
            json!({"profile":s.profile,"connected":!s.clients.is_empty(),"simulation":self.simulate}),
        )
    }
    pub async fn configure(&self, value: Value) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        ensure!(
            s.clients.is_empty(),
            error(
                0x40b,
                "Disconnect all accessory clients before editing the profile"
            )
        );
        let mut profile: Profile = serde_json::from_value(value)?;
        profile.unique_id = s.profile.unique_id.clone();
        self.validate(&profile)?;
        let old = s.profile.clone();
        s.profile = profile;
        if let Err(e) = self.save(&s) {
            s.profile = old;
            return Err(e);
        }
        Ok(json!(s.profile))
    }
    pub async fn discover(&self) -> Result<Value> {
        let s = self.state.lock().await;
        ensure!(
            s.clients.is_empty(),
            error(0x40b, "Disconnect before scanning USB")
        );
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            self.command("list-details").output(),
        )
        .await??;
        ensure!(output.status.success(), "accessory discovery failed");
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    pub async fn configured(&self) -> Result<Option<Value>> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        Ok(s.profile.serial.as_ref().map(|_|json!({"DeviceName":self.name(),"DeviceType":if self.kind=="efw"{"FilterWheel"}else{"Focuser"},"DeviceNumber":if self.kind=="eta"{2}else if self.kind=="fc3"{1}else{0},"UniqueID":s.profile.unique_id})))
    }
    async fn connect(&self, s: &mut State, client: u32, on: bool) -> Result<()> {
        if !on {
            s.clients.remove(&client);
            if s.clients.is_empty()
                && let Some(worker) = s.worker.take()
            {
                worker.close().await;
            }
            return Ok(());
        }
        if s.worker.is_some() {
            s.clients.insert(client);
            return Ok(());
        }
        let serial = s
            .profile
            .serial
            .as_ref()
            .ok_or_else(|| error(0x40b, "Select an accessory on its setup page first"))?
            .clone();
        let mut child = self.command("serve").arg("--serial").arg(&serial).spawn()?;
        let mut worker = Worker {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        };
        let result: Result<()> = async {
            let identity = worker.request(json!({"command":"identity"})).await?;
            ensure!(
                identity["serial"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(&serial)),
                "USB identity changed"
            );
            let status = worker.request(json!({"command":"status"})).await?;
            if self.kind == "efw" {
                let count = status["slots"]
                    .as_u64()
                    .ok_or_else(|| error(0x500, "Invalid slot count"))?
                    as usize;
                if s.profile.names.len() != count {
                    s.profile.names = (1..=count).map(|i| format!("Filter {i}")).collect();
                    s.profile.focus_offsets = vec![0; count];
                    self.save(s)?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            worker.close().await;
            return Err(error);
        }
        s.worker = Some(worker);
        s.clients.insert(client);
        Ok(())
    }
    pub async fn request(&self, member: &str, put: bool, p: &Params) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        if let Some(w) = s.worker.as_mut()
            && w.child.try_wait()?.is_some()
        {
            s.worker.take();
            s.clients.clear();
        }
        let client = p.optional_id("ClientID")?;
        if member == "connected" || (self.kind != "efw" && member == "link") {
            if put {
                self.connect(
                    &mut s,
                    client,
                    p.boolean(if member == "link" {
                        "Link"
                    } else {
                        "Connected"
                    })?,
                )
                .await?;
                return Ok(Value::Null);
            }
            return Ok(json!(s.clients.contains(&client)));
        }
        if !put {
            match member {
                "name" | "description" => return Ok(json!(self.name())),
                "driverinfo" => return Ok(json!("PulsarFab regain SDK-free native USB driver")),
                "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
                "interfaceversion" => return Ok(json!(if self.kind == "efw" { 2 } else { 3 })),
                "supportedactions" => {
                    return Ok(if self.kind == "eta" {
                        json!([
                            "Regain.Status",
                            "Regain.Identity",
                            "Regain.MovePoint",
                            "Regain.CancelQueued"
                        ])
                    } else if self.kind == "efw" {
                        json!(["Regain.Status", "Regain.Identity", "Regain.Calibrate"])
                    } else {
                        json!(["Regain.Status", "Regain.Identity"])
                    });
                }
                _ => (),
            }
        }
        ensure!(
            s.clients.contains(&client),
            error(0x407, "This accessory client is not connected")
        );
        if member == "action" && put {
            if self.kind == "eta"
                && crate::branding::action_name(p.string("Action")?) == "regain.movepoint"
            {
                let value: Value = serde_json::from_str(p.string("Parameters")?)
                    .map_err(|_| error(0x401, "Expected JSON point and position"))?;
                let point = value["point"]
                    .as_u64()
                    .filter(|v| (1..=3).contains(v))
                    .ok_or_else(|| error(0x401, "Point must be 1..3"))?;
                let position = value["position"]
                    .as_i64()
                    .filter(|v| (0..=1200).contains(v))
                    .ok_or_else(|| error(0x401, "Position must be 0..1200 micrometres"))?;
                ensure!(
                    value.as_object().is_some_and(|o| o
                        .keys()
                        .all(|k| ["point", "position"].contains(&k.as_str()))),
                    error(0x401, "Unknown point parameter")
                );
                return Ok(json!(
                    s.worker
                        .as_mut()
                        .unwrap()
                        .request(json!({"command":"move-point","point":point,"position":position}))
                        .await?
                        .to_string()
                ));
            }
            let command = match crate::branding::action_name(p.string("Action")?).as_str() {
                "regain.status" => "status",
                "regain.identity" => "identity",
                "regain.cancelqueued" if self.kind == "eta" => "cancel-queued",
                "regain.calibrate" if self.kind == "efw" => "calibrate",
                _ => return Err(error(0x40c, "Unknown action")),
            };
            return Ok(json!(
                s.worker
                    .as_mut()
                    .unwrap()
                    .request(json!({"command":command}))
                    .await?
                    .to_string()
            ));
        }
        if !put && self.kind == "efw" {
            match member {
                "names" => return Ok(json!(s.profile.names)),
                "focusoffsets" => return Ok(json!(s.profile.focus_offsets)),
                _ => (),
            }
        }
        if !put && self.kind != "efw" {
            match member {
                "absolute" => return Ok(json!(true)),
                "tempcomp" | "tempcompavailable" => return Ok(json!(false)),
                "stepsize" if self.kind == "eta" => return Ok(json!(1.0)),
                "stepsize" => return Err(unsupported(member)),
                _ => (),
            }
        }
        if !put {
            let status = s
                .worker
                .as_mut()
                .unwrap()
                .request(json!({"command":"status"}))
                .await?;
            ensure!(
                status["error"] == 0 && status["fault"].is_null(),
                error(0x500, format!("Accessory fault: {status}"))
            );
            return match member {
                "position" => Ok(if self.kind == "efw" && status["moving"] == true {
                    json!(-1)
                } else {
                    status["position"].clone()
                }),
                "ismoving" if self.kind != "efw" => Ok(status["moving"].clone()),
                "maxstep" | "maxincrement" if self.kind != "efw" => Ok(status["max_step"].clone()),
                "temperature" if self.kind != "efw" => {
                    if status["temperature_c"].is_null() {
                        Err(unsupported(member))
                    } else {
                        Ok(status["temperature_c"].clone())
                    }
                }
                _ => Err(unsupported(member)),
            };
        }
        if (member == "position" && self.kind == "efw") || (member == "move" && self.kind != "efw")
        {
            let position = p.integer("Position")?;
            let status = s
                .worker
                .as_mut()
                .unwrap()
                .request(json!({"command":"status"}))
                .await?;
            let maximum = if self.kind == "efw" {
                status["slots"].as_i64().unwrap_or(0) - 1
            } else {
                status["max_step"].as_i64().unwrap_or(-1)
            };
            ensure!(
                (0..=maximum).contains(&position),
                error(0x401, "Position is out of range")
            );
            let unidirectional = s.profile.unidirectional;
            s.worker
                .as_mut()
                .unwrap()
                .request(
                    json!({"command":"move","position":position,"unidirectional":unidirectional}),
                )
                .await?;
            return Ok(Value::Null);
        }
        if member == "halt" && self.kind == "eta" {
            return Err(unsupported("ETA has no documented stop command"));
        }
        if member == "halt" && self.kind != "efw" {
            s.worker
                .as_mut()
                .unwrap()
                .request(json!({"command":"halt"}))
                .await?;
            return Ok(Value::Null);
        }
        Err(unsupported(member))
    }
    pub async fn settings(&self, value: Value) -> Result<Value> {
        let mut s = self.state.lock().await;
        ensure!(
            self.kind != "efw",
            error(0x400, "EFW settings belong to its profile")
        );
        ensure!(
            self.kind != "eta",
            error(0x400, "ETA has no motor settings; use MovePoint or Move")
        );
        let mut request = value;
        let obj = request
            .as_object_mut()
            .ok_or_else(|| error(0x401, "Expected settings object"))?;
        ensure!(
            obj.keys().all(|k| if self.kind == "fc3" {
                ["speed", "reverse", "backlash"].contains(&k.as_str())
            } else {
                ["beep", "reverse", "backlash", "max_step"].contains(&k.as_str())
            }),
            error(0x401, "Unknown setting")
        );
        obj.insert("command".into(), json!("settings"));
        s.worker
            .as_mut()
            .ok_or_else(|| error(0x407, "Connect for setup first"))?
            .request(request)
            .await
    }
    pub async fn shutdown(&self) {
        let mut s = self.state.lock().await;
        if let Some(w) = s.worker.take() {
            w.close().await;
        }
        s.clients.clear();
    }
}
