//! Alpaca rotator backed by the same exclusive HID worker as the native frontends.
use crate::device::{Params, error, unsupported};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, io::Write, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const ACTIONS: &[&str] = &[
    "ZwoGain.CAA.Status",
    "ZwoGain.CAA.Settings",
    "ZwoGain.CAA.Identity",
    "ZwoGain.CAA.ResetOrigin",
    "ZwoGain.CAA.SetReference",
    "ZwoGain.CAA.SetLimit",
    "ZwoGain.CAA.SetBeep",
    "ZwoGain.CAA.SetAlias",
    "ZwoGain.CAA.RotateUnwrapped",
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub serial: Option<String>,
    pub unique_id: String,
    pub logical_offset: f64,
    pub coordinates_uncertain: bool,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            serial: None,
            unique_id: uuid::Uuid::new_v4().to_string(),
            logical_offset: 0.,
            coordinates_uncertain: false,
        }
    }
}
pub(crate) struct Worker {
    pub(crate) child: Child,
    pub(crate) input: Option<ChildStdin>,
    pub(crate) output: BufReader<ChildStdout>,
}
impl Worker {
    pub(crate) async fn request(&mut self, request: Value) -> Result<Value> {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let bytes = format!("{request}\n");
            self.input
                .as_mut()
                .ok_or_else(|| error(0x407, "USB accessory is disconnected"))?
                .write_all(bytes.as_bytes())
                .await?;
            let mut line = String::new();
            ensure!(
                self.output.read_line(&mut line).await? > 0,
                "USB accessory worker exited; movement was not retried"
            );
            let reply: Value = serde_json::from_str(&line)?;
            ensure!(
                reply["ok"] == true,
                "{}",
                reply["error"]
                    .as_str()
                    .unwrap_or("USB accessory command failed")
            );
            Ok(reply["result"].clone())
        })
        .await;
        match result {
            Ok(v) => v,
            Err(_) => {
                self.input.take();
                let _ = self.child.kill().await;
                Err(error(
                    0x500,
                    "USB accessory worker timed out; movement was not retried",
                ))
            }
        }
    }
    pub(crate) async fn close(mut self) {
        self.input.take();
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}
struct State {
    profile: Profile,
    loaded: bool,
    worker: Option<Worker>,
    clients: HashSet<u32>,
}
pub struct Rotator {
    path: Option<PathBuf>,
    executable: PathBuf,
    simulate: bool,
    state: Mutex<State>,
}
impl Rotator {
    pub fn new(path: Option<PathBuf>, directory: PathBuf, simulate: bool) -> Self {
        Self {
            path,
            executable: directory.join(if cfg!(windows) {
                "zwogain-caa.exe"
            } else {
                "zwogain-caa"
            }),
            simulate,
            state: Mutex::new(State {
                profile: Profile::default(),
                loaded: false,
                worker: None,
                clients: HashSet::new(),
            }),
        }
    }
    fn load(&self, s: &mut State) -> Result<()> {
        if !s.loaded {
            if let Some(p) = &self.path
                && p.exists()
            {
                s.profile = serde_json::from_slice(&std::fs::read(p)?)?;
            }
            ensure!(
                s.profile.logical_offset.is_finite()
                    && uuid::Uuid::parse_str(&s.profile.unique_id).is_ok(),
                "Invalid rotator profile"
            );
            s.loaded = true;
        }
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
    fn command(&self, command: &str) -> Command {
        let mut c = Command::new(&self.executable);
        c.arg(command);
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
    pub async fn configured(&self) -> Result<Option<Value>> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        Ok(s.profile.serial.as_ref().map(|_| json!({"DeviceName":"ZWOgain CAA Rotator","DeviceType":"Rotator","DeviceNumber":0,"UniqueID":s.profile.unique_id})))
    }
    pub async fn select(&self, serial: Option<String>) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        ensure!(
            s.clients.is_empty(),
            error(
                0x40B,
                "Disconnect all rotator clients before selecting a device"
            )
        );
        ensure!(
            serial
                .as_ref()
                .is_none_or(|v| v.len() == 16 && v.bytes().all(|b| b.is_ascii_hexdigit())),
            error(0x401, "Choose a CAA serial from discovery")
        );
        let mut profile = s.profile.clone();
        if profile.serial != serial {
            profile.logical_offset = 0.;
            profile.coordinates_uncertain = false;
        }
        profile.serial = serial;
        let old = std::mem::replace(&mut s.profile, profile);
        if let Err(e) = self.save(&s) {
            s.profile = old;
            return Err(e);
        }
        Ok(Value::Null)
    }
    pub async fn discover(&self) -> Result<Value> {
        let s = self.state.lock().await;
        ensure!(
            s.clients.is_empty(),
            error(0x40B, "Disconnect the rotator before scanning USB")
        );
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            self.command("list-details").output(),
        )
        .await??;
        ensure!(output.status.success(), "CAA discovery failed");
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    async fn remember(&self, s: &mut State) -> Result<Value> {
        let status = s
            .worker
            .as_mut()
            .ok_or_else(|| error(0x407, "CAA is disconnected"))?
            .request(json!({"command":"status"}))
            .await?;
        if status["moving"] == false && status["error"] == 0 && status["motion_error"].is_null() {
            let offset = status["logical_offset"]
                .as_f64()
                .ok_or_else(|| error(0x500, "Invalid CAA offset"))?;
            if s.profile.coordinates_uncertain || s.profile.logical_offset != offset {
                s.profile.logical_offset = offset;
                s.profile.coordinates_uncertain = false;
                self.save(s)?;
            }
        }
        Ok(status)
    }
    async fn connect(&self, s: &mut State, client: u32, on: bool) -> Result<()> {
        if !on {
            s.clients.remove(&client);
            if s.clients.is_empty() && s.worker.is_some() {
                let result = s
                    .worker
                    .as_mut()
                    .unwrap()
                    .request(json!({"command":"stop"}))
                    .await;
                if result.is_ok() {
                    let _ = self.remember(s).await;
                }
                s.worker.take().unwrap().close().await;
                result?;
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
            .ok_or_else(|| error(0x40B, "Select a CAA on the setup page first"))?
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
                "CAA identity changed"
            );
            if s.profile.coordinates_uncertain {
                s.profile.logical_offset = 0.;
                s.profile.coordinates_uncertain = false;
                self.save(s)?;
            }
            let status = worker.request(json!({"command":"status"})).await?;
            let angle = (status["logical_degrees"].as_f64().unwrap() + s.profile.logical_offset)
                .rem_euclid(360.);
            worker
                .request(json!({"command":"sync","degrees":angle}))
                .await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            worker.close().await;
            return Err(e);
        }
        s.worker = Some(worker);
        s.clients.insert(client);
        Ok(())
    }
    pub async fn request(&self, member: &str, put: bool, p: &Params) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        if let Some(worker) = s.worker.as_mut()
            && worker.child.try_wait()?.is_some()
        {
            s.worker.take();
            s.clients.clear();
        }
        let client = p.optional_id("ClientID")?;
        if member == "connected" {
            if put {
                self.connect(&mut s, client, p.boolean("Connected")?)
                    .await?;
                return Ok(Value::Null);
            }
            return Ok(json!(s.clients.contains(&client)));
        }
        if !put {
            match member {
                "name" => return Ok(json!("ZWOgain CAA Rotator")),
                "description" => return Ok(json!("ZWO CAA rotator over USB HID")),
                "driverinfo" => return Ok(json!("ZWOgain native Rust CAA driver")),
                "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
                "interfaceversion" => return Ok(json!(3)),
                "supportedactions" => return Ok(json!(ACTIONS)),
                _ => (),
            }
        }
        ensure!(
            s.clients.contains(&client),
            error(0x407, "This rotator client is not connected")
        );
        if !put {
            return match member {
                "canreverse" => Ok(json!(true)),
                "stepsize" => Ok(json!(0.02)),
                "reverse" => Ok(s
                    .worker
                    .as_mut()
                    .unwrap()
                    .request(json!({"command":"settings"}))
                    .await?["reverse"]
                    .clone()),
                "position" | "mechanicalposition" | "targetposition" | "ismoving" => {
                    let status = self.remember(&mut s).await?;
                    if member == "ismoving" {
                        ensure!(
                            status["error"] == 0 && status["motion_error"].is_null(),
                            error(0x500, format!("CAA motion fault: {status}"))
                        );
                        Ok(status["moving"].clone())
                    } else {
                        let key = match member {
                            "position" => "logical_degrees",
                            "mechanicalposition" => "mechanical_degrees",
                            _ => "target_degrees",
                        };
                        Ok(json!(status[key].as_f64().unwrap().rem_euclid(360.)))
                    }
                }
                _ => Err(unsupported(member)),
            };
        }
        let (request, reference) = match member {
            "halt" => (json!({"command":"stop"}), false),
            "reverse" => (
                json!({"command":"reverse","enabled":p.boolean("Reverse")?}),
                true,
            ),
            "move" | "moveabsolute" | "movemechanical" | "sync" => {
                let degrees = p.number("Position")?;
                ensure!(
                    if member == "move" {
                        degrees.abs() <= 360.
                    } else {
                        (0.0..360.0).contains(&degrees)
                    },
                    error(0x401, "Position is outside the ASCOM angle range")
                );
                (
                    json!({"command":match member {"move"=>"move-relative","moveabsolute"=>"move-to","movemechanical"=>"move-mechanical",_=>"sync"},"degrees":degrees}),
                    member == "sync",
                )
            }
            "action" => {
                let name = p.string("Action")?;
                let index = ACTIONS
                    .iter()
                    .position(|a| a.eq_ignore_ascii_case(name))
                    .ok_or_else(|| error(0x40C, "Unknown CAA action"))?;
                let command = [
                    "status",
                    "settings",
                    "identity",
                    "reset-origin",
                    "reference",
                    "limit",
                    "beep",
                    "alias",
                    "rotate-unwrapped",
                ][index];
                let mut args = if index < 4 {
                    json!({})
                } else {
                    serde_json::from_str::<Value>(p.string("Parameters")?)?
                };
                ensure!(
                    args.is_object(),
                    error(0x401, "Action parameters must be a JSON object")
                );
                args["command"] = json!(command);
                (args, matches!(index, 3 | 4 | 8))
            }
            _ => return Err(unsupported(member)),
        };
        if reference {
            s.profile.coordinates_uncertain = true;
            self.save(&s)?;
        }
        let value = s.worker.as_mut().unwrap().request(request).await?;
        self.remember(&mut s).await?;
        Ok(if member == "action" {
            json!(value.to_string())
        } else {
            Value::Null
        })
    }
    pub async fn shutdown(&self) {
        let mut s = self.state.lock().await;
        s.clients.clear();
        if let Some(mut worker) = s.worker.take() {
            let _ = worker.request(json!({"command":"stop"})).await;
            worker.close().await;
        }
    }
}
