#[allow(dead_code)]
mod raw;
mod sdk;
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

const MAX_JSON: usize = 65536;
const MAX_FRAME: usize = 512 * 1024 * 1024;
#[derive(Clone, Deserialize)]
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
    sdk: Option<sdk::Sdk>,
    exposure: Option<Exposure>,
    started: Option<Instant>,
    values: Value,
    fault: String,
    opened: bool,
    sim_info: Value,
    sim_instant: bool,
}
impl Host {
    fn command(&mut self, method: &str, p: Value) -> Result<(Value, Vec<u8>)> {
        let mut bytes = Vec::new();
        let value = match method {
            "list" => {
                if let Some(s) = &self.sdk {
                    s.list()?
                } else {
                    json!([self.sim_info])
                }
            }
            "open" => {
                let name = p["name"].as_str().unwrap_or("");
                let v = if let Some(s) = &mut self.sdk {
                    s.open(name, p["serial"].as_str())?
                } else {
                    ensure!(
                        name == self.sim_info["name"]
                            && (p["serial"].is_null()
                                || p["serial"]
                                    == self.values["simSerial"].as_str().unwrap_or("sim00001")),
                        "wrong simulated identity"
                    );
                    let info = self.command("list", json!({}))?.0[0].clone();
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
                self.opened = true;
                v
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
            "set" => {
                ensure!(self.opened, "not open");
                let c = p["control"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("control missing"))?
                    as i32;
                let v = p["value"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("value missing"))?;
                if let Some(s) = &self.sdk {
                    s.set(c, v)?;
                } else {
                    let minimum = self.values[format!("clampMinimum:{c}")].as_i64();
                    self.values[c.to_string()] = json!(minimum.map_or(v, |m| v.max(m)));
                }
                json!(null)
            }
            "start" => {
                ensure!(self.opened, "not open");
                ensure!(self.exposure.is_none(), "exposure pending");
                let e: Exposure = serde_json::from_value(p)?;
                e.size()?;
                if let Some(s) = &self.sdk {
                    s.start(&e)?;
                }
                self.started = Some(Instant::now());
                self.exposure = Some(e);
                json!(null)
            }
            "status" => {
                if let Some(s) = &self.sdk {
                    json!(s.status()?)
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
                            return Err(sdk::SdkError {
                                code: 11,
                                operation: "download".into(),
                            }
                            .into());
                        }
                        "invalid" => {
                            return Err(sdk::SdkError {
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
                bytes.resize(e.size()?, 0);
                if let Some(s) = &self.sdk {
                    s.download(&mut bytes)?;
                } else {
                    for (i, pixel) in bytes.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                        pixel.copy_from_slice(&(i as u16).to_le_bytes());
                    }
                }
                let v = json!({"width":e.width,"height":e.height,"bytes":bytes.len()});
                self.exposure = None;
                v
            }
            "stop" => {
                if let Some(s) = &self.sdk {
                    s.stop()?;
                }
                self.exposure = None;
                json!(null)
            }
            "close" => {
                if let Some(s) = &mut self.sdk {
                    s.close()?;
                }
                self.opened = false;
                self.exposure = None;
                json!(null)
            }
            "fault" if self.sdk.is_none() => {
                self.fault = p["kind"].as_str().unwrap_or("").into();
                json!(null)
            }
            "simulation" if self.sdk.is_none() => {
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
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let sdk = if args.iter().any(|v| v == "--simulate") {
        None
    } else {
        let path = args
            .windows(2)
            .find(|v| v[0] == "--sdk")
            .map(|v| std::path::PathBuf::from(&v[1]))
            .unwrap_or(std::env::current_exe()?.with_file_name("ASICamera2.dll"));
        Some(sdk::Sdk::load(&path)?)
    };
    let mut host = Host {
        sdk,
        exposure: None,
        started: None,
        values: json!({}),
        fault: String::new(),
        opened: false,
        sim_instant: false,
        sim_info: json!({"id":0,"name":"ZWO Simulated","width":960,"height":640,"color":true,"bayer":0,"pixelSize":3.76,"bitDepth":16,"cooled":true,"shutter":false,"bins":[1,2,4],"formats":[0,2]}),
    };
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    loop {
        let mut header = [0u8; 4];
        match input.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(header) as usize;
        ensure!(len > 0 && len <= MAX_JSON, "invalid request length");
        let mut data = vec![0; len];
        input.read_exact(&mut data)?;
        let req: Value = serde_json::from_slice(&data)?;
        ensure!(req["version"] == 1, "unsupported protocol");
        let (reply, frame) = match host
            .command(req["method"].as_str().unwrap_or(""), req["params"].clone())
        {
            Ok((v, b)) => (
                json!({"version":1,"id":req["id"],"ok":true,"result":v,"binaryLength":b.len()}),
                b,
            ),
            Err(e) => {
                eprintln!("{e:#}");
                let sdk_error = e.downcast_ref::<sdk::SdkError>();
                (
                    json!({"version":1,"id":req["id"],"ok":false,"error":format!("{e:#}"),"sdkCode":sdk_error.map(|s|s.code),"sdkOperation":sdk_error.map(|s|&s.operation),"binaryLength":0}),
                    Vec::new(),
                )
            }
        };
        let data = serde_json::to_vec(&reply)?;
        ensure!(data.len() <= MAX_JSON, "reply too large");
        output.write_all(&(data.len() as u32).to_le_bytes())?;
        output.write_all(&data)?;
        output.write_all(&frame)?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
