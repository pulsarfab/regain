mod cli;
mod library;
#[allow(dead_code)]
mod raw;
use anyhow::{Result, bail, ensure};
use regain_core::white_balance::{Geometry, Settings as WhiteBalanceSettings, WhiteBalance};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 512 * 1024 * 1024;
#[derive(Clone, PartialEq, Deserialize)]
pub struct Exposure {
    width: i32,
    height: i32,
    bin: i32,
    x: i32,
    y: i32,
    microseconds: i64,
    dark: bool,
}
impl Exposure {
    fn same_geometry(&self, other: &Self) -> bool {
        let mut prior = self.clone();
        prior.microseconds = other.microseconds;
        prior == *other
    }
    fn size(&self) -> Result<usize> {
        ensure!(
            self.width > 0
                && self.height > 0
                && self.width % 8 == 0
                && self.height % 2 == 0
                && self.bin > 0
                && self.x >= 0
                && self.y >= 0
                && self.microseconds > 0,
            "invalid exposure/ROI"
        );
        let size = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|v| v.checked_mul(2))
            .filter(|&v| v <= MAX_FRAME)
            .ok_or_else(|| anyhow::anyhow!("frame too large"))?;
        Ok(size)
    }
}
struct Host {
    sdk: Option<library::Sdk>,
    exposure: Option<Exposure>,
    started: Option<Instant>,
    values: Value,
    fault: String,
    opened: bool,
    sim_info: Value,
    sim_instant: bool,
    camera: Value,
    white_balance: Option<WhiteBalance>,
    video: Option<Exposure>,
    video_pixels: Vec<u8>,
    video_ready: bool,
    video_progress: Option<Instant>,
    video_retry_at: Option<Instant>,
    video_timeouts: u64,
}
impl Host {
    fn stop_video(&mut self) -> Result<()> {
        if self.video.is_some() {
            if let Some(sdk) = &self.sdk {
                sdk.stop_video()?;
            }
            self.video = None;
        }
        self.video_pixels.clear();
        self.video_ready = false;
        self.video_progress = None;
        self.video_retry_at = None;
        self.video_timeouts = 0;
        Ok(())
    }
    fn require_unflipped(&self) -> Result<()> {
        let flip = if let Some(s) = &self.sdk {
            if self.camera["controls"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c["type"] == 9))
            {
                s.get(9)?
            } else {
                0
            }
        } else {
            self.values["9"].as_i64().unwrap_or(0)
        };
        ensure!(
            flip == 0,
            regain_core::Failure::Invalid("managed white balance requires flip 0".into())
        );
        Ok(())
    }
    fn command(&mut self, method: &str, p: Value) -> Result<(Value, Vec<u8>)> {
        let mut bytes = Vec::new();
        let value = match method {
            "list" => {
                // Reject before any SDK call, including while connected but idle.
                // The SDK's property lookup may open unrelated connected devices.
                ensure!(
                    !self.opened,
                    "SDK discovery is unavailable while a camera is open; use the cached camera list"
                );
                if let Some(s) = &mut self.sdk {
                    if p["serials"] == true {
                        s.list_with_serials()?
                    } else {
                        s.list()?
                    }
                } else {
                    let mut info = self.sim_info.clone();
                    if p["serials"] == true {
                        info["serial"] =
                            json!(self.values["simSerial"].as_str().unwrap_or("sim00001"));
                    }
                    json!([info])
                }
            }
            "open" => {
                ensure!(!self.opened, "already open");
                let name = p["name"].as_str().unwrap_or("");
                let serial = crate::asi::normalized_serial(p["serial"].as_str());
                let selected_id = p
                    .get("id")
                    .map(|v| -> Result<i32> {
                        let id = v
                            .as_i64()
                            .ok_or_else(|| anyhow::anyhow!("invalid selected camera ID"))?;
                        ensure!(id >= 0, "invalid selected camera ID");
                        Ok(i32::try_from(id)?)
                    })
                    .transpose()?;
                let mut v = if let Some(s) = &mut self.sdk {
                    if let Some(id) = selected_id {
                        s.open_selected(id, name, serial.as_deref())?
                    } else {
                        s.open(name, serial.as_deref())?
                    }
                } else {
                    ensure!(
                        name == self.sim_info["name"]
                            && selected_id.is_none_or(|id| self.sim_info["id"] == id)
                            && serial.as_deref().is_none_or(|s| s.eq_ignore_ascii_case(
                                self.values["simSerial"].as_str().unwrap_or("sim00001")
                            )),
                        "wrong simulated identity"
                    );
                    let info = self.sim_info.clone();
                    json!({"serial":self.values["simSerial"].as_str().unwrap_or("sim00001"),"sdkVersion":"simulator","info":info,"controls":[
                        {"type":0,"min":0,"max":600,"default":100,"value":100,"writable":true},
                        {"type":1,"min":32,"max":2000000000i64,"default":10000,"value":10000,"writable":true},
                        {"type":5,"min":0,"max":100,"default":10,"value":10,"writable":true},
                        {"type":6,"min":40,"max":100,"default":40,"value":40,"writable":true},
                        {"type":8,"min":-500,"max":1000,"default":-100,"value":-100,"writable":false},
                        {"type":15,"min":0,"max":100,"default":30,"value":30,"writable":false},
                        {"type":16,"min":-40,"max":30,"default":-10,"value":-10,"writable":true},
                        {"type":17,"min":0,"max":1,"default":1,"value":1,"writable":true}]})
                };
                let supported = v["info"]["color"] == true
                    && v["info"]["bayer"].as_u64().is_some_and(|b| b <= 3)
                    && (self.sdk.is_none()
                        || [3, 4].iter().all(|kind| {
                            v["controls"].as_array().is_some_and(|caps| {
                                caps.iter().any(|c| {
                                    c["type"] == *kind
                                        && c["writable"] == true
                                        && c["min"].as_i64().is_some_and(|n| n <= 50)
                                        && c["max"].as_i64().is_some_and(|n| n >= 50)
                                })
                            })
                        }));
                v["whiteBalance"] = WhiteBalance::capabilities(supported);
                v["captureModes"] = json!(["still", "video"]);
                self.camera = v.clone();
                self.white_balance = None;
                self.opened = true;
                v
            }
            "white-balance" => {
                ensure!(self.opened, "not open");
                if !p.is_null() && p.as_object().is_none_or(|o| !o.is_empty()) {
                    ensure!(
                        self.exposure.is_none(),
                        "cannot change white balance during capture"
                    );
                    ensure!(
                        self.camera["whiteBalance"]["supported"] == true,
                        "managed white balance unavailable"
                    );
                    let settings: WhiteBalanceSettings = serde_json::from_value(p)?;
                    let mut next = self.white_balance.clone().unwrap_or_default();
                    next.configure(settings)?;
                    self.require_unflipped()?;
                    self.white_balance = Some(next);
                }
                json!({"capabilities":self.camera["whiteBalance"],
                    "managed":self.white_balance.is_some(),
                    "settings":self.white_balance.as_ref().map(WhiteBalance::settings)})
            }
            "get" => {
                ensure!(self.opened, "not open");
                let c = p["control"].as_i64().unwrap_or(-1) as i32;
                json!(if let Some(s) = &self.sdk {
                    s.get(c)?
                } else {
                    self.values[c.to_string()].as_i64().unwrap_or(match c {
                        8 => -100,
                        15 => 30,
                        _ => 0,
                    })
                })
            }
            "get-control-state" | "set-control-state" => {
                ensure!(self.opened, "not open");
                let c = i32::try_from(
                    p["control"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("control missing"))?,
                )?;
                if method == "set-control-state" {
                    ensure!(
                        self.white_balance.is_none() || !matches!(c, 3 | 4 | 9),
                        "WB and flip controls are owned by managed white balance; close to return to legacy controls"
                    );
                    let value = p["value"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("value missing"))?;
                    let auto = p["auto"]
                        .as_bool()
                        .ok_or_else(|| anyhow::anyhow!("auto missing"))?;
                    if let Some(s) = &self.sdk {
                        s.set_control_state(c, value, auto)?;
                    } else {
                        self.values[c.to_string()] = json!(value);
                        self.values[format!("auto:{c}")] = json!(auto);
                    }
                }
                let (value, auto) = if let Some(s) = &self.sdk {
                    s.control_state(c)?
                } else {
                    (
                        self.values[c.to_string()].as_i64().unwrap_or(0),
                        self.values[format!("auto:{c}")].as_bool().unwrap_or(false),
                    )
                };
                json!({"value":value,"auto":auto})
            }
            "set" => {
                ensure!(self.opened, "not open");
                let c = p["control"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("control missing"))?
                    as i32;
                let v = p["value"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("value missing"))?;
                ensure!(
                    self.white_balance.is_none() || !matches!(c, 3 | 4 | 9),
                    "WB and flip controls are owned by managed white balance; close to return to legacy controls"
                );
                if let Some(s) = &self.sdk {
                    s.set(c, v)?;
                    self.values[c.to_string()] = json!(v);
                } else {
                    let minimum = self.values[format!("clampMinimum:{c}")].as_i64();
                    self.values[c.to_string()] = json!(minimum.map_or(v, |m| v.max(m)));
                }
                json!(null)
            }
            "validate" | "start" => {
                ensure!(self.opened, "not open");
                ensure!(
                    method == "validate" || self.exposure.is_none(),
                    "exposure pending"
                );
                let mode = p.get("mode").map_or(Some("still"), Value::as_str);
                ensure!(
                    matches!(mode, Some("still" | "video")),
                    "unknown capture mode"
                );
                let video = mode == Some("video");
                let e: Exposure = serde_json::from_value(p.clone())?;
                e.size()?;
                for (key, control) in [("microseconds", 1), ("gain", 0)] {
                    if let Some(value) = p.get(key) {
                        let value = value
                            .as_i64()
                            .ok_or_else(|| anyhow::anyhow!("invalid {key}"))?;
                        let cap = self.camera["controls"]
                            .as_array()
                            .and_then(|caps| caps.iter().find(|c| c["type"] == control))
                            .ok_or_else(|| anyhow::anyhow!("missing {key} capability"))?;
                        ensure!(
                            cap["min"].as_i64().is_some_and(|min| value >= min)
                                && cap["max"].as_i64().is_some_and(|max| value <= max),
                            "{key} outside camera limits"
                        );
                    }
                }
                ensure!(
                    !video || !e.dark,
                    "video does not support shutter dark exposures"
                );
                if method == "validate" {
                    return Ok((Value::Null, bytes));
                }
                let reuse = video
                    && self.video.as_ref().is_some_and(|old| {
                        old == &e || (p["continuousDrain"] == true && old.same_geometry(&e))
                    });
                if let Some(gain) = p["gain"].as_i64()
                    && self.values["0"].as_i64() != Some(gain)
                {
                    if let Some(s) = &self.sdk {
                        s.set(0, gain)?;
                    }
                    self.values["0"] = json!(gain);
                }
                if reuse && self.video.as_ref() != Some(&e) {
                    if let Some(s) = &self.sdk {
                        s.set(1, e.microseconds)?;
                    }
                    self.video = Some(e.clone());
                    self.video_progress = Some(Instant::now());
                }
                if !reuse {
                    self.stop_video()?;
                }
                if !reuse && self.white_balance.is_some() {
                    WhiteBalance::validate_geometry(
                        self.camera["info"]["color"] == true,
                        self.camera["info"]["bayer"].as_u64(),
                        u32::try_from(e.bin)?,
                    )
                    .map_err(|error| regain_core::invalid(error.to_string()))?;
                    self.require_unflipped()?;
                    if let Some(s) = &mut self.sdk {
                        s.neutral_white_balance()?;
                    }
                }
                if !reuse {
                    if let Some(s) = &self.sdk {
                        if video {
                            s.start_video(&e)?;
                        } else {
                            s.start(&e)?;
                        }
                    }
                    if video {
                        self.video = Some(e.clone());
                        self.video_progress = Some(Instant::now());
                    }
                }
                self.started = Some(Instant::now());
                self.exposure = Some(e);
                json!(null)
            }
            "status" => {
                if let Some(e) = &self.video {
                    if self.exposure.is_none() {
                        return Ok((json!(0), bytes));
                    }
                    if !self.video_ready {
                        if self.video_retry_at.is_some_and(|t| Instant::now() < t) {
                            return Ok((json!(1), bytes));
                        }
                        self.video_pixels.resize(e.size()?, 0);
                        self.video_ready = if let Some(s) = &self.sdk {
                            s.video_frame(&mut self.video_pixels, e.microseconds)?
                        } else {
                            self.sim_instant
                                || self.started.is_some_and(|t| {
                                    t.elapsed() >= Duration::from_micros(e.microseconds as u64)
                                })
                        };
                        if self.video_ready {
                            self.video_progress = Some(Instant::now());
                            self.video_retry_at = None;
                        } else {
                            if self.sdk.is_some() {
                                self.video_timeouts += 1;
                                self.video_retry_at =
                                    Some(Instant::now() + Duration::from_millis(100));
                            }
                            ensure!(
                                self.video_progress.is_some_and(|t| t.elapsed()
                                    < Duration::from_micros(e.microseconds as u64)
                                        .saturating_mul(2)
                                        + Duration::from_secs(30)),
                                "SDK video produced no frame within its exposure/readout deadline"
                            );
                        }
                    }
                    json!(if self.video_ready { 2 } else { 1 })
                } else if let Some(s) = &self.sdk {
                    json!(s.status()?)
                } else if self.exposure.is_some()
                    && self.values["failedStatuses"].as_u64().unwrap_or(0) > 0
                {
                    self.values["failedStatuses"] =
                        json!(self.values["failedStatuses"].as_u64().unwrap() - 1);
                    json!(3)
                } else {
                    json!(match (&self.exposure, self.started) {
                        (Some(e), Some(t))
                            if !self.sim_instant
                                && t.elapsed() < Duration::from_micros(e.microseconds as u64) =>
                            1,
                        (Some(_), _) => 2,
                        _ => 0,
                    })
                }
            }
            "download" => {
                let e = self
                    .exposure
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no exposure"))?;
                if self.sdk.is_none() {
                    match std::mem::take(&mut self.fault).as_str() {
                        "download" => {
                            return Err(library::SdkError {
                                code: 11,
                                operation: "download".into(),
                            }
                            .into());
                        }
                        "invalid" => {
                            return Err(library::SdkError {
                                code: 8,
                                operation: "download".into(),
                            }
                            .into());
                        }
                        "crash" => std::process::exit(70),
                        "hang" => loop {
                            std::thread::park();
                        },
                        _ => {}
                    }
                }
                if self.video.is_some() {
                    ensure!(self.video_ready, "video frame not ready");
                    bytes = std::mem::take(&mut self.video_pixels);
                    self.video_ready = false;
                } else {
                    bytes.resize(e.size()?, 0);
                }
                if self.video.is_some() {
                    // Already drained on the same owner thread by status.
                } else if let Some(s) = &self.sdk {
                    s.download(&mut bytes)?;
                }
                if self.sdk.is_none() {
                    for (i, pixel) in bytes.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                        pixel.copy_from_slice(&(i as u16).to_le_bytes());
                    }
                }
                let mut v = json!({"width":e.width,"height":e.height,"bytes":bytes.len()});
                if self.video.is_some() {
                    v["videoTimeouts"] = json!(self.video_timeouts);
                }
                if let Some(wb) = &mut self.white_balance {
                    v["whiteBalance"] = wb.process(
                        &mut bytes,
                        Geometry {
                            width: e.width as usize,
                            height: e.height as usize,
                            x: e.x as u32,
                            y: e.y as u32,
                            bayer: self.camera["info"]["bayer"].as_u64().unwrap() as u8,
                            dark: e.dark,
                        },
                    )?;
                }
                self.exposure = None;
                v
            }
            "stop" => {
                if self.video.is_some() {
                    self.stop_video()?;
                } else if let Some(s) = &self.sdk {
                    s.stop()?;
                }
                self.exposure = None;
                json!(null)
            }
            "close" => {
                self.stop_video()?;
                if let Some(s) = &mut self.sdk {
                    s.close()?;
                }
                self.opened = false;
                self.exposure = None;
                self.white_balance = None;
                self.camera = Value::Null;
                json!(null)
            }
            "fault" if self.sdk.is_none() => {
                self.fault = p["kind"].as_str().unwrap_or("").into();
                json!(null)
            }
            "simulation" if self.sdk.is_none() => {
                if let Some(count) = p["failedStatuses"].as_u64() {
                    ensure!(count <= 2, "at most two simulated failed exposure statuses");
                    self.values["failedStatuses"] = json!(count);
                }
                if let Some(fault) = p["fault"].as_str() {
                    self.fault = fault.into();
                }
                if let Some(name) = p["name"].as_str() {
                    self.sim_info["name"] = json!(name);
                }
                if let Some(serial) = p["serial"].as_str() {
                    self.values["simSerial"] = json!(serial);
                }
                if p["bins"].is_array() {
                    self.sim_info["bins"] = p["bins"].clone();
                }
                if let Some(cooled) = p["cooled"].as_bool() {
                    self.sim_info["cooled"] = json!(cooled);
                }
                for key in ["color", "bayer"] {
                    if !p[key].is_null() {
                        self.sim_info[key] = p[key].clone();
                    }
                }

                if let (Some(control), Some(minimum)) =
                    (p["clampControl"].as_i64(), p["clampMinimum"].as_i64())
                {
                    self.values[format!("clampMinimum:{control}")] = json!(minimum);
                }
                if let Some(instant) = p["instant"].as_bool() {
                    self.sim_instant = instant;
                }
                if let Some(width) = p["width"].as_i64() {
                    self.sim_info["width"] = json!(width);
                }
                if let Some(height) = p["height"].as_i64() {
                    self.sim_info["height"] = json!(height);
                }
                if let Some(t) = p["temperature"].as_i64() {
                    self.values["8"] = json!(t);
                }
                if let Some(power) = p["coolerPower"].as_i64() {
                    self.values["15"] = json!(power);
                }
                json!(null)
            }
            _ => bail!("unknown method {method}"),
        };
        Ok((value, bytes))
    }
}
pub fn run(args: Vec<String>) -> Result<()> {
    if args.iter().any(|arg| arg == "--help") {
        cli::help();
        return Ok(());
    }
    let command = cli::Options::parse(&args)?;
    if let Some(options) = &command {
        options.arm_watchdog();
    }
    let sdk = if args.iter().any(|v| v == "--simulate") {
        None
    } else {
        let path = args
            .windows(2)
            .find(|v| v[0] == "--sdk")
            .map(|v| std::path::PathBuf::from(&v[1]))
            .unwrap_or(std::env::current_exe()?.with_file_name(library::LIBRARY_NAME));
        Some(library::Sdk::load(&path)?)
    };
    let mut host = Host {
        sdk,
        exposure: None,
        started: None,
        values: json!({}),
        fault: String::new(),
        opened: false,
        sim_instant: false,
        camera: Value::Null,
        white_balance: None,
        video: None,
        video_pixels: Vec::new(),
        video_ready: false,
        video_progress: None,
        video_retry_at: None,
        video_timeouts: 0,
        sim_info: json!({"id":0,"name":"ZWO Simulated","width":960,"height":640,"color":true,"bayer":0,"pixelSize":3.76,"bitDepth":16,"cooled":true,"shutter":false,"bins":[1,2,4],"formats":[0,2]}),
    };
    if let Some(options) = command {
        return cli::run(&mut host, options);
    }
    crate::asi::continuous::serve(
        |method, params| host.command(method, params),
        |e| {
            let sdk_error = e.downcast_ref::<library::SdkError>();
            let code = sdk_error.map(|s| s.code).or_else(|| {
                matches!(
                    e.downcast_ref::<regain_core::Failure>(),
                    Some(regain_core::Failure::Invalid(_))
                )
                .then_some(8)
            });
            json!({"sdkCode":code,"sdkOperation":sdk_error.map(|s|&s.operation)})
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_discovery_is_rejected_until_camera_is_closed() {
        let mut host = Host {
            sdk: None,
            exposure: None,
            started: None,
            values: json!({}),
            fault: String::new(),
            opened: false,
            sim_info: json!({"name":"test camera"}),
            sim_instant: true,
            camera: Value::Null,
            white_balance: None,
            video: None,
            video_pixels: Vec::new(),
            video_ready: false,
            video_progress: None,
            video_retry_at: None,
            video_timeouts: 0,
        };
        assert!(host.command("list", Value::Null).is_ok());
        assert_eq!(
            host.command("list", json!({"serials":true})).unwrap().0[0]["serial"],
            "sim00001"
        );
        for serial in [Value::Null, json!(""), json!("  "), json!(" SIM00001 ")] {
            host.command("open", json!({"name":"test camera","serial":serial}))
                .unwrap();
            host.command("close", Value::Null).unwrap();
        }
        host.command("open", json!({"name":"test camera"})).unwrap();
        assert!(
            host.command("list", Value::Null)
                .unwrap_err()
                .to_string()
                .contains("cached camera list")
        );
        assert!(host.command("open", json!({"name":"test camera"})).is_err());
        host.command(
            "start",
            json!({"width":128,"height":128,"bin":1,"x":0,"y":0,"microseconds":1000,"dark":true}),
        )
        .unwrap();
        assert!(host.command("list", Value::Null).is_err());
        host.command("download", Value::Null).unwrap();
        assert!(host.command("list", Value::Null).is_err());
        host.command("close", Value::Null).unwrap();
        assert!(host.command("list", Value::Null).is_ok());
    }
    #[test]
    fn frame_bounds() {
        let mut e = Exposure {
            width: 9576,
            height: 6388,
            bin: 1,
            x: 0,
            y: 0,
            microseconds: 1000,
            dark: false,
        };
        assert_eq!(e.size().unwrap(), 9576 * 6388 * 2);
        e.width = i32::MAX;
        assert!(e.size().is_err());
        e.width = 0;
        assert!(e.size().is_err());
    }
}
