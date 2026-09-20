//! ASCOM CoverCalibrator V1 over the native OFP2 serial worker.
use crate::{
    device::{Params, error, unsupported},
    rotator::Worker,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, io::Write, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::BufReader, process::Command, sync::Mutex};

const NAME: &str = "Deep Sky Dad OFP2 · ZWOgain";
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub serial: Option<String>,
    pub unique_id: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            serial: None,
            unique_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}
struct State {
    profile: Profile,
    loaded: bool,
    worker: Option<Worker>,
    clients: HashSet<u32>,
    fault: Option<String>,
}
pub struct FlatPanel {
    path: Option<PathBuf>,
    executable: PathBuf,
    simulate: bool,
    state: Mutex<State>,
}
impl FlatPanel {
    pub fn new(path: Option<PathBuf>, directory: PathBuf, simulate: bool) -> Self {
        Self {
            path,
            executable: directory.join(if cfg!(windows) {
                "zwogain-ofp2.exe"
            } else {
                "zwogain-ofp2"
            }),
            simulate,
            state: Mutex::new(State {
                profile: Profile::default(),
                loaded: false,
                worker: None,
                clients: HashSet::new(),
                fault: None,
            }),
        }
    }
    fn validate(p: &Profile) -> Result<()> {
        ensure!(
            p.serial.as_ref().is_none_or(|s| !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')),
            error(0x401, "Select an OFP2 USB serial number")
        );
        ensure!(
            uuid::Uuid::parse_str(&p.unique_id).is_ok(),
            "Invalid flat panel UUID"
        );
        Ok(())
    }
    fn load(&self, s: &mut State) -> Result<()> {
        if !s.loaded {
            if let Some(path) = &self.path
                && path.exists()
            {
                s.profile = serde_json::from_slice(&std::fs::read(path)?)?;
            }
            Self::validate(&s.profile)?;
            s.loaded = true;
        }
        Ok(())
    }
    fn command(&self, action: &str) -> Command {
        let mut c = Command::new(&self.executable);
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
            json!({"profile":s.profile,"connected":!s.clients.is_empty(),"simulation":self.simulate,"fault":s.fault}),
        )
    }
    pub async fn configure(&self, value: Value) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        ensure!(
            s.clients.is_empty(),
            error(
                0x40b,
                "Disconnect all flat panel clients before changing the device"
            )
        );
        let mut profile: Profile = serde_json::from_value(value)?;
        profile.unique_id = s.profile.unique_id.clone();
        Self::validate(&profile)?;
        if let Some(path) = &self.path {
            let directory = path.parent().unwrap();
            std::fs::create_dir_all(directory)?;
            let mut temp = tempfile::NamedTempFile::new_in(directory)?;
            temp.write_all(&serde_json::to_vec_pretty(&profile)?)?;
            temp.as_file().sync_all()?;
            temp.persist(path)?;
        }
        s.profile = profile;
        Ok(json!(s.profile))
    }
    pub async fn discover(&self) -> Result<Value> {
        let s = self.state.lock().await;
        ensure!(
            s.clients.is_empty(),
            error(0x40b, "Disconnect before scanning serial ports")
        );
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            self.command("list-details").output(),
        )
        .await??;
        ensure!(output.status.success(), "OFP2 discovery failed");
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    pub async fn configured(&self) -> Result<Option<Value>> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        Ok(s.profile.serial.as_ref().map(|_| json!({"DeviceName":NAME,"DeviceType":"CoverCalibrator","DeviceNumber":0,"UniqueID":s.profile.unique_id})))
    }
    async fn connect(&self, s: &mut State, client: u32, on: bool) -> Result<()> {
        if !on {
            s.clients.remove(&client);
            if s.clients.is_empty() {
                if let Some(w) = s.worker.take() {
                    w.close().await;
                }
                s.fault = None;
            }
            return Ok(());
        }
        ensure!(
            s.fault.is_none(),
            error(
                0x500,
                "Panel faulted; disconnect all clients and check the device before reconnecting"
            )
        );
        if let Some(w) = &mut s.worker {
            ensure!(
                w.child.try_wait()?.is_none(),
                error(
                    0x500,
                    "Panel worker exited; disconnect all clients before reconnecting"
                )
            );
            s.clients.insert(client);
            return Ok(());
        }
        let serial = s
            .profile
            .serial
            .as_ref()
            .ok_or_else(|| error(0x40b, "Select an OFP2 on its setup page first"))?;
        let mut child = self.command("serve").arg("--serial").arg(serial).spawn()?;
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
                    .is_some_and(|v| v.eq_ignore_ascii_case(serial)),
                "OFP2 USB identity changed"
            );
            worker.request(json!({"command":"status"})).await?;
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
    async fn native(s: &mut State, request: Value) -> Result<Value> {
        if let Some(fault) = &s.fault {
            return Err(error(0x500, fault));
        }
        let result = s
            .worker
            .as_mut()
            .ok_or_else(|| error(0x407, "Panel disconnected"))?
            .request(request)
            .await;
        if let Err(e) = &result {
            s.fault = Some(format!("{e:#}"));
        }
        result
    }
    pub async fn request(&self, member: &str, put: bool, p: &Params) -> Result<Value> {
        let mut s = self.state.lock().await;
        self.load(&mut s)?;
        let client = p.optional_id("ClientID")?;
        if member == "connected" {
            if put {
                self.connect(&mut s, client, p.boolean("Connected")?)
                    .await?;
                return Ok(Value::Null);
            }
            let alive = if let Some(w) = &mut s.worker {
                w.child.try_wait()?.is_none()
            } else {
                false
            };
            return Ok(json!(alive && s.clients.contains(&client)));
        }
        if !put {
            match member {
                "name" | "description" => return Ok(json!(NAME)),
                "driverinfo" => {
                    return Ok(json!(
                        "Native Rust OFP2 USB serial driver; no vendor SDK or ASCOM driver required"
                    ));
                }
                "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
                "interfaceversion" => return Ok(json!(1)),
                "supportedactions" => return Ok(json!(["ZwoGain.Status", "ZwoGain.Identity"])),
                _ => (),
            }
        }
        ensure!(
            s.clients.contains(&client),
            error(0x407, "This flat panel client is not connected")
        );
        if put {
            let command = match member {
                "opencover" => "open",
                "closecover" => "close",
                "haltcover" => "halt",
                "calibratoroff" => "off",
                "calibratoron" => "on",
                "action" => {
                    let name = p.string("Action")?.to_ascii_lowercase();
                    let action = match name.as_str() {
                        "zwogain.status" => "status",
                        "zwogain.identity" => "identity",
                        _ => return Err(error(0x40c, "Unknown flat panel action")),
                    };
                    return Ok(json!(
                        Self::native(&mut s, json!({"command":action}))
                            .await?
                            .to_string()
                    ));
                }
                _ => return Err(unsupported(member)),
            };
            let mut request = json!({"command":command});
            if command == "on" {
                let brightness = p.integer("Brightness")?;
                ensure!(
                    (0..=4096).contains(&brightness),
                    error(0x401, "Brightness must be 0..=4096")
                );
                request["brightness"] = json!(brightness);
            }
            if matches!(command, "open" | "close") {
                let status = Self::native(&mut s, json!({"command":"status"})).await?;
                ensure!(
                    status["cover"] != "moving",
                    error(0x40b, "Cover is already moving")
                );
            }
            Self::native(&mut s, request).await?;
            return Ok(Value::Null);
        }
        if member == "maxbrightness" {
            return Ok(json!(4096));
        }
        if !["coverstate", "calibratorstate", "brightness"].contains(&member) {
            return Err(unsupported(member));
        }
        let result = Self::native(&mut s, json!({"command":"status"})).await;
        // ASCOM state enums report asynchronous failures as Error, never Ready/Unknown.
        if result.is_err() && ["coverstate", "calibratorstate"].contains(&member) {
            return Ok(json!(5));
        }
        let status = result?;
        Ok(match member {
            "coverstate" => json!(match status["cover"].as_str() {
                Some("closed") => 1,
                Some("moving") => 2,
                Some("open") => 3,
                _ => 4,
            }),
            "calibratorstate" => json!(if status["calibrator_on"] == true {
                3
            } else {
                1
            }),
            _ => {
                if status["calibrator_on"] == true {
                    status["brightness"].clone()
                } else {
                    json!(0)
                }
            }
        })
    }
    pub async fn shutdown(&self) {
        let mut s = self.state.lock().await;
        if let Some(w) = s.worker.take() {
            w.close().await;
        }
        s.clients.clear();
        s.fault = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn communication_fault_is_an_error_state_until_disconnect() {
        let panel = FlatPanel::new(None, PathBuf::new(), true);
        {
            let mut state = panel.state.lock().await;
            state.clients.insert(7);
            state.fault = Some("Serial response timed out; command was not retried".into());
        }
        let params = Params::parse("ClientID=7").unwrap();
        for member in ["coverstate", "calibratorstate"] {
            assert_eq!(
                panel.request(member, false, &params).await.unwrap(),
                json!(5)
            );
        }
        assert_eq!(
            crate::server::error_code(
                &panel
                    .request("brightness", false, &params)
                    .await
                    .unwrap_err()
            ),
            0x500
        );
        panel
            .request(
                "connected",
                true,
                &Params::parse("ClientID=7&Connected=false").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            crate::server::error_code(
                &panel
                    .request("coverstate", false, &params)
                    .await
                    .unwrap_err()
            ),
            0x407
        );
        assert!(panel.state.lock().await.fault.is_none());
    }
}
