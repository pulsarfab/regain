use crate::*;
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

/// One owner per camera. All public entry points are serialized by the frontend.
/// Status and queued controls remain readable while capture owns the worker.
pub struct Session {
    pub status: SharedStatus,
    pub selection: Selection,
    runtime: Runtime,
    log: Diagnostic,
    worker: Option<Worker>,
    direct: bool,
    ever_opened: bool,
    applied: BTreeMap<i32, i64>,
    recovery_temperature: Option<f64>,
    recovery_power: Option<i64>,
    settle_required: bool,
}
impl Session {
    pub fn new(selection: Selection, runtime: Runtime, log: Diagnostic) -> Result<Self> {
        selection.recovery.validate()?;
        Ok(Self {
            status: Arc::new(Mutex::new(Status::default())),
            direct: selection.direct,
            selection,
            runtime,
            log,
            worker: None,
            ever_opened: false,
            applied: BTreeMap::new(),
            recovery_temperature: None,
            recovery_power: None,
            settle_required: false,
        })
    }
    fn emit(&self, level: &str, event: &str, message: impl AsRef<str>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.log)(level, event, message.as_ref())
        }));
    }
    fn phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.status.lock().unwrap().phase = phase.clone();
        self.emit(
            if matches!(
                phase.as_str(),
                "Idle" | "Starting exposure" | "Exposing" | "Downloading"
            ) {
                "debug"
            } else {
                "info"
            },
            "session.phase",
            phase,
        );
    }
    pub fn snapshot(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    pub async fn simulate_read_failures(
        &mut self,
        count: u32,
        token: &CancellationToken,
    ) -> Result<()> {
        ensure!(
            self.runtime.simulate && self.direct,
            "Read fault injection requires a simulated direct camera"
        );
        self.call(
            "simulate-read-failures",
            json!({"count":count}),
            None,
            token,
        )
        .await?;
        Ok(())
    }
    pub fn seed_recovery(&mut self, temperature: Option<f64>, power: Option<i64>) {
        self.recovery_temperature = temperature.filter(|t| t.is_finite());
        self.recovery_power = power;
        self.settle_required = true;
    }
    pub fn queue_control(status: &SharedStatus, kind: i32, value: i64) -> Result<()> {
        let mut state = status.lock().unwrap();
        let cap = state
            .controls
            .get(&kind)
            .ok_or_else(|| invalid(format!("Control {kind} unavailable")))?;
        ensure!(
            cap.writable && value >= cap.min && value <= cap.max,
            Failure::Invalid(format!(
                "Control {kind} must be writable and between {} and {}",
                cap.min, cap.max
            ))
        );
        ensure!(
            matches!(
                kind,
                0 | 2 | 3 | 4 | 5 | 6 | 7 | 9 | 13 | 14 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23
            ),
            Failure::Invalid("Control is not a persistent imaging setting".into())
        );
        state.values.insert(kind, value);
        Ok(())
    }
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        seconds: Option<f64>,
        token: &CancellationToken,
    ) -> Result<(Value, Vec<u8>)> {
        self.worker
            .as_mut()
            .context("Camera worker is disconnected")?
            .call(
                method,
                params,
                seconds.unwrap_or(self.selection.recovery.command_timeout_seconds),
                token,
            )
            .await
    }
    async fn delay(&self, seconds: f64, token: &CancellationToken) -> Result<()> {
        tokio::select! {biased;_=token.cancelled()=>Err(Failure::Cancelled.into()),_=tokio::time::sleep(Duration::from_secs_f64(seconds))=>Ok(())}
    }
    async fn invalidate(&mut self) {
        if self.ever_opened && !self.settle_required {
            let state = self.snapshot();
            self.recovery_temperature = state.values.get(&8).map(|v| *v as f64 / 10.);
            self.recovery_power = state.values.get(&15).copied();
            self.settle_required = true;
        }
        self.status.lock().unwrap().control_connection_available = false;
        if let Some(mut worker) = self.worker.take() {
            worker.kill().await;
        }
    }
    async fn fallback(&mut self, reason: &str, token: &CancellationToken) -> Result<()> {
        self.emit(
            "warning",
            "backend.fallback",
            format!("Switching to SDK fallback: {reason}"),
        );
        self.invalidate().await;
        self.direct = false;
        self.phase("SDK fallback reconnect delay");
        self.delay(self.selection.recovery.reconnect_delay_seconds, token)
            .await
    }
    pub async fn connect(&mut self, token: &CancellationToken) -> Result<()> {
        let result = async {
            match self.open(token).await {
                Err(error) if self.direct && self.selection.sdk_fallback && retryable(&error) => {
                    self.fallback(&error.to_string(), token).await?;
                    self.open(token).await
                }
                other => other,
            }?;
            self.ever_opened = true;
            self.status.lock().unwrap().connected = true;
            self.phase("Idle");
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            self.emit("warning", "connection.failed", format!("{error:#}"));
            self.invalidate().await;
        }
        result
    }
    async fn open(&mut self, token: &CancellationToken) -> Result<()> {
        if self.ever_opened && self.selection.serial.is_none() {
            return Err(invalid(
                "Automatic recovery requires a camera serial number",
            ));
        }
        self.phase("Opening");
        self.worker = Some(self.runtime.spawn(self.direct, self.log.clone()).await?);
        self.applied.clear();
        let (result, _) = self
            .call(
                "open",
                json!({"name":self.selection.name,"serial":self.selection.serial}),
                None,
                token,
            )
            .await?;
        let serial = result["serial"].as_str().map(str::to_owned);
        ensure!(
            self.selection.serial.is_none() || self.selection.serial == serial,
            Failure::Invalid("Camera identity changed".into())
        );
        let info = &result["info"];
        ensure!(
            info["name"] == self.selection.name
                && info["formats"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(2))),
            Failure::Invalid("Camera identity or RAW16 support changed".into())
        );
        let previous = self.snapshot();
        ensure!(
            !self.ever_opened
                || (previous.info["width"] == info["width"]
                    && previous.info["height"] == info["height"]),
            Failure::Invalid("Camera geometry changed".into())
        );
        let mut controls: Vec<Control> = serde_json::from_value(result["controls"].clone())
            .map_err(|_| invalid("Invalid camera controls"))?;
        if self.direct
            && self.selection.sdk_fallback
            && matches!(
                self.selection.name.as_str(),
                "ZWO ASI2600MM Duo" | "ZWO ASI220MM Mini"
            )
            && let Some(exp) = controls.iter_mut().find(|c| c.kind == 1)
        {
            exp.max = 2_000_000_000;
        }
        ensure!(
            controls.iter().any(|c| c.kind == 1),
            Failure::Invalid("Exposure control unavailable".into())
        );
        if info["cooled"] == true {
            ensure!(
                [8, 16, 17]
                    .iter()
                    .all(|k| controls.iter().any(|c| c.kind == *k)),
                Failure::Invalid("Required cooling controls unavailable".into())
            );
        }
        self.selection.serial = serial.clone();
        {
            let mut state = self.status.lock().unwrap();
            state.info = info.clone();
            state.serial = serial;
            state.sdk_version = result["sdkVersion"].as_str().unwrap_or("unknown").into();
            state.backend = if self.direct { "direct" } else { "sdk" }.into();
            state.sdk_fallback = self.selection.direct && !self.direct;
            state.controls = controls.into_iter().map(|c| (c.kind, c)).collect();
            for c in state.controls.values().cloned().collect::<Vec<_>>() {
                if (c.writable || c.kind == 6)
                    && matches!(
                        c.kind,
                        0 | 2
                            | 3
                            | 4
                            | 5
                            | 6
                            | 7
                            | 9
                            | 13
                            | 14
                            | 16
                            | 17
                            | 18
                            | 19
                            | 20
                            | 21
                            | 22
                            | 23
                    )
                {
                    state.values.entry(c.kind).or_insert(c.value);
                } else {
                    state.values.insert(c.kind, c.value);
                }
            }
            state.control_connection_available = true;
            state.process_id = self.worker.as_ref().and_then(Worker::pid);
        }
        self.emit(
            "info",
            "connection.opened",
            format!(
                "Camera opened using {}{}; serial {}",
                if self.direct { "direct" } else { "SDK" },
                if self.selection.direct && !self.direct {
                    " fallback"
                } else {
                    ""
                },
                self.selection.serial.as_deref().unwrap_or("unavailable")
            ),
        );
        Ok(())
    }
    fn settings(&self) -> BTreeMap<i32, i64> {
        let state = self.snapshot();
        state
            .values
            .into_iter()
            .filter(|(k, _)| {
                state.controls.get(k).is_some_and(|c| c.writable || *k == 6)
                    && matches!(
                        k,
                        0 | 2
                            | 3
                            | 4
                            | 5
                            | 6
                            | 7
                            | 9
                            | 13
                            | 14
                            | 16
                            | 17
                            | 18
                            | 19
                            | 20
                            | 21
                            | 22
                            | 23
                    )
            })
            .collect()
    }
    async fn apply(
        &mut self,
        values: &mut BTreeMap<i32, i64>,
        token: &CancellationToken,
    ) -> Result<()> {
        let mut ordered: Vec<_> = values.iter().map(|(k, v)| (*k, *v)).collect();
        ordered.sort_by_key(|(k, _)| if *k == 17 { 100 } else { *k });
        for (kind, value) in ordered {
            if self.applied.get(&kind) == Some(&value) {
                continue;
            }
            let state = self.snapshot();
            let cap = state
                .controls
                .get(&kind)
                .ok_or_else(|| invalid(format!("Control {kind} disappeared after reconnect")))?;
            if !cap.writable {
                ensure!(
                    cap.value == value,
                    Failure::Invalid(format!("Read-only control {kind} changed"))
                );
                self.applied.insert(kind, value);
                continue;
            }
            self.call("set", json!({"control":kind,"value":value}), None, token)
                .await?;
            let actual = self
                .call("get", json!({"control":kind}), None, token)
                .await?
                .0
                .as_i64()
                .ok_or_else(|| invalid("Invalid control readback"))?;
            if actual != value {
                ensure!(
                    !self.direct && kind == 5 && actual >= cap.min && actual <= cap.max,
                    "Control {kind} readback {actual} differs from requested {value}"
                );
                self.emit(
                    "info",
                    "control.clamped",
                    format!("SDK applied offset {actual} instead of {value}"),
                );
                values.insert(kind, actual);
                let mut shared = self.status.lock().unwrap();
                if shared.values.get(&kind) == Some(&value) {
                    shared.values.insert(kind, actual);
                }
            }
            self.applied.insert(kind, actual);
        }
        Ok(())
    }
    async fn read_environment(
        &mut self,
        token: &CancellationToken,
    ) -> Result<(Option<f64>, Option<i64>)> {
        for kind in [8, 15] {
            if self.snapshot().controls.contains_key(&kind) {
                let value = self
                    .call("get", json!({"control":kind}), None, token)
                    .await?
                    .0
                    .as_i64()
                    .ok_or_else(|| invalid("Invalid environment value"))?;
                self.status.lock().unwrap().values.insert(kind, value);
            }
        }
        let state = self.snapshot();
        Ok((
            state.values.get(&8).map(|v| *v as f64 / 10.),
            state.values.get(&15).copied(),
        ))
    }
    pub async fn refresh(&mut self, token: &CancellationToken) -> Result<()> {
        let result = async {
            if self.worker.is_none() {
                self.phase("Restoring camera controls");
                self.delay(self.selection.recovery.reconnect_delay_seconds, token)
                    .await?;
                self.open(token).await?;
            }
            self.apply(&mut self.settings(), token).await?;
            self.read_environment(token).await?;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            self.emit("warning", "controls.failed", format!("{error:#}"));
            self.invalidate().await;
        }
        result
    }
    pub fn ready_timeout(&self, seconds: f64) -> f64 {
        let o = &self.selection.recovery;
        seconds
            + o.exposure_grace_seconds
            + if self.direct {
                (1 + if self.retained() || seconds <= o.maximum_retry_exposure_seconds {
                    o.direct_read_retries
                } else {
                    0
                }) as f64
                    * o.download_timeout_seconds
            } else {
                0.
            }
    }
    fn retained(&self) -> bool {
        self.direct && self.snapshot().info["retainedFrameReads"] == true
    }
    pub async fn capture(&mut self, e: Exposure, token: &CancellationToken) -> Result<Frame> {
        let state = self.snapshot();
        validate_capture(
            &state.info,
            &state.controls,
            &e,
            self.direct && self.selection.sdk_fallback,
        )?;
        let options = self.selection.recovery.clone();
        let seconds = e.microseconds as f64 / 1e6;
        let retries = if seconds <= options.maximum_retry_exposure_seconds {
            options.max_retries
        } else {
            0
        };
        let mut settings = self.settings();
        let mut prior = self
            .recovery_temperature
            .or_else(|| state.values.get(&8).map(|v| *v as f64 / 10.));
        let mut power = self
            .recovery_power
            .or_else(|| state.values.get(&15).copied());
        let mut last = None;
        for attempt in 0..=retries {
            let result = self
                .attempt(&e, &mut settings, &mut prior, &mut power, attempt, token)
                .await;
            match result {
                Ok(mut frame) => {
                    frame.metadata["recoveries"] = json!(attempt);
                    self.phase("Idle");
                    return Ok(frame);
                }
                Err(error) => {
                    {
                        let mut state = self.status.lock().unwrap();
                        state.error = Some(format!("{error:#}"));
                        state.sdk_error_code = match error.downcast_ref::<Failure>() {
                            Some(Failure::Worker { code, .. }) => *code,
                            _ => None,
                        };
                    }
                    let can_retry = retryable(&error) && attempt < retries && !token.is_cancelled();
                    self.emit(
                        "warning",
                        "capture.failed",
                        format!("Attempt {}/{}: {error:#}", attempt + 1, retries + 1),
                    );
                    self.invalidate().await;
                    if !can_retry {
                        last = Some(error);
                        break;
                    }
                    if self.direct && self.selection.sdk_fallback {
                        self.direct = false;
                        self.emit(
                            "warning",
                            "backend.fallback",
                            "Next permitted exposure retry will use SDK fallback",
                        );
                    }
                    self.emit("warning","capture.retry",format!("Scheduling replacement exposure {}/{}: {seconds} s, {}x{}, bin {}; reconnect delay {} s",attempt+1,retries,e.width,e.height,e.bin,options.reconnect_delay_seconds));
                    last = Some(error);
                }
            }
        }
        self.phase(if token.is_cancelled() {
            "Aborted"
        } else {
            "Error"
        });
        Err(last.unwrap_or_else(|| invalid("Capture failed")))
    }
    async fn attempt(
        &mut self,
        e: &Exposure,
        settings: &mut BTreeMap<i32, i64>,
        prior: &mut Option<f64>,
        power: &mut Option<i64>,
        attempt: u32,
        token: &CancellationToken,
    ) -> Result<Frame> {
        let options = self.selection.recovery.clone();
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        if attempt > 0 || self.worker.is_none() {
            self.phase("Reconnect delay");
            self.invalidate().await;
            self.delay(options.reconnect_delay_seconds, token).await?;
            self.open(token).await?;
        }
        self.apply(settings, token).await?;
        if self.settle_required
            && settings.get(&17).copied().unwrap_or(0) != 0
            && let Some(t) = *prior
        {
            self.settle(t, *power, *settings.get(&16).unwrap_or(&0) as f64, token)
                .await?;
        }
        let observed = self.read_environment(token).await?;
        *prior = observed.0.or(*prior);
        *power = observed.1.or(*power);
        if self.direct && self.selection.sdk_fallback {
            let result = self
                .call("validate", serde_json::to_value(e)?, None, token)
                .await;
            if let Err(error) = result {
                if matches!(
                    error.downcast_ref::<Failure>(),
                    Some(Failure::Worker { code: Some(8), .. })
                ) {
                    self.fallback(&error.to_string(), token).await?;
                    self.open(token).await?;
                    self.apply(settings, token).await?;
                    if settings.get(&17).copied().unwrap_or(0) != 0
                        && let Some(t) = *prior
                    {
                        self.settle(t, *power, *settings.get(&16).unwrap_or(&0) as f64, token)
                            .await?;
                    }
                } else {
                    return Err(error);
                }
            }
        }
        self.settle_required = false;
        self.recovery_temperature = None;
        self.recovery_power = None;
        self.phase("Starting exposure");
        let started = Utc::now();
        let seconds = e.microseconds as f64 / 1e6;
        let mut params = serde_json::to_value(e)?;
        if self.direct {
            params["readRetries"] = json!(if self.retained()
                || seconds <= options.maximum_retry_exposure_seconds
            {
                options.direct_read_retries
            } else {
                0
            });
            params["captureTimeoutSeconds"] =
                json!(self.ready_timeout(seconds) + options.command_timeout_seconds);
        }
        self.call("start", params, None, token).await?;
        self.phase("Exposing");
        let clock = Instant::now();
        let mut environment_sample = Instant::now();
        loop {
            let state = self
                .call("status", Value::Null, None, token)
                .await?
                .0
                .as_i64()
                .ok_or_else(|| invalid("Invalid exposure state"))?;
            self.status.lock().unwrap().sdk_exposure_state = Some(state);
            if state == 2 {
                break;
            }
            ensure!(state == 1, "Exposure ended in camera state {state}");
            ensure!(
                clock.elapsed().as_secs_f64() <= self.ready_timeout(seconds),
                "Exposure readiness timed out"
            );
            if environment_sample.elapsed() >= Duration::from_secs(2) {
                self.read_environment(token).await?;
                environment_sample = Instant::now();
            }
            self.delay(0.025, token).await?;
        }
        self.phase("Downloading");
        let mut reads = 0;
        let (mut metadata, pixels) = loop {
            match self
                .call(
                    "download",
                    Value::Null,
                    Some(options.download_timeout_seconds),
                    token,
                )
                .await
            {
                Ok(frame) => break frame,
                Err(error) if !self.retained() && retryable(&error) && !token.is_cancelled() => {
                    self.emit("warning", "transfer.failed", format!("{error:#}"));
                    let state = self
                        .call("status", Value::Null, None, token)
                        .await?
                        .0
                        .as_i64()
                        .ok_or_else(|| invalid("Invalid post-transfer state"))?;
                    self.status.lock().unwrap().sdk_exposure_state = Some(state);
                    if reads >= options.ready_frame_download_retries || state != 2 {
                        return Err(error);
                    }
                    reads += 1;
                    self.phase(format!(
                        "Rereading ready frame ({reads}/{})",
                        options.ready_frame_download_retries
                    ));
                    self.delay(options.reconnect_delay_seconds, token).await?;
                }
                Err(error) => return Err(error),
            }
        };
        ensure!(
            pixels.len() == e.bytes()?,
            Failure::Invalid("Image length differs from requested ROI".into())
        );
        for (key, expected) in [("width", e.width), ("height", e.height)] {
            ensure!(
                metadata[key].as_u64() == Some(expected as u64),
                Failure::Invalid(format!("Frame {key} differs from request"))
            );
        }
        if let Some(error) = metadata["cleanupError"].as_str() {
            self.emit(
                "warning",
                "capture.cleanup_failed",
                format!("Frame preserved; reconnect required: {error}"),
            );
            self.invalidate().await;
        }
        metadata["startedUtc"] = json!(started);
        metadata["endedUtc"] = json!(Utc::now());
        metadata["exposure"] = serde_json::to_value(e)?;
        metadata["controls"] = serde_json::to_value(settings)?;
        metadata["backend"] = json!(if self.direct { "direct" } else { "sdk" });
        metadata["sdkFallback"] = json!(self.selection.direct && !self.direct);
        metadata["downloadRetries"] = json!(reads);
        if attempt > 0 || reads > 0 || metadata["readRecoveries"].as_u64().unwrap_or(0) > 0 {
            self.emit("info","capture.recovered",format!("Returning {}x{} image after {attempt} replacement exposures and {reads} SDK read retries",e.width,e.height));
        }
        Ok(Frame {
            exposure: e.clone(),
            metadata,
            pixels: pixels.into(),
        })
    }
    async fn settle(
        &mut self,
        prior: f64,
        prior_power: Option<i64>,
        target: f64,
        token: &CancellationToken,
    ) -> Result<()> {
        let o = self.selection.recovery.clone();
        let clock = Instant::now();
        let mut stable = 0;
        let mut hold = CoolingHold::default();
        self.phase(format!("Restoring cooling near {prior:.1} C"));
        while clock.elapsed().as_secs_f64() < o.cooling_timeout_seconds {
            let (temperature, power) = self.read_environment(token).await?;
            let output = prior_power.is_none_or(|p| p <= 10 || power.is_some_and(|v| v >= p - 10));
            let near = temperature.is_some_and(|t| {
                t >= prior.min(target) - o.temperature_tolerance_c
                    && t <= prior + o.temperature_tolerance_c
            });
            let target_held = hold.observe(
                temperature,
                power,
                target,
                o.temperature_tolerance_c,
                o.cooling_sample_seconds,
                clock.elapsed().as_secs_f64(),
            );
            stable = if (near && output) || target_held {
                stable + 1
            } else {
                0
            };
            self.emit("info","cooling.wait",format!("Temperature {temperature:?} C (prior {prior}), power {power:?}% (prior {prior_power:?}%), stable {stable}/{}",o.cooling_stable_samples));
            if stable >= o.cooling_stable_samples {
                self.emit(
                    "info",
                    "cooling.recovered",
                    format!("Cooling recovered at {temperature:?} C; restored setpoint {target} C"),
                );
                return Ok(());
            }
            self.delay(o.cooling_sample_seconds, token).await?;
        }
        anyhow::bail!("Camera did not recover its prior cooling temperature and output")
    }
    pub async fn close(&mut self) {
        let token = CancellationToken::new();
        let closed = if self.worker.is_some() {
            self.call("close", Value::Null, Some(2.), &token)
                .await
                .is_ok()
        } else {
            false
        };
        self.invalidate().await;
        if !closed
            && self.ever_opened
            && self.direct
            && self.snapshot().info["cooled"] == true
            && self.selection.serial.is_some()
        {
            let cleanup = async {
                self.delay(self.selection.recovery.reconnect_delay_seconds, &token)
                    .await?;
                self.open(&token).await?;
                self.call("set", json!({"control":17,"value":0}), None, &token)
                    .await?;
                self.call("close", Value::Null, Some(2.), &token).await?;
                Result::<()>::Ok(())
            }
            .await;
            if let Err(error) = cleanup {
                self.emit(
                    "warning",
                    "cooling.cleanup_failed",
                    format!("Could not disable cooling: {error:#}"),
                );
            }
            self.invalidate().await;
        }
        self.status.lock().unwrap().connected = false;
        self.phase("Disconnected");
    }
}

#[derive(Default)]
pub struct CoolingHold {
    since: Option<f64>,
    first: f64,
    last: Option<f64>,
}
impl CoolingHold {
    pub fn observe(
        &mut self,
        temperature: Option<f64>,
        power: Option<i64>,
        target: f64,
        tolerance: f64,
        sample: f64,
        elapsed: f64,
    ) -> bool {
        let valid = temperature
            .is_some_and(|t| t.is_finite() && (t - target).abs() <= tolerance.min(1.))
            && power.is_some_and(|p| p > 0);
        if !valid {
            self.since = None;
            self.last = None;
            return false;
        }
        let t = temperature.unwrap();
        if self.since.is_none()
            || self
                .last
                .is_some_and(|last| elapsed < last || elapsed - last > 5f64.max(sample * 2.))
            || t > self.first + 0.2
        {
            self.since = Some(elapsed);
            self.first = t;
        }
        self.last = Some(elapsed);
        elapsed - self.since.unwrap() >= 30.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> Runtime {
        let directory = std::env::var_os("ZWOGAIN_TEST_WORKERS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
            });
        Runtime {
            directory,
            sdk: "unused".into(),
            simulate: true,
            sdk_simulation: Some(json!({"instant":true})),
        }
    }
    fn selection(direct: bool) -> Selection {
        Selection {
            name: if direct {
                "ZWO ASI676MC"
            } else {
                "ZWO Simulated"
            }
            .into(),
            serial: None,
            direct,
            sdk_fallback: false,
            recovery: RecoveryOptions {
                reconnect_delay_seconds: 0.01,
                cooling_sample_seconds: 0.01,
                cooling_stable_samples: 1,
                ..RecoveryOptions::default()
            },
        }
    }
    fn exposure() -> Exposure {
        Exposure {
            width: 64,
            height: 64,
            bin: 1,
            x: 0,
            y: 0,
            microseconds: 10000,
            dark: true,
        }
    }
    fn log() -> Diagnostic {
        Arc::new(|_, _, _| {})
    }
    #[test]
    fn cooling_requires_output_or_sustained_setpoint_and_resets_on_warming_or_gaps() {
        let mut hold = CoolingHold::default();
        for i in 0..=15 {
            assert_eq!(
                hold.observe(Some(-10.), Some(20), -10., 2., 2., i as f64 * 2.),
                i == 15
            );
        }
        assert!(!hold.observe(Some(-9.7), Some(20), -10., 2., 2., 32.));
        assert!(!hold.observe(Some(-10.), Some(20), -10., 2., 2., 100.));
        assert!(!hold.observe(Some(-10.), Some(0), -10., 2., 2., 102.));
    }
    #[tokio::test]
    async fn environment_refreshes_before_sdk_and_direct_exposures_finish() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut sel = selection(direct);
            if direct {
                sel.name = "ZWO ASI6200MM Pro".into();
            }
            let mut rt = runtime();
            rt.sdk_simulation = Some(json!({"instant":false}));
            let mut s = Session::new(sel, rt, log()).unwrap();
            s.connect(&token).await.unwrap();
            let shared = s.status.clone();
            let capture = async {
                s.capture(
                    Exposure {
                        microseconds: 6_000_000,
                        ..exposure()
                    },
                    &token,
                )
                .await
            };
            let observe = async {
                let deadline = Instant::now() + Duration::from_secs(5);
                while shared.lock().unwrap().phase != "Exposing" {
                    assert!(Instant::now() < deadline);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                // Make stale frontend values distinguishable from worker telemetry.
                {
                    let mut state = shared.lock().unwrap();
                    state.values.insert(8, 999);
                    state.values.insert(15, 99);
                }
                loop {
                    let state = shared.lock().unwrap().clone();
                    // The two worker reads update status separately. Wait for
                    // both before checking their values, including on slow CI.
                    if state.values[&8] != 999 && state.values[&15] != 99 {
                        assert_eq!(state.phase, "Exposing");
                        assert_eq!(state.values[&8], if direct { 250 } else { -100 });
                        assert_eq!(state.values[&15], if direct { 0 } else { 30 });
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "Telemetry stayed stale during capture"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let (frame, ()) = tokio::join!(capture, observe);
            assert_eq!(frame.unwrap().pixels.len(), 8192);
            s.close().await;
        }
    }
    #[tokio::test]
    async fn sdk_ready_frame_read_retry_does_not_replace_exposure() {
        let token = CancellationToken::new();
        let mut s = Session::new(selection(false), runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        s.call("fault", json!({"kind":"download"}), None, &token)
            .await
            .unwrap();
        let frame = s.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["recoveries"], 0);
        assert_eq!(frame.metadata["downloadRetries"], 1);
        assert_eq!(frame.pixels.len(), 8192);
        s.close().await;
    }
    #[tokio::test]
    async fn sdk_replacement_restores_controls_and_respects_long_exposure_limit() {
        for (seconds, allowed) in [(30., true), (30.000001, false), (1200., false)] {
            let token = CancellationToken::new();
            let mut sel = selection(false);
            sel.recovery.ready_frame_download_retries = 0;
            let mut s = Session::new(sel, runtime(), log()).unwrap();
            s.connect(&token).await.unwrap();
            Session::queue_control(&s.status, 0, 250).unwrap();
            Session::queue_control(&s.status, 16, -10).unwrap();
            s.call("fault", json!({"kind":"download"}), None, &token)
                .await
                .unwrap();
            let result = s
                .capture(
                    Exposure {
                        microseconds: (seconds * 1e6) as u64,
                        ..exposure()
                    },
                    &token,
                )
                .await;
            assert_eq!(result.is_ok(), allowed);
            if let Ok(frame) = result {
                assert_eq!(frame.metadata["recoveries"], 1);
                assert_eq!(frame.metadata["controls"]["0"], 250);
            }
            s.close().await;
        }
    }
    #[tokio::test]
    async fn direct_retained_reads_and_abort_share_the_recovery_session() {
        let token = CancellationToken::new();
        let mut s = Session::new(selection(true), runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        s.call("simulate-read-failures", json!({"count":2}), None, &token)
            .await
            .unwrap();
        let frame = s.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["readRecoveries"], 2);
        assert_eq!(frame.metadata["recoveries"], 0);
        let abort = CancellationToken::new();
        let cancel = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        });
        assert!(
            s.capture(
                Exposure {
                    microseconds: 2000000,
                    ..exposure()
                },
                &abort
            )
            .await
            .is_err()
        );
        assert_eq!(
            s.capture(exposure(), &token).await.unwrap().pixels.len(),
            8192
        );
        s.close().await;
    }
    #[tokio::test]
    async fn direct_fallback_revalidates_same_identity_and_uses_one_budget() {
        let token = CancellationToken::new();
        let mut sel = selection(true);
        sel.sdk_fallback = true;
        let mut runtime = runtime();
        runtime.sdk_simulation = Some(
            json!({"name":"ZWO ASI676MC","width":3552,"height":3552,"bins":[1],"serial":"direct-simulator","cooled":false,"instant":true}),
        );
        let mut s = Session::new(sel, runtime, log()).unwrap();
        s.connect(&token).await.unwrap();
        s.call("simulate-read-failures", json!({"count":3}), None, &token)
            .await
            .unwrap();
        let frame = s.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["backend"], "sdk");
        assert_eq!(frame.metadata["sdkFallback"], true);
        assert_eq!(frame.metadata["recoveries"], 1);
        s.close().await;
    }
    #[test]
    fn invalid_limits_are_rejected() {
        assert!(
            RecoveryOptions {
                maximum_retry_exposure_seconds: f64::NAN,
                ..RecoveryOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            RecoveryOptions {
                direct_read_retries: 6,
                ..RecoveryOptions::default()
            }
            .validate()
            .is_err()
        );
    }
}
