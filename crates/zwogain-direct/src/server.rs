//! Version-1 plugin protocol over inherited pipes. Only the verified ASI676 path.
//! A dedicated worker owns the exclusive driver handle for the entire connection.
use crate::{asi676, settings::Settings, transport};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

type Frame = (Value, Vec<u8>);
type Work = (Settings, mpsc::SyncSender<Result<Frame>>);
const NAME: &str = "ZWO ASI676MC";

fn descriptor() -> Value {
    json!({"id":0,"name":NAME,"width":3552,"height":3552,"color":true,"bayer":0,
        "pixelSize":2.0,"bitDepth":12,"cooled":false,"shutter":false,"bins":[1],"formats":[2],
        "minimumWidth":64,"minimumHeight":64,"originAlignment":2})
}

fn paths() -> Result<Vec<Vec<u16>>> {
    Ok(transport::enumerate()?
        .into_iter()
        .filter(|path| {
            String::from_utf16_lossy(path)
                .to_ascii_lowercase()
                .contains("vid_03c3&pid_676d")
        })
        .collect())
}

#[derive(Debug)]
struct HardwareFailure(String);
impl std::fmt::Display for HardwareFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HardwareFailure {}
fn hardware(error: anyhow::Error) -> anyhow::Error {
    HardwareFailure(format!("{error:#}")).into()
}

fn open_camera(serial: Option<&str>) -> Result<(transport::Camera, Value, String)> {
    let paths = paths()?;
    ensure!(!paths.is_empty(), "ASI676MC is not attached");
    ensure!(
        serial.is_some() || paths.len() == 1,
        "multiple ASI676MC cameras require a serial number"
    );
    for path in paths {
        let camera = transport::Camera::open(&path)?;
        let info = camera.probe()?;
        ensure!(
            info["productId"] == 0x676d && info["usbVersionBcd"] == 0x300,
            "experimental capture requires ASI676MC USB3"
        );
        // SDK 1.41 ASIGetSerialNumber uses vendor IN C8, value/index 0, eight bytes.
        let bytes = camera.vendor(0xc8, 0, 0, 8)?;
        ensure!(
            bytes.iter().any(|&v| v != 0),
            "camera serial is unavailable"
        );
        let found: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
        if serial.is_none_or(|s| s == found) {
            return Ok((camera, info, found));
        }
    }
    bail!("selected ASI676MC serial is not attached")
}

struct Worker {
    sender: mpsc::Sender<Work>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    fn open(serial: Option<String>, simulate: bool) -> Result<(Self, String)> {
        let (sender, receiver) = mpsc::channel::<Work>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let device = if simulate {
                None
            } else {
                match open_camera(serial.as_deref()) {
                    Ok(device) => Some(device),
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                }
            };
            let identity = device
                .as_ref()
                .map(|d| d.2.clone())
                .unwrap_or("direct-simulator".into());
            if serial.as_ref().is_some_and(|s| s != &identity) {
                let _ = ready_tx.send(Err(anyhow::anyhow!("camera identity differs")));
                return;
            }
            if ready_tx.send(Ok(identity)).is_err() {
                return;
            }
            let (watchdog, deadlines) = mpsc::channel::<Option<Duration>>();
            std::thread::spawn(move || {
                let mut timeout = None;
                loop {
                    let event = if let Some(duration) = timeout {
                        deadlines.recv_timeout(duration)
                    } else {
                        deadlines
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    match event {
                        Ok(next) => timeout = next,
                        Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(124),
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
            while let Ok((settings, reply)) = receiver.recv() {
                // A stuck kernel operation must end the process, never free live I/O buffers.
                let timeout = Duration::from_micros(u64::from(settings.microseconds))
                    + Duration::from_secs(45);
                let _ = watchdog.send(Some(timeout));
                let result = if let Some((camera, info, _)) = &device {
                    asi676::capture(camera, info, &settings, false)
                } else {
                    std::thread::sleep(Duration::from_micros(u64::from(settings.microseconds)));
                    let pixels: Vec<_> = (0..settings.width * settings.height)
                        .flat_map(|i| (i as u16).to_le_bytes())
                        .collect();
                    Ok((
                        json!({"width":settings.width,"height":settings.height,"sdkLoaded":false,
                        "simulated":true,"readoutRetriesUsed":0}),
                        pixels,
                    ))
                };
                let _ = watchdog.send(None);
                if reply.send(result).is_err() {
                    return;
                }
            }
        });
        match ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("direct worker exited during open"))?
        {
            Ok(identity) => Ok((
                Self {
                    sender,
                    thread: Some(thread),
                },
                identity,
            )),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }
    fn close(mut self) {
        drop(self.sender);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct Host {
    worker: Option<Worker>,
    pending: Option<mpsc::Receiver<Result<Frame>>>,
    frame: Option<Result<Frame>>,
    settings: Settings,
    simulate: bool,
}
impl Host {
    fn update(&mut self) {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(frame) => {
                    self.frame = Some(frame);
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.frame = Some(Err(anyhow::anyhow!("direct worker exited")));
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn command(&mut self, method: &str, params: &Value) -> Result<Frame> {
        self.update();
        let mut pixels = Vec::new();
        let number = |key: &str| -> Result<u32> {
            Ok(u32::try_from(params[key].as_u64().ok_or_else(|| {
                anyhow::anyhow!("missing or invalid {key}")
            })?)?)
        };
        let value = match method {
            "list" => {
                if self.simulate {
                    json!([descriptor()])
                } else {
                    json!(
                        paths()
                            .map_err(hardware)?
                            .iter()
                            .map(|_| descriptor())
                            .collect::<Vec<_>>()
                    )
                }
            }
            "open" => {
                ensure!(self.worker.is_none(), "camera is already open");
                ensure!(
                    params["name"] == NAME,
                    "SDK-less capture currently supports ASI676MC only"
                );
                let serial = params["serial"].as_str().map(str::to_owned);
                let (worker, identity) = Worker::open(serial, self.simulate).map_err(hardware)?;
                self.worker = Some(worker);
                self.settings = Settings::default();
                json!({"serial":identity,"info":descriptor(),"sdkVersion":"SDK-less experimental ASI676MC",
                    "backend":"direct","controls":[
                    {"type":0,"min":0,"max":600,"value":0,"writable":true},
                    {"type":1,"min":32,"max":30000000,"value":100000,"writable":true},
                    {"type":5,"min":0,"max":200,"value":10,"writable":true},
                    {"type":6,"min":40,"max":40,"value":40,"writable":false}]})
            }
            "get" | "set" => {
                ensure!(self.worker.is_some(), "camera is not open");
                let control = number("control")?;
                if method == "set" {
                    ensure!(
                        self.pending.is_none() && self.frame.is_none(),
                        "cannot change controls during capture"
                    );
                    let mut settings = self.settings.clone();
                    match control {
                        0 => settings.gain = number("value")?,
                        1 => settings.microseconds = number("value")?,
                        5 => settings.offset = number("value")?,
                        _ => bail!("control is unsupported or read-only"),
                    }
                    settings.validate()?;
                    self.settings = settings;
                    Value::Null
                } else {
                    json!(match control {
                        0 => self.settings.gain,
                        1 => self.settings.microseconds,
                        5 => self.settings.offset,
                        6 => 40,
                        _ => bail!("unsupported control"),
                    })
                }
            }
            "start" => {
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("camera is not open"))?;
                ensure!(
                    self.pending.is_none() && self.frame.is_none(),
                    "exposure pending"
                );
                ensure!(number("bin")? == 1, "SDK-less capture supports bin 1 only");
                let mut settings = self.settings.clone();
                settings.width = number("width")?;
                settings.height = number("height")?;
                settings.x = number("x")?;
                settings.y = number("y")?;
                settings.microseconds = number("microseconds")?;
                if !params["readRetries"].is_null() {
                    settings.read_retries = number("readRetries")?;
                }
                settings.validate()?;
                let (sender, receiver) = mpsc::sync_channel(1);
                worker
                    .sender
                    .send((settings, sender))
                    .map_err(|_| hardware(anyhow::anyhow!("direct worker exited")))?;
                self.pending = Some(receiver);
                Value::Null
            }
            "status" => {
                ensure!(self.worker.is_some(), "camera is not open");
                if let Some(Err(error)) = &self.frame {
                    return Err(hardware(anyhow::anyhow!("{error:#}")));
                }
                json!(if self.pending.is_some() {
                    1
                } else if self.frame.is_some() {
                    2
                } else {
                    0
                })
            }
            "download" => {
                let frame = self
                    .frame
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("no completed frame"))?
                    .map_err(hardware)?;
                pixels = frame.1;
                frame.0
            }
            "stop" | "close" => {
                ensure!(
                    self.pending.is_none(),
                    "active capture must be aborted by terminating the isolated host"
                );
                self.frame = None;
                if method == "close"
                    && let Some(worker) = self.worker.take()
                {
                    worker.close();
                }
                Value::Null
            }
            _ => bail!("unknown direct method {method}"),
        };
        Ok((value, pixels))
    }
}

pub fn run(simulate: bool) -> Result<()> {
    transport::require_sdk_absent()?;
    let mut host = Host {
        simulate,
        ..Host::default()
    };
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    loop {
        let mut length = [0_u8; 4];
        match input.read_exact(&mut length) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let size = u32::from_le_bytes(length) as usize;
        ensure!((1..=65536).contains(&size), "invalid request length");
        let mut bytes = vec![0; size];
        input.read_exact(&mut bytes)?;
        let request: Value = serde_json::from_slice(&bytes)?;
        ensure!(request["version"] == 1, "unsupported protocol");
        let began = Instant::now();
        let (reply, pixels) =
            match host.command(request["method"].as_str().unwrap_or(""), &request["params"]) {
                Ok((result, pixels)) => (
                    json!({"version":1,"id":request["id"],"ok":true,
                "result":result,"binaryLength":pixels.len()}),
                    pixels,
                ),
                Err(error) => (
                    json!({"version":1,"id":request["id"],"ok":false,"error":format!("{error:#}"),
                "sdkCode":if error.is::<HardwareFailure>() { None } else {Some(8)},
                "sdkOperation":"direct","binaryLength":0}),
                    Vec::new(),
                ),
            };
        transport::require_sdk_absent()?;
        if request["method"] == "download" {
            eprintln!(
                "ZWOgain direct frame delivered: {} bytes (IPC preparation {} ms)",
                pixels.len(),
                began.elapsed().as_millis()
            );
        }
        let json = serde_json::to_vec(&reply)?;
        output.write_all(&(json.len() as u32).to_le_bytes())?;
        output.write_all(&json)?;
        output.write_all(&pixels)?;
        output.flush()?;
    }
}
