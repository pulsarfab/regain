//! Opt-in continuous acquisition. The owner drains independently of IPC/output.
//! Scientific single-exposure commands retain their original semantics.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

type Frame = (Value, Vec<u8>);

#[derive(Default)]
struct Stream {
    params: Option<Value>,
    requested: Option<Value>,
    settling_until: Option<Instant>,
    discard_frames: u32,
    latest: Option<Frame>,
    failure: Option<String>,
    failure_details: Value,
    error_fields: Option<fn(&anyhow::Error) -> Value>,
    interval: Duration,
    delivered_at: Option<Instant>,
    acquired: u64,
    delivered: u64,
    replaced: u64,
    camera: Value,
    stopping: bool,
    generation: u64,
    configured_at: Option<Instant>,
    last_frame_at: Option<Instant>,
    reported_at: Option<Instant>,
}

impl Stream {
    fn ready(&self, now: Instant) -> bool {
        self.failure.is_none()
            && self.latest.is_some()
            && self
                .delivered_at
                .is_none_or(|t| now.duration_since(t) >= self.interval)
    }

    fn stop(&mut self, command: &mut impl FnMut(&str, Value) -> Result<Frame>) -> Result<()> {
        if self.params.is_some() {
            // Retain active/faulted state if cleanup fails: no unsafe restart.
            if let Err(error) = command("stop", Value::Null) {
                self.failure_details = self.error_fields.map_or(Value::Null, |f| f(&error));
                self.latest = None;
                self.failure = Some(format!(
                    "capture cleanup failed; host replacement required: {error:#}"
                ));
                return Err(error);
            }
        }
        self.params = None;
        self.requested = None;
        self.settling_until = None;
        self.discard_frames = 0;
        self.latest = None;
        self.delivered_at = None;
        self.failure = None;
        self.failure_details = Value::Null;
        self.stopping = false;
        Ok(())
    }

    fn command(
        &mut self,
        method: &str,
        mut p: Value,
        command: &mut impl FnMut(&str, Value) -> Result<Frame>,
    ) -> Result<Frame> {
        match method {
            "stream-start" => {
                ensure!(
                    !self.stopping,
                    "stream is stopping at the exposure boundary"
                );
                let fps = p
                    .get("maxFps")
                    .map_or(Some(1.0), Value::as_f64)
                    .ok_or_else(|| anyhow::anyhow!("invalid maxFps"))?;
                ensure!(
                    fps.is_finite() && (0.01..=120.0).contains(&fps),
                    "maxFps must be 0.01..120"
                );
                ensure!(p.is_object(), "stream parameters must be an object");
                p.as_object_mut().unwrap().remove("maxFps");
                // Automatic selection never invents a model's USB video protocol.
                if p.get("mode").is_none() {
                    let native = self.camera["captureModes"]
                        .as_array()
                        .is_some_and(|m| m.iter().any(|v| v == "video"))
                        && p["dark"] != true
                        && p["microseconds"].as_u64().is_some_and(|exposure| {
                            self.camera["videoMaxExposureMicroseconds"]
                                .as_u64()
                                .is_none_or(|limit| exposure <= limit)
                        });
                    p["mode"] = json!(if native { "video" } else { "still" });
                }
                p["continuousDrain"] = json!(true);
                ensure!(
                    self.failure.is_none(),
                    "stream faulted; stop before restarting"
                );
                if self.params.as_ref() != Some(&p) {
                    command("validate", p.clone())?;
                    if self.params.is_some() {
                        // The acquisition owner applies edits only after draining
                        // the in-flight frame, never midway through a Direct trigger.
                        self.requested = Some(p);
                        self.latest = None;
                        self.interval = Duration::from_secs_f64(1.0 / fps);
                        return Ok((self.status(), Vec::new()));
                    }
                    let began = Instant::now();
                    self.stop(command)?;
                    let stopped_ms = began.elapsed().as_millis();
                    command("start", p.clone())?;
                    diagnostic(
                        "stream.configured",
                        json!({
                            "mode":p["mode"],"exposureMicroseconds":p["microseconds"],
                            "width":p["width"],"height":p["height"],"bin":p["bin"],
                            "stopMilliseconds":stopped_ms,"totalMilliseconds":began.elapsed().as_millis(),
                            "settingsGeneration":self.generation + 1,"settingsChange":"restart-boundary"
                        }),
                    );
                    self.params = Some(p);
                    self.acquired = 0;
                    self.delivered = 0;
                    self.replaced = 0;
                    self.generation += 1;
                    self.configured_at = Some(Instant::now());
                    self.last_frame_at = None;
                    self.reported_at = self.configured_at;
                    // Keep delivery cadence across settings changes.
                } else {
                    self.requested = None;
                }
                self.interval = Duration::from_secs_f64(1.0 / fps);
                Ok((self.status(), Vec::new()))
            }
            "stream-status" => Ok((self.status(), Vec::new())),
            "stream-download" => {
                if let Some(error) = &self.failure {
                    bail!("continuous acquisition failed: {error}");
                }
                ensure!(
                    self.ready(Instant::now()),
                    "continuous frame not ready or delivery FPS limited"
                );
                let (mut metadata, bytes) = self.latest.take().unwrap();
                self.delivered += 1;
                self.delivered_at = Some(Instant::now());
                metadata["continuous"] = self.status();
                Ok((metadata, bytes))
            }
            "stream-stop" => {
                self.requested = None;
                if self.failure.is_none()
                    && self.params.as_ref().is_some_and(|p| p["mode"] == "still")
                {
                    // These direct protocols cannot abort integration safely. Drain
                    // the in-flight exposure once, then do not start another.
                    self.stopping = true;
                    self.latest = None;
                    return Ok((self.status(), Vec::new()));
                }
                self.stop(command)?;
                Ok((self.status(), Vec::new()))
            }
            "stop" | "close" if self.params.is_some() => {
                self.stop(command)?;
                if method == "close" {
                    command(method, p)
                } else {
                    Ok((Value::Null, Vec::new()))
                }
            }
            _ => {
                ensure!(
                    self.params.is_none(),
                    "continuous acquisition active; use stream commands or stop before changing controls"
                );
                let (mut v, bytes) = command(method, p)?;
                if method == "open" {
                    self.delivered_at = None;
                    self.generation = 0;
                    v["continuousAcquisition"] = json!({"supported":true,"buffer":"latest-only",
                        "fpsScope":"delivery","settingsChange":"scalar-boundary-structural-restart"});
                    self.camera = v.clone();
                }
                Ok((v, bytes))
            }
        }
    }

    fn status(&self) -> Value {
        json!({"active":self.params.is_some(), "ready":self.ready(Instant::now()),
            "error":self.failure,"errorDetails":self.failure_details,"acquiredFrames":self.acquired,"deliveredFrames":self.delivered,
            "replacedFrames":self.replaced,"mode":self.params.as_ref().map(|p| &p["mode"]),
            "fpsScope":"delivery", "stopping":self.stopping,"settingsGeneration":self.generation,
            "settingsPending":self.requested.is_some(),
            "settling":self.discard_frames > 0 || self.settling_until.is_some_and(|t| Instant::now() < t),
            "lastFrameAgeMilliseconds":self.last_frame_at.map(|t| t.elapsed().as_millis()),
            "exposureMicroseconds":self.params.as_ref().map(|p| &p["microseconds"])})
    }

    fn tick(&mut self, command: &mut impl FnMut(&str, Value) -> Result<Frame>) {
        if self.failure.is_some() {
            return;
        }
        let Some(params) = self.params.clone() else {
            return;
        };
        let result = (|| -> Result<()> {
            let state = command("status", Value::Null)?.0;
            match state.as_i64() {
                Some(1) => return Ok(()),
                Some(2) => (),
                _ => bail!("unexpected capture state {state}"),
            }
            let mut frame = command("download", Value::Null)?;
            if self.stopping {
                command("stop", Value::Null)?;
                self.params = None;
                self.stopping = false;
                return Ok(());
            }
            if let Some(next) = self.requested.take() {
                let live = scalar_update(&params, &next);
                if !live {
                    command("stop", Value::Null)?;
                }
                command("start", next.clone())?;
                self.latest = None;
                self.generation += 1;
                self.configured_at = Some(Instant::now());
                // A successful register write does not identify the first new
                // sensor frame. Drain transition data for old+new integration
                // and at least two reads; never label buffered data as settled.
                self.discard_frames = if live { 2 } else { 0 };
                self.settling_until = live.then(|| {
                    Instant::now()
                        + Duration::from_micros(
                            params["microseconds"]
                                .as_u64()
                                .unwrap_or(0)
                                .saturating_add(next["microseconds"].as_u64().unwrap_or(0)),
                        )
                });
                diagnostic(
                    "stream.settings_applied",
                    json!({"settingsGeneration":self.generation,
                    "settingsChange":if live {"scalar-boundary"} else {"restart-boundary"},
                    "exposureMicroseconds":next["microseconds"],"gain":next["gain"],
                    "transitionFramesToDiscard":self.discard_frames}),
                );
                self.params = Some(next);
                return Ok(());
            }
            if self.discard_frames > 0 || self.settling_until.is_some_and(|t| Instant::now() < t) {
                self.discard_frames = self.discard_frames.saturating_sub(1);
                command("start", params)?;
                return Ok(());
            }
            self.settling_until = None;
            self.acquired += 1;
            let first_for_generation = self.last_frame_at.is_none_or(|last| {
                self.configured_at
                    .is_some_and(|configured| last < configured)
            });
            self.last_frame_at = Some(Instant::now());
            if first_for_generation {
                diagnostic(
                    "stream.first_frame",
                    json!({"settingsGeneration":self.generation,
                    "millisecondsSinceConfigured":self.configured_at.map(|t|t.elapsed().as_millis()),
                    "exposureMicroseconds":params["microseconds"],"mode":params["mode"]}),
                );
            }
            if self
                .reported_at
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(30))
            {
                diagnostic("stream.progress", self.status());
                self.reported_at = Some(Instant::now());
            }
            frame.0["acquisitionSequence"] = json!(self.acquired);
            frame.0["settingsGeneration"] = json!(self.generation);
            if self.latest.replace(frame).is_some() {
                self.replaced += 1;
            }
            command("start", params)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.failure_details = self.error_fields.map_or(Value::Null, |f| f(&error));
            // Never deliver a buffered image as success after a terminal fault.
            self.latest = None;
            self.failure = Some(format!("{error:#}"));
            let _ = writeln!(
                std::io::stderr().lock(),
                "REGAIN_DIAGNOSTIC {}",
                json!({"version":1,"level":"warning","event":"stream.failed",
                    "message":self.failure,"acquiredFrames":self.acquired})
            );
        }
    }
}

fn scalar_update(old: &Value, new: &Value) -> bool {
    if old["mode"] != "video" || new["mode"] != "video" {
        return false;
    }
    let mut prior = old.clone();
    for key in ["microseconds", "gain"] {
        if let Some(value) = new.get(key) {
            prior[key] = value.clone();
        } else if let Some(object) = prior.as_object_mut() {
            object.remove(key);
        }
    }
    prior == *new
}

fn diagnostic(event: &str, details: Value) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "REGAIN_DIAGNOSTIC {}",
        json!({"version":1,"level":"debug","event":event,"message":details.to_string()})
    );
}

/// The I/O thread never calls a camera API. Even a blocked pipe write cannot
/// stop the owner from draining. One reply and one latest image bound memory.
pub(super) fn serve(
    mut command: impl FnMut(&str, Value) -> Result<Frame>,
    error_fields: fn(&anyhow::Error) -> Value,
) -> Result<()> {
    let (requests, receiver) = mpsc::sync_channel::<Value>(1);
    let (replies, response) = mpsc::sync_channel::<Frame>(1);
    let io = std::thread::spawn(move || -> Result<()> {
        let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
        loop {
            let mut header = [0; 4];
            match input.read_exact(&mut header) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e.into()),
            }
            let len = u32::from_le_bytes(header) as usize;
            ensure!((1..=65536).contains(&len), "invalid request length");
            let mut data = vec![0; len];
            input.read_exact(&mut data)?;
            let request: Value = serde_json::from_slice(&data)?;
            ensure!(request["version"] == 1, "unsupported protocol");
            requests.send(request)?;
            let (reply, pixels) = response.recv()?;
            let data = serde_json::to_vec(&reply)?;
            ensure!(data.len() <= 65536, "reply too large");
            output.write_all(&(data.len() as u32).to_le_bytes())?;
            output.write_all(&data)?;
            output.write_all(&pixels)?;
            output.flush()?;
        }
    });
    let mut stream = Stream {
        error_fields: Some(error_fields),
        ..Stream::default()
    };
    loop {
        let request = if stream.params.is_some() && stream.failure.is_none() {
            receiver.recv_timeout(Duration::from_millis(1))
        } else {
            receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match request {
            Ok(req) => {
                let method = req["method"].as_str().unwrap_or("");
                let (reply, bytes) =
                    match stream.command(method, req["params"].clone(), &mut command) {
                        Ok((v, bytes)) => (
                            json!({"version":1,"id":req["id"],"ok":true,
                        "result":v,"binaryLength":bytes.len()}),
                            bytes,
                        ),
                        Err(error) => {
                            let _ = writeln!(
                                std::io::stderr().lock(),
                                "REGAIN_DIAGNOSTIC {}",
                                json!({"version":1,"level":"warning","event":"command.failed",
                                "message":format!("{method} request {}: {error:#}", req["id"])})
                            );
                            let mut reply = error_fields(&error);
                            reply["version"] = json!(1);
                            reply["id"] = req["id"].clone();
                            reply["ok"] = json!(false);
                            reply["error"] = json!(format!("{error:#}"));
                            reply["binaryLength"] = json!(0);
                            (reply, Vec::new())
                        }
                    };
                if replies.send((reply, bytes)).is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        stream.tick(&mut command);
    }
    let stopped = stream.stop(&mut command);
    let closed = command("close", Value::Null);
    let io_result = io
        .join()
        .map_err(|_| anyhow::anyhow!("protocol I/O thread panicked"))?;
    stopped?;
    closed?;
    io_result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn backend(method: &str, p: Value) -> Result<Frame> {
        Ok((
            match method {
                "open" => {
                    json!({"captureModes":["still","video"],"videoMaxExposureMicroseconds":30_000_000})
                }
                "status" => json!(2),
                "download" => return Ok((json!({"width":64}), vec![42; 8])),
                "start" => {
                    assert_eq!(p["continuousDrain"], true);
                    Value::Null
                }
                _ => Value::Null,
            },
            Vec::new(),
        ))
    }
    fn start(s: &mut Stream, p: Value) {
        s.command("open", Value::Null, &mut backend).unwrap();
        s.command("stream-start", p, &mut backend).unwrap();
    }
    #[test]
    fn drains_and_retains_only_latest_even_below_one_fps() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000,"maxFps":0.1}));
        for _ in 0..200 {
            s.tick(&mut backend);
        }
        assert_eq!((s.acquired, s.replaced), (200, 199));
        let frame = s
            .command("stream-download", Value::Null, &mut backend)
            .unwrap();
        assert_eq!(frame.0["acquisitionSequence"], 200);
        assert_eq!(frame.1, vec![42; 8]);
        for _ in 0..200 {
            s.tick(&mut backend);
        }
        assert_eq!(s.acquired, 400);
        assert!(!s.ready(Instant::now()));
        assert!(
            s.command("stream-download", Value::Null, &mut backend)
                .is_err()
        );
        assert!(s.ready(s.delivered_at.unwrap() + Duration::from_secs(10)));
    }
    #[test]
    fn fps_only_changes_do_not_reconfigure_but_settings_changes_discard_old_frame() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000}));
        s.tick(&mut backend);
        s.command(
            "stream-start",
            json!({"microseconds":1000,"maxFps":0.5}),
            &mut backend,
        )
        .unwrap();
        assert_eq!(s.generation, 1);
        assert!(s.latest.is_some());
        s.command("stream-start", json!({"microseconds":2000}), &mut backend)
            .unwrap();
        assert_eq!(s.generation, 1);
        assert!(s.latest.is_none());
        s.tick(&mut backend);
        assert_eq!(s.generation, 2);
        assert!(s.latest.is_none());
        s.settling_until = Some(Instant::now());
        s.tick(&mut backend);
        s.tick(&mut backend);
        assert!(s.latest.is_none());
        s.tick(&mut backend);
        assert_eq!(s.latest.unwrap().0["settingsGeneration"], 2);
    }
    #[test]
    fn explicit_stop_resets_delivery_cadence_for_a_new_session() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000,"maxFps":0.01}));
        s.tick(&mut backend);
        s.command("stream-download", Value::Null, &mut backend)
            .unwrap();
        s.command("stream-stop", Value::Null, &mut backend).unwrap();
        s.command(
            "stream-start",
            json!({"microseconds":1000,"maxFps":0.01}),
            &mut backend,
        )
        .unwrap();
        s.tick(&mut backend);
        assert!(s.ready(Instant::now()));
    }
    #[test]
    fn scalar_changes_are_coalesced_at_boundary_without_restart() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000,"gain":100}));
        let mut calls = Vec::new();
        let mut observed = |method: &str, p: Value| {
            calls.push((method.to_string(), p.clone()));
            backend(method, p)
        };
        for exposure in [2000, 3000] {
            s.command(
                "stream-start",
                json!({"microseconds":exposure,"gain":200}),
                &mut observed,
            )
            .unwrap();
        }
        s.tick(&mut observed);
        assert_eq!(s.params.as_ref().unwrap()["microseconds"], 3000);
        assert_eq!(s.params.as_ref().unwrap()["gain"], 200);
        assert_eq!(calls.iter().filter(|(m, _)| m == "start").count(), 1);
        assert!(!calls.iter().any(|(m, _)| m == "stop"));
        assert_eq!(s.discard_frames, 2);
    }
    #[test]
    fn structural_or_still_changes_restart_only_after_draining() {
        for mode in ["video", "still"] {
            let mut s = Stream::default();
            start(&mut s, json!({"microseconds":1000,"width":64,"mode":mode}));
            let mut calls = Vec::new();
            let mut observed = |method: &str, p: Value| {
                calls.push(method.to_string());
                backend(method, p)
            };
            s.command(
                "stream-start",
                json!({"microseconds":2000,"width":128,"mode":mode}),
                &mut observed,
            )
            .unwrap();
            s.tick(&mut observed);
            assert_eq!(calls, ["validate", "status", "download", "stop", "start"]);
            assert!(s.latest.is_none());
        }
    }
    #[test]
    fn rejected_edit_preserves_active_stream_and_failed_apply_latches_fault() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000}));
        assert!(
            s.command(
                "stream-start",
                json!({"microseconds":0}),
                &mut |_, _| bail!("invalid")
            )
            .is_err()
        );
        assert!(s.requested.is_none());
        assert!(s.failure.is_none());
        s.command("stream-start", json!({"microseconds":2000}), &mut backend)
            .unwrap();
        s.tick(&mut |method, p| {
            if method == "start" {
                bail!("write failed")
            } else {
                backend(method, p)
            }
        });
        assert!(s.failure.is_some());
        assert!(s.latest.is_none());
        s.tick(&mut |_, _| panic!("fault must not retrigger"));
    }
    #[test]
    fn invalid_fps_and_concurrent_legacy_calls_do_not_touch_backend() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000}));
        let mut forbidden =
            |_: &str, _: Value| -> Result<Frame> { panic!("unexpected camera call") };
        for fps in [json!(0), json!(-1), json!(121), json!("bad"), Value::Null] {
            assert!(
                s.command("stream-start", json!({"maxFps":fps}), &mut forbidden)
                    .is_err()
            );
        }
        for method in ["list", "get", "set", "start", "download", "white-balance"] {
            assert!(s.command(method, Value::Null, &mut forbidden).is_err());
        }
        assert!(
            s.command("stream-status", Value::Null, &mut forbidden)
                .is_ok()
        );
    }
    #[test]
    fn terminal_failure_discards_buffer_and_never_retries_in_a_loop() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000}));
        s.tick(&mut backend);
        let mut calls = 0;
        let mut failed = |_: &str, _: Value| -> Result<Frame> {
            calls += 1;
            bail!("removed")
        };
        for _ in 0..50 {
            s.tick(&mut failed);
        }
        assert_eq!(calls, 1);
        assert!(s.latest.is_none());
        assert!(!s.ready(Instant::now()));
        assert!(s.command("stream-start", json!({}), &mut backend).is_err());
        s.command("stream-stop", Value::Null, &mut backend).unwrap();
        assert!(s.failure.is_none());
    }
    #[test]
    fn failed_cleanup_requires_host_replacement_not_more_grabs() {
        let mut s = Stream::default();
        start(&mut s, json!({"microseconds":1000}));
        let mut fail = |_: &str, _: Value| -> Result<Frame> { bail!("cleanup failed") };
        assert!(s.command("stream-stop", Value::Null, &mut fail).is_err());
        assert!(s.failure.as_ref().unwrap().contains("host replacement"));
        s.tick(&mut |_, _| panic!("must not keep capturing"));
    }
    #[test]
    fn long_or_non_video_camera_uses_honest_still_fallback_with_boundary_stop() {
        for (native, exposure) in [(true, 60_000_000), (false, 1000)] {
            let mut s = Stream {
                camera: if native {
                    json!({"captureModes":["still","video"],"videoMaxExposureMicroseconds":30_000_000})
                } else {
                    json!({"captureModes":["still"]})
                },
                ..Stream::default()
            };
            s.command(
                "stream-start",
                json!({"microseconds":exposure}),
                &mut backend,
            )
            .unwrap();
            assert_eq!(s.params.as_ref().unwrap()["mode"], "still");
            s.command("stream-stop", Value::Null, &mut backend).unwrap();
            assert!(s.stopping);
            assert!(!s.ready(Instant::now()));
            s.tick(&mut backend);
            assert!(s.params.is_none());
            assert!(s.latest.is_none());
        }
    }
}
