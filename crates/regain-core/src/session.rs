use crate::timing::{
    ACKNOWLEDGED_CONTROLS, CLOSE_SECONDS, MAX_READ_RETRY_OVERHEAD_SECONDS, PERSISTENT_CONTROLS,
    USB_BIND_SECONDS, USB_REBIND_PAUSE_SECONDS, USB_RESET_SECONDS,
};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenPurpose {
    Acquisition,
    ThermalShutdown,
}
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
    recovery_target: Option<i64>,
    settle_required: bool,
    cooling_seeded: bool,
    usb_target: Option<String>,
    cooling: cooling::Mailbox,
}
impl Session {
    pub fn new(selection: Selection, runtime: Runtime, log: Diagnostic) -> Result<Self> {
        selection.recovery.validate()?;
        let status = Arc::new(Mutex::new(Status::default()));
        let observed = status.clone();
        let log: Diagnostic = Arc::new(move |level, event, message| {
            // The direct worker reports transfer retries while its status call
            // is still pending. Surface those records without parsing prose or
            // changing any recovery decisions. Final frame metadata reconciles
            // successful reads if stderr delivery lagged behind the reply.
            if matches!(event, "transfer.retry" | "transfer.exhausted") {
                let mut state = observed.lock().unwrap();
                if matches!(
                    state.phase.as_str(),
                    "Starting exposure" | "Exposing" | "Downloading"
                ) {
                    if event == "transfer.retry" {
                        state.retry.usb_reads = state.retry.usb_reads.saturating_add(1);
                    }
                    state.retry.last_failure = Some(message.into());
                }
            }
            log(level, event, message);
        });
        Ok(Self {
            status,
            direct: selection.direct,
            selection,
            runtime,
            log,
            worker: None,
            ever_opened: false,
            applied: BTreeMap::new(),
            recovery_temperature: None,
            recovery_power: None,
            recovery_target: None,
            settle_required: false,
            cooling_seeded: false,
            usb_target: None,
            cooling: cooling::Mailbox::default(),
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
    pub fn cooling(&self) -> cooling::CoolingHandle {
        cooling::CoolingHandle {
            mailbox: self.cooling.clone(),
            status: self.status.clone(),
        }
    }
    /// Drive one reserved command while idle. Capture drives the same mailbox
    /// at safe worker checkpoints. Frontends retain/serialize this operation.
    pub async fn service_cooling(&mut self, token: &CancellationToken) -> Result<()> {
        self.service_cooling_with(&mut self.settings(), token).await
    }
    async fn service_cooling_with(
        &mut self,
        settings: &mut BTreeMap<i32, i64>,
        token: &CancellationToken,
    ) -> Result<()> {
        let Some(request) = self.cooling.claim() else {
            return Ok(());
        };
        let (kind, value, deadline) = {
            let request = request.lock().unwrap();
            (request.kind, request.value, request.deadline)
        };
        let deadline = deadline.min(
            Instant::now()
                + Duration::from_secs_f64(self.selection.recovery.command_timeout_seconds),
        );
        let mailbox = self.cooling.clone();
        if let Err(error) = cooling::validate(&self.status, kind, value) {
            let _ = mailbox.finish(&request, Err(error), || {});
            return Ok(());
        }
        if token.is_cancelled() {
            let _ = mailbox.finish(&request, Err(cooling::CoolingError::Cancelled), || {});
            return Err(Failure::Cancelled.into());
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64();
        if remaining == 0. {
            let _ = mailbox.finish(&request, Err(cooling::CoolingError::Expired), || {});
            return Ok(());
        }
        if !mailbox.dispatch(&request) {
            return Ok(());
        }
        let result = self
            .write_control(kind, value, deadline, token, false)
            .await;
        let observation = result.as_ref().ok().copied();
        let result = mailbox.finish(&request, result.map(|observed| observed.value), || {
            self.applied.insert(kind, value);
            settings.insert(kind, value);
            let mut state = self.status.lock().unwrap();
            state.values.insert(kind, value);
            if let Some(observation) = observation {
                state.observations.insert(kind, observation);
            }
        });
        match result {
            Err(error @ cooling::CoolingError::Uncertain { .. }) => {
                self.control_result(Err(error)).await.map(|_| ())
            }
            Err(cooling::CoolingError::Cancelled) => Err(Failure::Cancelled.into()),
            _ => Ok(()), // Unsent expiry/failure belongs to the command receipt.
        }
    }
    /// Acknowledge gain/offset while idle, using the same serialized worker as
    /// capture. The caller must retain this future through dispatch/cleanup.
    /// Unlike queue_control, success means write plus readback, not desired intent.
    /// The absolute deadline includes time spent waiting for owner admission.
    pub async fn set_imaging_control(
        &mut self,
        kind: i32,
        value: i64,
        deadline: Instant,
        token: &CancellationToken,
    ) -> Result<i64> {
        ensure!(
            matches!(kind, 0 | 5),
            invalid("Only gain and offset are supported")
        );
        let state = self.snapshot();
        ensure!(
            state.connected && state.control_connection_available && self.worker.is_some(),
            invalid("Camera controls are unavailable")
        );
        ensure!(
            !self.cooling().pending(),
            invalid("A cooler command is pending")
        );
        let cap = state
            .controls
            .get(&kind)
            .ok_or_else(|| invalid("Control is unavailable"))?;
        ensure!(
            cap.kind == kind
                && cap.writable
                && cap.min <= cap.max
                && (cap.min..=cap.max).contains(&value),
            invalid("Control value is outside writable capabilities")
        );
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        if Instant::now() >= deadline {
            return Err(cooling::CoolingError::Expired.into());
        }
        let deadline = deadline.min(
            Instant::now()
                + Duration::from_secs_f64(self.selection.recovery.command_timeout_seconds),
        );
        let result = self.write_control(kind, value, deadline, token, true).await;
        let observation = self.control_result(result).await?;
        self.applied.insert(kind, observation.value);
        let mut state = self.status.lock().unwrap();
        state.values.insert(kind, observation.value);
        state.observations.insert(kind, observation);
        Ok(observation.value)
    }
    async fn write_control(
        &mut self,
        kind: i32,
        value: i64,
        deadline: Instant,
        token: &CancellationToken,
        allow_sdk_offset_clamp: bool,
    ) -> std::result::Result<ControlObservation, cooling::CoolingError> {
        let mut set_acknowledged = false;
        let result = async {
            self.worker
                .as_mut()
                .context("Camera worker is disconnected")?
                .control_call(
                    "set",
                    json!({"control":kind,"value":value}),
                    deadline,
                    token,
                )
                .await?;
            set_acknowledged = true;
            self.emit(
                "debug",
                "control.write_acknowledged",
                format!("Control {kind}; readback pending"),
            );
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64();
            ensure!(remaining > 0., "Control deadline expired before readback");
            let request_started = Instant::now();
            let reply = self
                .worker
                .as_mut()
                .context("Camera worker is disconnected")?
                .control_call("get-observation", json!({"control":kind}), deadline, token)
                .await?
                .0;
            let observation = serde_json::from_value::<ControlObservationReply>(reply)
                .map_err(|_| invalid("Invalid control readback"))?
                .normalize(request_started)?;
            let actual = observation.value;
            if actual != value {
                let cap = self.snapshot().controls.get(&kind).cloned();
                ensure!(
                    allow_sdk_offset_clamp
                        && !self.direct
                        && kind == 5
                        && cap.is_some_and(|cap| (cap.min..=cap.max).contains(&actual)),
                    "Control readback differs from request"
                );
                self.emit(
                    "info",
                    "control.clamped",
                    format!("SDK applied offset {actual} instead of {value}"),
                );
            }
            ensure!(
                Instant::now() < deadline,
                "Control readback exceeded its deadline"
            );
            Ok::<_, anyhow::Error>(observation)
        }
        .await;
        result.map_err(|error| {
            if !set_acknowledged
                && matches!(
                    error.downcast_ref::<cooling::CoolingError>(),
                    Some(cooling::CoolingError::Expired)
                )
            {
                cooling::CoolingError::Expired
            } else if !set_acknowledged
                && matches!(error.downcast_ref::<Failure>(), Some(Failure::Cancelled))
            {
                cooling::CoolingError::Cancelled
            } else {
                let code = match error.downcast_ref::<Failure>() {
                    Some(Failure::Worker { code, .. } | Failure::UncertainControl { code, .. }) => {
                        *code
                    }
                    _ => None,
                };
                cooling::CoolingError::Uncertain {
                    message: format!("{error:#}"),
                    code,
                }
            }
        })
    }
    async fn control_result(
        &mut self,
        result: std::result::Result<ControlObservation, cooling::CoolingError>,
    ) -> Result<ControlObservation> {
        match result {
            Err(cooling::CoolingError::Uncertain { message, code }) => {
                self.invalidate().await;
                Err(Failure::UncertainControl { message, code }.into())
            }
            Err(cooling::CoolingError::Cancelled) => {
                // Worker cancellation can retire an otherwise untouched process.
                // Withdraw its core availability before another owner operation.
                self.invalidate().await;
                Err(Failure::Cancelled.into())
            }
            Err(error) => Err(error.into()),
            Ok(actual) => Ok(actual),
        }
    }
    /// Opt into Regain-owned WB. The caller serializes this with capture, like
    /// all Session operations. Settings and effective AWB gains survive recovery.
    pub async fn set_white_balance(
        &mut self,
        settings: crate::white_balance::Settings,
        token: &CancellationToken,
    ) -> Result<()> {
        settings.gains.validate()?;
        let result = self
            .call(
                "white-balance",
                serde_json::to_value(settings)?,
                None,
                token,
            )
            .await?
            .0;
        self.status.lock().unwrap().white_balance =
            Some(serde_json::from_value(result["settings"].clone())?);
        // Legacy WB/flip controls must not be replayed over managed WB.
        for kind in [3, 4, 9] {
            self.applied.remove(&kind);
        }
        Ok(())
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
    pub fn seed_recovery_target(&mut self, target: Option<i64>) {
        self.recovery_target = target.filter(|v| (-40..=30).contains(v));
    }
    pub fn queue_control(status: &SharedStatus, kind: i32, value: i64) -> Result<()> {
        let mut state = status.lock().unwrap();
        ensure!(
            state.white_balance.is_none() || !matches!(kind, 3 | 4 | 9),
            Failure::Invalid("WB and flip controls are owned by managed white balance".into())
        );
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
            PERSISTENT_CONTROLS.contains(&kind),
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
        self.cooling.retire();
        if self.ever_opened && !self.settle_required {
            let state = self.snapshot();
            self.recovery_temperature = state.values.get(&8).map(|v| *v as f64 / 10.);
            self.recovery_power = state.values.get(&15).copied();
            self.recovery_target = self
                .applied
                .get(&16)
                .copied()
                .or_else(|| state.values.get(&16).copied());
            self.settle_required = true;
        }
        self.status.lock().unwrap().control_connection_available = false;
        if let Some(mut worker) = self.worker.take() {
            worker.kill().await;
        }
        self.status.lock().unwrap().process_id = None;
    }
    async fn fallback(&mut self, reason: &str, token: &CancellationToken) -> Result<()> {
        self.status.lock().unwrap().retry.last_failure = Some(reason.into());
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
            if self.selection.recovery.usb_reset_after_failures > 0 && self.usb_target.is_none() {
                // First connection can learn the serial. Release its handle before
                // binding the physical device, then reopen that exact serial.
                self.invalidate().await;
                self.bind_usb(token).await?;
                self.open(token).await?;
            }
            self.ever_opened = true;
            self.status.lock().unwrap().connected = true;
            self.phase("Idle");
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            if !token.is_cancelled() {
                self.status.lock().unwrap().retry.last_failure = Some(format!("{error:#}"));
            }
            self.emit("warning", "connection.failed", format!("{error:#}"));
            self.invalidate().await;
        }
        result
    }
    async fn bind_usb(&mut self, token: &CancellationToken) -> Result<()> {
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        if self.runtime.simulate {
            self.usb_target = Some("simulation".into());
            return Ok(());
        }
        let serial = self
            .selection
            .serial
            .as_deref()
            .context("USB recovery requires a camera serial")?;
        let args = [
            "zwo",
            "camera-direct",
            "--usb-target",
            &self.selection.name,
            serial,
        ];
        let command = self.runtime.usb_command(&args, USB_BIND_SECONDS);
        let encoded = tokio::select! { biased; _=token.cancelled()=>return Err(Failure::Cancelled.into()), r=command=>r? };
        let target = regain_transport::usb::Target::decode(&encoded)?;
        ensure!(
            target.serial.eq_ignore_ascii_case(serial),
            "USB recovery serial mismatch"
        );
        self.usb_target = Some(encoded);
        self.emit(
            "info",
            "usb.bound",
            "USB recovery bound to the selected camera's serial and physical location",
        );
        Ok(())
    }
    async fn reset_usb(&mut self, token: &CancellationToken) -> Result<()> {
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        let target = self
            .usb_target
            .as_deref()
            .context("No verified USB recovery target")?;
        ensure!(
            self.worker.is_none(),
            "Close the camera worker before USB recovery"
        );
        self.phase("USB recovery");
        self.emit(
            "warning",
            "usb.reset",
            "Resetting the selected camera; the retained frame is abandoned",
        );
        if !self.runtime.simulate {
            // Once dispatched, finish the bounded helper before honoring abort so
            // a Linux port cycle can always re-enable the port.
            self.runtime
                .usb_command(
                    &[
                        "usb",
                        if self.selection.recovery.usb_port_cycle {
                            "cycle"
                        } else {
                            "reset"
                        },
                        target,
                    ],
                    USB_RESET_SECONDS,
                )
                .await?;
        }
        self.usb_target = None;
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        self.phase("Waiting for USB camera");
        let deadline = Instant::now() + Duration::from_secs(USB_BIND_SECONDS);
        loop {
            match self.bind_usb(token).await {
                Ok(()) => break,
                Err(e) if token.is_cancelled() || Instant::now() >= deadline => return Err(e),
                Err(_) => self.delay(USB_REBIND_PAUSE_SECONDS, token).await?,
            }
        }
        self.emit(
            "info",
            "usb.returned",
            "The same camera serial is available after USB recovery",
        );
        Ok(())
    }
    async fn open(&mut self, token: &CancellationToken) -> Result<()> {
        self.open_worker(token, OpenPurpose::Acquisition).await
    }
    async fn open_worker(&mut self, token: &CancellationToken, purpose: OpenPurpose) -> Result<()> {
        if self.ever_opened && self.selection.serial.is_none() {
            return Err(invalid(
                "Automatic recovery requires a camera serial number",
            ));
        }
        self.phase(if purpose == OpenPurpose::ThermalShutdown {
            "Closing camera"
        } else {
            "Opening"
        });
        self.worker = Some(self.runtime.spawn(self.direct, self.log.clone()).await?);
        self.applied.clear();
        // Values retain desired recovery settings; old evidence cannot describe
        // a replacement worker or its newly negotiated capabilities.
        self.status.lock().unwrap().observations.clear();
        self.cooling_seeded = false;
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
                if (c.writable || c.kind == 6) && PERSISTENT_CONTROLS.contains(&c.kind) {
                    state.values.entry(c.kind).or_insert(c.value);
                } else {
                    state.values.insert(c.kind, c.value);
                }
            }
            state.control_connection_available = purpose == OpenPurpose::Acquisition;
            state.process_id = self.worker.as_ref().and_then(Worker::pid);
            state.white_balance_capabilities = result["whiteBalance"].clone();
        }
        if purpose == OpenPurpose::Acquisition
            && let Some(settings) = previous.white_balance
        {
            // A new worker has no previous estimate for Locked to freeze.
            if settings.mode == crate::white_balance::Mode::Locked {
                self.set_white_balance(
                    crate::white_balance::Settings {
                        mode: crate::white_balance::Mode::Manual,
                        ..settings
                    },
                    token,
                )
                .await?;
            }
            self.set_white_balance(settings, token).await?;
        }
        self.emit(
            "info",
            if purpose == OpenPurpose::ThermalShutdown {
                "camera.cleanup_opened"
            } else {
                "connection.opened"
            },
            format!(
                "Camera opened using {}{}; serial {}{}",
                if self.direct { "direct" } else { "SDK" },
                if self.selection.direct && !self.direct {
                    " fallback"
                } else {
                    ""
                },
                self.selection.serial.as_deref().unwrap_or("unavailable"),
                if purpose == OpenPurpose::ThermalShutdown {
                    "; thermal shutdown only"
                } else {
                    ""
                }
            ),
        );
        if purpose == OpenPurpose::Acquisition {
            self.cooling.activate();
        }
        Ok(())
    }
    fn settings(&self) -> BTreeMap<i32, i64> {
        let state = self.snapshot();
        state
            .values
            .into_iter()
            .filter(|(k, _)| {
                (state.white_balance.is_none() || !matches!(k, 3 | 4 | 9))
                    && state.controls.get(k).is_some_and(|c| c.writable || *k == 6)
                    && PERSISTENT_CONTROLS.contains(k)
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
            if self.snapshot().white_balance.is_some() && matches!(kind, 3 | 4 | 9) {
                continue;
            }
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
            let observation = if ACKNOWLEDGED_CONTROLS.contains(&kind) {
                let deadline = Instant::now()
                    + Duration::from_secs_f64(self.selection.recovery.command_timeout_seconds);
                let result = self.write_control(kind, value, deadline, token, true).await;
                Some(self.control_result(result).await?)
            } else {
                self.call("set", json!({"control":kind,"value":value}), None, token)
                    .await?;
                None
            };
            let actual = if let Some(observation) = observation {
                observation.value
            } else {
                self.call("get", json!({"control":kind}), None, token)
                    .await?
                    .0
                    .as_i64()
                    .ok_or_else(|| invalid("Invalid control readback"))?
            };
            if actual != value {
                ensure!(
                    !self.direct && kind == 5 && actual >= cap.min && actual <= cap.max,
                    "Control {kind} readback {actual} differs from requested {value}"
                );
                // Acknowledged controls already emit their clamp diagnostic in
                // the shared helper; other controls cannot use this policy.
                values.insert(kind, actual);
                let mut shared = self.status.lock().unwrap();
                if shared.values.get(&kind) == Some(&value) {
                    shared.values.insert(kind, actual);
                }
            }
            self.applied.insert(kind, actual);
            if let Some(observation) = observation {
                self.status
                    .lock()
                    .unwrap()
                    .observations
                    .insert(kind, observation);
            }
        }
        if self.direct
            && self.settle_required
            && !self.cooling_seeded
            && values.get(&17).is_some_and(|v| *v != 0)
            && let (Some(power), Some(temperature)) =
                (self.recovery_power, self.recovery_temperature)
        {
            self.call(
                "resume-cooling",
                json!({"power":power,"temperature":temperature,"previousTarget":self.recovery_target.or_else(|| values.get(&16).copied()).unwrap_or(0)}),
                None,
                token,
            )
            .await?;
            self.cooling_seeded = true;
            self.emit(
                "info",
                "cooling.resumed",
                format!(
                    "Resumed prior cooler demand {power}% at restored target {} C",
                    values.get(&16).unwrap_or(&0)
                ),
            );
        }
        Ok(())
    }
    async fn observe_control(
        &mut self,
        kind: i32,
        token: &CancellationToken,
    ) -> Result<ControlObservation> {
        let request_started = Instant::now();
        let (reply, pixels) = self
            .call("get-observation", json!({"control":kind}), None, token)
            .await?;
        ensure!(
            pixels.is_empty(),
            invalid("Unexpected observation image payload")
        );
        serde_json::from_value::<ControlObservationReply>(reply)
            .map_err(|_| invalid("Invalid control observation"))?
            .normalize(request_started)
    }
    async fn read_environment(
        &mut self,
        token: &CancellationToken,
    ) -> Result<(Option<f64>, Option<i64>)> {
        for kind in [8, 15] {
            if self.snapshot().controls.contains_key(&kind) {
                let observation = self.observe_control(kind, token).await?;
                let mut state = self.status.lock().unwrap();
                state.values.insert(kind, observation.value);
                state.observations.insert(kind, observation);
            }
        }
        let state = self.snapshot();
        Ok((
            state.values.get(&8).map(|v| *v as f64 / 10.),
            state.values.get(&15).copied(),
        ))
    }
    /// Read-only idle telemetry on the existing worker. Unlike refresh, this
    /// never opens a replacement worker or applies desired configuration.
    pub async fn refresh_environment(&mut self, token: &CancellationToken) -> Result<()> {
        let state = self.snapshot();
        ensure!(
            state.connected && state.control_connection_available && self.worker.is_some(),
            invalid("Camera controls are unavailable")
        );
        let result = self.read_environment(token).await.map(|_| ());
        if let Err(error) = &result {
            self.emit("warning", "environment.failed", format!("{error:#}"));
            self.invalidate().await;
        }
        result
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
            if !token.is_cancelled() {
                self.status.lock().unwrap().retry.last_failure = Some(format!("{error:#}"));
            }
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
                    + o.direct_read_retries as f64
                        * self.snapshot().info["readRetryOverheadSeconds"]
                            .as_f64()
                            .unwrap_or(0.)
                            .clamp(0., MAX_READ_RETRY_OVERHEAD_SECONDS)
            } else {
                0.
            }
    }
    fn retained(&self) -> bool {
        self.direct && self.snapshot().info["retainedFrameReads"] == true
    }
    pub async fn capture(&mut self, e: Exposure, token: &CancellationToken) -> Result<Frame> {
        if self.snapshot().white_balance.is_some() {
            let state = self.snapshot();
            crate::white_balance::WhiteBalance::validate_geometry(
                state.info["color"] == true,
                state.info["bayer"].as_u64(),
                e.bin,
            )
            .map_err(|error| invalid(error.to_string()))?;
        }
        let state = self.snapshot();
        validate_capture(
            &state.info,
            &state.controls,
            &e,
            self.direct && self.selection.sdk_fallback,
        )?;
        let options = self.selection.recovery.clone();
        let seconds = e.microseconds as f64 / 1e6;
        let retries = options.replacement_exposures(e.microseconds);
        let mut settings = self.settings();
        let mut prior = self
            .recovery_temperature
            .or_else(|| state.values.get(&8).map(|v| *v as f64 / 10.));
        let mut power = self
            .recovery_power
            .or_else(|| state.values.get(&15).copied());
        let mut last = None;
        let mut usb_resets = 0;
        {
            let mut status = self.status.lock().unwrap();
            status.retry.recaptures = 0;
            status.retry.downloads = 0;
            status.retry.usb_reads = 0;
        }
        for attempt in 0..=retries {
            if attempt > 0 && !token.is_cancelled() {
                self.status.lock().unwrap().retry.recaptures = attempt;
            }
            let usb_reads_before = self.snapshot().retry.usb_reads;
            let result = self
                .attempt(&e, &mut settings, &mut prior, &mut power, attempt, token)
                .await;
            match result {
                Ok(mut frame) => {
                    {
                        let reads = frame.metadata["readRecoveries"]
                            .as_u64()
                            .unwrap_or(0)
                            .min(u32::MAX as u64) as u32;
                        let mut status = self.status.lock().unwrap();
                        // Close the diagnostic-counting window atomically with
                        // reconciliation: late stderr records must not count
                        // the same successful retry a second time.
                        status.retry.usb_reads = usb_reads_before.saturating_add(reads);
                        status.phase = "Idle".into();
                    }
                    frame.metadata["recoveries"] = json!(attempt);
                    frame.metadata["usbResets"] = json!(usb_resets);
                    let retries = self.snapshot().retry;
                    frame.metadata["retainedReadRetries"] = json!(retries.usb_reads);
                    frame.metadata["downloadRetriesTotal"] = json!(retries.downloads);
                    if attempt > 0 || retries.downloads > 0 || retries.usb_reads > 0 {
                        self.emit("info", "capture.recovered", format!(
                            "Returning {}x{} image after {attempt} replacement exposures, {} SDK read retries and {} retained-frame retries across all attempts; delivered frame used {} retained-frame retries and {} handle reopens",
                            e.width, e.height, retries.downloads, retries.usb_reads,
                            frame.metadata["readRecoveries"].as_u64().unwrap_or(0),
                            frame.metadata["handleReopens"].as_u64().unwrap_or(0)));
                    }
                    self.phase("Idle");
                    return Ok(frame);
                }
                Err(error) => {
                    if token.is_cancelled() {
                        // Finish framed replies before reaching this boundary.
                        // Acknowledged stop preserves the worker and cooler;
                        // an uncertain stop still retires the isolated worker.
                        let stopped = self
                            .call("stop", Value::Null, None, &CancellationToken::new())
                            .await;
                        if let Err(stop_error) = stopped {
                            let message = format!(
                                "Stop was not acknowledged; retiring worker: {stop_error:#}"
                            );
                            {
                                let mut state = self.status.lock().unwrap();
                                state.error = Some(message.clone());
                                state.retry.last_failure = Some(message.clone());
                            }
                            self.emit("warning", "capture.abort_failed", message);
                            self.invalidate().await;
                        } else {
                            self.status.lock().unwrap().error = None;
                            self.emit("info", "capture.aborted", "Client cancelled capture; camera stopped without reopening or replacing exposure");
                        }
                        self.phase("Aborted");
                        return Err(Failure::Cancelled.into());
                    }
                    {
                        let mut state = self.status.lock().unwrap();
                        state.error = Some(format!("{error:#}"));
                        if !token.is_cancelled() {
                            state.retry.last_failure = Some(format!("{error:#}"));
                        }
                        state.sdk_error_code = match error.downcast_ref::<Failure>() {
                            Some(
                                Failure::Worker { code, .. }
                                | Failure::UncertainControl { code, .. },
                            ) => *code,
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
                    if usb_resets == 0
                        && options.usb_reset_after_failures > 0
                        && attempt + 1 >= options.usb_reset_after_failures
                    {
                        usb_resets += 1;
                        if let Err(e) = self.reset_usb(token).await {
                            self.emit("error", "usb.failed", format!("{e:#}"));
                            last = Some(e);
                            break;
                        }
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
        self.service_cooling_with(settings, token).await?;
        self.apply(settings, token).await?;
        if self.settle_required
            && settings.get(&17).copied().unwrap_or(0) != 0
            && let Some(t) = *prior
        {
            self.settle(t, *power, settings, token).await?;
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
                        self.settle(t, *power, settings, token).await?;
                    }
                } else {
                    return Err(error);
                }
            }
        }
        self.settle_required = false;
        self.recovery_temperature = None;
        self.recovery_power = None;
        self.recovery_target = None;
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
            params["transferTimeoutSeconds"] = json!(options.download_timeout_seconds);
            params["readChunkKiB"] = json!(options.direct_read_chunk_kib);
        }
        // Never cancel a framed pipe exchange halfway through a reply. Check
        // cancellation at owner checkpoints, then send a bounded stop command.
        let exchange_token = CancellationToken::new();
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        self.call("start", params, None, &exchange_token).await?;
        self.phase("Exposing");
        let clock = Instant::now();
        let mut environment_sample = Instant::now();
        loop {
            if token.is_cancelled() {
                return Err(Failure::Cancelled.into());
            }
            self.service_cooling_with(settings, token).await?;
            let state = self
                .call("status", Value::Null, None, &exchange_token)
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
                self.read_environment(&exchange_token).await?;
                environment_sample = Instant::now();
            }
            self.delay(0.025, token).await?;
        }
        self.service_cooling_with(settings, token).await?;
        self.phase("Downloading");
        let mut reads = 0;
        let (mut metadata, pixels) = loop {
            match self
                .worker
                .as_mut()
                .context("Camera worker is disconnected")?
                .call_image(
                    "download",
                    Value::Null,
                    options.download_timeout_seconds,
                    &exchange_token,
                    e.bytes()?,
                )
                .await
            {
                Ok(frame) => break frame,
                Err(error) if !self.retained() && retryable(&error) && !token.is_cancelled() => {
                    self.status.lock().unwrap().retry.last_failure = Some(format!("{error:#}"));
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
                    self.status.lock().unwrap().retry.downloads += 1;
                    self.phase(format!(
                        "Rereading ready frame ({reads}/{})",
                        options.ready_frame_download_retries
                    ));
                    self.delay(options.reconnect_delay_seconds, token).await?;
                }
                Err(error) => return Err(error),
            }
        };
        if token.is_cancelled() {
            return Err(Failure::Cancelled.into());
        }
        {
            let mut state = self.status.lock().unwrap();
            let recovered = metadata["readRecoveries"]
                .as_u64()
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
            if let Some(failure) = metadata["readErrors"]
                .as_array()
                .or_else(|| metadata["readoutErrors"].as_array())
                .and_then(|errors| errors.last())
                .and_then(Value::as_str)
            {
                state.retry.last_failure = Some(failure.into());
            } else if recovered > 0 && state.retry.usb_reads == 0 {
                state.retry.last_failure = Some(
                    "USB frame read failed; frame recovered (worker supplied no failure detail)"
                        .into(),
                );
            }
            if let Some(failure) = metadata["cleanupError"].as_str() {
                state.retry.last_failure = Some(failure.into());
            }
        }
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
        } else {
            self.service_cooling_with(settings, token).await?;
        }
        metadata["startedUtc"] = json!(started);
        if self.snapshot().white_balance.is_some() {
            let settings = serde_json::from_value(metadata["whiteBalance"]["settings"].clone())
                .map_err(|_| invalid("Missing or invalid managed white balance frame metadata"))?;
            self.status.lock().unwrap().white_balance = Some(settings);
        }
        metadata["endedUtc"] = json!(Utc::now());
        metadata["exposure"] = serde_json::to_value(e)?;
        metadata["controls"] = serde_json::to_value(settings)?;
        metadata["backend"] = json!(if self.direct { "direct" } else { "sdk" });
        metadata["sdkFallback"] = json!(self.selection.direct && !self.direct);
        metadata["downloadRetries"] = json!(reads);
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
        settings: &mut BTreeMap<i32, i64>,
        token: &CancellationToken,
    ) -> Result<()> {
        let o = self.selection.recovery.clone();
        let clock = Instant::now();
        let mut stable = 0;
        let mut hold = CoolingHold::default();
        self.phase(format!("Restoring cooling near {prior:.1} C"));
        while clock.elapsed().as_secs_f64() < o.cooling_timeout_seconds {
            self.service_cooling_with(settings, token).await?;
            if settings.get(&17).copied().unwrap_or(0) == 0 {
                return Ok(());
            }
            let target = *settings.get(&16).unwrap_or(&0) as f64;
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
        self.cooling.retire();
        self.status.lock().unwrap().connected = false;
        let token = CancellationToken::new();
        let closed = if self.worker.is_some() {
            match self
                .call("close", Value::Null, Some(CLOSE_SECONDS), &token)
                .await
            {
                Ok(_) => true,
                Err(error) => {
                    let message = format!("Camera close or settings restoration failed: {error:#}");
                    self.emit("warning", "camera.cleanup_failed", &message);
                    self.status.lock().unwrap().error = Some(message);
                    false
                }
            }
        } else {
            false
        };
        self.invalidate().await;
        if !closed
            && self.ever_opened
            && self
                .snapshot()
                .controls
                .values()
                .any(|c| c.writable && matches!(c.kind, 17 | 21))
            && self.selection.serial.is_some()
        {
            let cleanup = async {
                self.delay(self.selection.recovery.reconnect_delay_seconds, &token)
                    .await?;
                self.open_worker(&token, OpenPurpose::ThermalShutdown)
                    .await?;
                // Backend close disables each supported thermal actuator and
                // attempts both even if one write fails. Never restore settings.
                self.call("close", Value::Null, Some(CLOSE_SECONDS), &token)
                    .await?;
                Result::<()>::Ok(())
            }
            .await;
            if let Err(error) = cleanup {
                self.emit(
                    "warning",
                    "cooling.cleanup_failed",
                    format!("Could not disable cooler/dew heater: {error:#}"),
                );
            }
            self.invalidate().await;
        }
        self.status.lock().unwrap().connected = false;
        self.cooling.retire();
        self.usb_target = None;
        self.phase("Disconnected");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cooling.retire();
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
        let directory = std::env::var_os("REGAIN_TEST_WORKERS")
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
    async fn exposing(status: &SharedStatus) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if status.lock().unwrap().phase == "Exposing" {
                return;
            }
            assert!(Instant::now() < deadline, "Capture never reached Exposing");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    #[tokio::test]
    async fn worker_evidence_age_survives_core_refresh_and_invalid_readback_is_uncertain() {
        let token = CancellationToken::new();
        let mut session = Session::new(selection(false), runtime(), log()).unwrap();
        session.connect(&token).await.unwrap();
        session.refresh(&token).await.unwrap();
        session
            .call(
                "simulation",
                json!({"observationControl":8,
            "observationReply":{"value":-100,"ageSeconds":120.25}}),
                None,
                &token,
            )
            .await
            .unwrap();
        let requested = Instant::now();
        session.read_environment(&token).await.unwrap();
        let received = Instant::now();
        let observed = session.snapshot().observations[&8];
        assert_eq!(observed.value, -100);
        // The internal request admission lies between these two boundaries.
        let age = Duration::from_secs_f64(120.25);
        assert!(observed.observed_at >= requested - age);
        assert!(observed.observed_at <= received - age);
        let previous = session.snapshot().observations[&0];
        session
            .call(
                "simulation",
                json!({"observationControl":0,
            "observationReply":{"value":123,"ageSeconds":-1}}),
                None,
                &token,
            )
            .await
            .unwrap();
        let error = session
            .set_imaging_control(0, 123, Instant::now() + Duration::from_secs(5), &token)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::UncertainControl { .. })
        ));
        assert!(!retryable(&error));
        assert_eq!(session.snapshot().observations[&0], previous);
        assert!(!session.snapshot().control_connection_available);
        assert!(session.worker.is_none());
        session.close().await;
    }
    #[tokio::test]
    async fn queued_persistent_control_invalid_observation_never_replays_or_starts_exposure() {
        for (kind, value) in [(0, 123), (5, 20), (16, -15), (17, 0)] {
            let token = CancellationToken::new();
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            let mut session = Session::new(
                selection(false),
                runtime(),
                Arc::new(move |_, event, _| sink.lock().unwrap().push(event.to_owned())),
            )
            .unwrap();
            session.connect(&token).await.unwrap();
            session.refresh(&token).await.unwrap();
            let previous = session.snapshot().observations[&kind];
            assert_ne!(
                previous.value, value,
                "The test must dispatch a changed setting"
            );
            let writes = events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.as_str() == "control.write_acknowledged")
                .count();
            session
                .call(
                    "simulation",
                    json!({"observationControl":kind,
                "observationReply":{"value":value,"ageSeconds":-1}}),
                    None,
                    &token,
                )
                .await
                .unwrap();
            Session::queue_control(&session.status, kind, value).unwrap();
            let error = session.capture(exposure(), &token).await.err().unwrap();
            assert!(matches!(
                error.downcast_ref::<Failure>(),
                Some(Failure::UncertainControl { .. })
            ));
            assert!(!retryable(&error));
            assert_eq!(session.snapshot().observations[&kind], previous);
            assert!(!session.snapshot().control_connection_available);
            assert!(session.worker.is_none());
            let observed = events.lock().unwrap().clone();
            assert_eq!(
                observed
                    .iter()
                    .filter(|event| event.as_str() == "control.write_acknowledged")
                    .count(),
                writes + 1
            );
            assert_eq!(
                observed
                    .iter()
                    .filter(|event| event.as_str() == "connection.opened")
                    .count(),
                1
            );
            assert!(!observed.iter().any(|event| event == "capture.retry"));
            assert_ne!(session.snapshot().phase, "Exposing");
            session.close().await;
        }
    }
    #[tokio::test]
    async fn idle_environment_read_never_applies_queued_settings_or_reopens_lost_worker() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut selected = selection(direct);
            if direct {
                selected.name = "ZWO ASI585MM Pro".into();
            }
            let mut session = Session::new(selected, runtime(), log()).unwrap();
            session.connect(&token).await.unwrap();
            session.refresh(&token).await.unwrap();
            let before = session.snapshot();
            Session::queue_control(&session.status, 0, 234).unwrap();
            Session::queue_control(&session.status, 16, -20).unwrap();
            session.refresh_environment(&token).await.unwrap();
            let after = session.snapshot();
            assert_eq!(after.process_id, before.process_id);
            for kind in [0, 5, 16, 17] {
                assert_eq!(after.observations[&kind], before.observations[&kind]);
                assert_eq!(
                    session
                        .call("get", json!({"control":kind}), None, &token)
                        .await
                        .unwrap()
                        .0,
                    before.observations[&kind].value
                );
            }
            assert_eq!(after.values[&0], 234);
            assert_eq!(after.values[&16], -20);
            for kind in [8, 15] {
                assert!(
                    after.observations[&kind].observed_at > before.observations[&kind].observed_at
                );
            }
            session.invalidate().await;
            assert!(session.refresh_environment(&token).await.is_err());
            assert!(session.worker.is_none());
            assert!(session.snapshot().process_id.is_none());
            assert_eq!(session.snapshot().observations, after.observations);
            session.close().await;
        }
    }
    #[tokio::test]
    async fn failed_idle_environment_read_retires_worker_without_reopening_or_applying_settings() {
        let token = CancellationToken::new();
        let mut session = Session::new(selection(false), runtime(), log()).unwrap();
        session.connect(&token).await.unwrap();
        session.refresh(&token).await.unwrap();
        let before = session.snapshot();
        Session::queue_control(&session.status, 0, 234).unwrap();
        session
            .call(
                "simulation",
                json!({"observationControl":8,
            "observationReply":{"value":100,"ageSeconds":-1}}),
                None,
                &token,
            )
            .await
            .unwrap();
        assert!(session.refresh_environment(&token).await.is_err());
        assert!(!session.snapshot().control_connection_available);
        assert!(session.worker.is_none());
        assert_eq!(session.snapshot().observations, before.observations);
        assert_eq!(session.snapshot().values[&0], 234);
        assert!(session.refresh_environment(&token).await.is_err());
        assert!(session.worker.is_none());
        assert_eq!(session.snapshot().observations, before.observations);
        session.close().await;
    }
    #[tokio::test]
    async fn observations_are_acknowledged_evidence_not_desired_settings_or_new_worker_defaults() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut selected = selection(direct);
            if direct {
                selected.name = "ZWO ASI585MM Pro".into();
            }
            let mut session = Session::new(selected, runtime(), log()).unwrap();
            session.connect(&token).await.unwrap();
            assert!(session.snapshot().observations.is_empty());
            Session::queue_control(&session.status, 0, 123).unwrap();
            assert!(session.snapshot().observations.is_empty());
            session.refresh(&token).await.unwrap();
            let observed = session.snapshot().observations;
            for kind in [0, 5, 8, 15, 16, 17] {
                assert!(observed[&kind].observed_at <= Instant::now());
            }
            assert_eq!(observed[&0].value, 123);
            Session::queue_control(&session.status, 0, 234).unwrap();
            let queued = session.snapshot();
            assert_eq!(queued.values[&0], 234);
            assert_eq!(queued.observations[&0], observed[&0]);
            let json = serde_json::to_value(&queued).unwrap();
            assert!(json.get("observations").is_none());
            assert!(json.to_string().find("observedAt").is_none());
            session.refresh(&token).await.unwrap();
            let acknowledged = session.snapshot().observations[&0];
            assert_eq!(acknowledged.value, 234);
            assert!(acknowledged.observed_at >= observed[&0].observed_at);
            session.invalidate().await;
            // Diagnostics can retain known aged evidence while unavailable.
            assert_eq!(session.snapshot().observations[&0], acknowledged);
            session.open(&token).await.unwrap();
            assert!(session.snapshot().observations.is_empty());
            assert_eq!(session.snapshot().values[&0], 234);
            session.refresh(&token).await.unwrap();
            assert_eq!(session.snapshot().observations[&0].value, 234);
            session.close().await;
        }
    }
    #[tokio::test]
    async fn acknowledged_imaging_controls_apply_before_success_and_survive_worker_recovery() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut session = Session::new(selection(direct), runtime(), log()).unwrap();
            session.connect(&token).await.unwrap();
            session.refresh(&token).await.unwrap();
            for (kind, value) in [(0, 123), (5, 20)] {
                assert_eq!(
                    session
                        .set_imaging_control(
                            kind,
                            value,
                            Instant::now() + Duration::from_secs(5),
                            &token
                        )
                        .await
                        .unwrap(),
                    value
                );
                assert_eq!(session.snapshot().values[&kind], value);
                let observation = session.snapshot().observations[&kind];
                assert_eq!(observation.value, value);
                assert!(observation.observed_at <= Instant::now());
                assert_eq!(session.applied[&kind], value);
                assert_eq!(
                    session
                        .call("get", json!({"control":kind}), None, &token)
                        .await
                        .unwrap()
                        .0,
                    value
                );
            }
            session.invalidate().await;
            let frame = session.capture(exposure(), &token).await.unwrap();
            assert_eq!(frame.metadata["controls"]["0"], 123);
            assert_eq!(frame.metadata["controls"]["5"], 20);
            for (kind, value) in [(0, 123), (5, 20)] {
                assert_eq!(
                    session
                        .call("get", json!({"control":kind}), None, &token)
                        .await
                        .unwrap()
                        .0,
                    value
                );
            }
            session.close().await;
        }
    }
    #[tokio::test]
    async fn imaging_control_mismatch_retires_worker_without_publishing_or_retrying() {
        let token = CancellationToken::new();
        let mut rt = runtime();
        rt.sdk_simulation = Some(json!({"instant":true,"clampControl":0,"clampMinimum":200}));
        let mut session = Session::new(selection(false), rt, log()).unwrap();
        session.connect(&token).await.unwrap();
        let before = session.snapshot().values[&0];
        let error = session
            .set_imaging_control(0, 100, Instant::now() + Duration::from_secs(5), &token)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::UncertainControl { .. })
        ));
        assert!(!retryable(&error));
        assert_eq!(session.snapshot().values[&0], before);
        assert!(!session.snapshot().control_connection_available);
        assert!(session.worker.is_none());
        assert!(session.applied.is_empty());
        session.close().await;
    }
    #[tokio::test]
    async fn acknowledged_sdk_offset_preserves_existing_bounded_clamp_policy() {
        let token = CancellationToken::new();
        let mut rt = runtime();
        rt.sdk_simulation = Some(json!({"instant":true,"clampControl":5,"clampMinimum":20}));
        let mut session = Session::new(selection(false), rt, log()).unwrap();
        session.connect(&token).await.unwrap();
        assert_eq!(
            session
                .set_imaging_control(5, 0, Instant::now() + Duration::from_secs(5), &token)
                .await
                .unwrap(),
            20
        );
        assert_eq!(session.snapshot().values[&5], 20);
        assert_eq!(session.applied[&5], 20);
        let frame = session.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["controls"]["5"], 20);
        let maximum = session.snapshot().controls[&5].max;
        session
            .call(
                "simulation",
                json!({"clampControl":5,"clampMinimum":maximum+1}),
                None,
                &token,
            )
            .await
            .unwrap();
        let error = session
            .set_imaging_control(5, 0, Instant::now() + Duration::from_secs(5), &token)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::UncertainControl { .. })
        ));
        assert_eq!(session.snapshot().values[&5], 20);
        assert!(session.worker.is_none());
        session.close().await;
    }
    #[tokio::test]
    async fn imaging_control_preflight_rejects_without_io_or_desired_state_changes() {
        let token = CancellationToken::new();
        let mut session = Session::new(selection(false), runtime(), log()).unwrap();
        session.connect(&token).await.unwrap();
        session.refresh(&token).await.unwrap();
        let before = session.snapshot();
        let applied = session.applied.clone();
        for (kind, value) in [(1, 1000), (16, -10), (0, before.controls[&0].max + 1)] {
            let error = session
                .set_imaging_control(kind, value, Instant::now() + Duration::from_secs(5), &token)
                .await
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<Failure>(),
                Some(Failure::Invalid(_))
            ));
        }
        for malformed in 0..4 {
            {
                let mut state = session.status.lock().unwrap();
                let cap = state.controls.get_mut(&0).unwrap();
                match malformed {
                    0 => cap.kind = 5,
                    1 => cap.min = cap.max + 1,
                    2 => cap.writable = false,
                    _ => {
                        state.controls.remove(&0);
                    }
                }
            }
            assert!(
                session
                    .set_imaging_control(0, 123, Instant::now() + Duration::from_secs(5), &token)
                    .await
                    .is_err()
            );
            session
                .status
                .lock()
                .unwrap()
                .controls
                .insert(0, before.controls[&0].clone());
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = session
            .set_imaging_control(0, 123, Instant::now() + Duration::from_secs(5), &cancelled)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::Cancelled)
        ));
        let error = session
            .set_imaging_control(0, 123, Instant::now(), &token)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<cooling::CoolingError>(),
            Some(cooling::CoolingError::Expired)
        ));
        let cooler = session
            .cooling()
            .submit(16, -15, Duration::from_secs(5))
            .unwrap();
        assert!(
            session
                .set_imaging_control(0, 123, Instant::now() + Duration::from_secs(5), &token)
                .await
                .is_err()
        );
        drop(cooler);
        assert_eq!(session.snapshot().values, before.values);
        assert_eq!(session.applied, applied);
        assert_eq!(session.snapshot().process_id, before.process_id);
        for kind in [0, 5] {
            assert_eq!(
                session
                    .call("get", json!({"control":kind}), None, &token)
                    .await
                    .unwrap()
                    .0,
                before.values[&kind]
            );
        }
        session.close().await;
    }
    async fn acknowledged_cooling(direct: bool) {
        let token = CancellationToken::new();
        let mut sel = selection(direct);
        if direct {
            sel.name = "ZWO ASI585MM Pro".into();
        }
        sel.recovery.ready_frame_download_retries = 0;
        let mut rt = runtime();
        rt.sdk_simulation = Some(json!({"instant":false}));
        let mut session = Session::new(sel, rt, log()).unwrap();
        session.connect(&token).await.unwrap();
        let handle = session.cooling();
        let shared = session.status.clone();
        let original = shared.lock().unwrap().values[&16];
        Session::queue_control(&shared, 0, 123).unwrap();
        if !direct {
            session
                .call("fault", json!({"kind":"download"}), None, &token)
                .await
                .unwrap();
        }
        let capture = session.capture(
            Exposure {
                microseconds: 6_000_000,
                ..exposure()
            },
            &token,
        );
        let commands = async {
            exposing(&shared).await;
            let first = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
            assert_eq!(shared.lock().unwrap().values[&16], original);
            assert_eq!(first.wait().await, Ok(-10));
            for (kind, value) in [(17, 1), (16, -15)] {
                assert_eq!(
                    handle
                        .submit(kind, value, Duration::from_secs(5))
                        .unwrap()
                        .wait()
                        .await,
                    Ok(value)
                );
            }
            drop(handle.submit(16, -20, Duration::from_secs(5)).unwrap());
            // Legacy deferred imaging intent must not change frozen capture or
            // replacement settings. Only acknowledged cooler keys are live.
            Session::queue_control(&shared, 0, 200).unwrap();
        };
        let (frame, ()) = tokio::join!(capture, commands);
        let frame = frame.unwrap();
        assert_eq!(frame.metadata["controls"]["16"], -15);
        assert_eq!(frame.metadata["controls"]["17"], 1);
        assert_eq!(frame.metadata["controls"]["0"], 123);
        assert_eq!(frame.metadata["recoveries"], if direct { 0 } else { 1 });
        assert_eq!(
            frame.pixels.as_ref(),
            (0..4096u16).flat_map(u16::to_le_bytes).collect::<Vec<_>>()
        );
        assert_eq!(session.snapshot().values[&16], -15);
        assert_eq!(session.snapshot().values[&17], 1);
        assert!(!handle.pending());
        session.invalidate().await;
        // A later capture restores the acknowledged target after worker loss.
        let next = session.capture(exposure(), &token).await.unwrap();
        assert_eq!(next.metadata["controls"]["16"], -15);
        assert_eq!(next.metadata["controls"]["17"], 1);
        assert_eq!(next.metadata["controls"]["0"], 200);
        session.close().await;
    }
    #[tokio::test]
    async fn acknowledged_sdk_cooling_survives_replacement_and_preserves_imaging_settings() {
        acknowledged_cooling(false).await;
    }
    #[tokio::test]
    async fn acknowledged_direct_cooling_survives_worker_recovery() {
        acknowledged_cooling(true).await;
    }
    #[tokio::test]
    async fn cooler_readback_mismatch_stops_capture_without_retry_or_target_publication() {
        let token = CancellationToken::new();
        let mut rt = runtime();
        rt.sdk_simulation = Some(json!({"instant":false}));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let mut session = Session::new(
            selection(false),
            rt,
            Arc::new(move |_, event, _| sink.lock().unwrap().push(event.to_owned())),
        )
        .unwrap();
        session.connect(&token).await.unwrap();
        session.refresh(&token).await.unwrap();
        session
            .call(
                "simulation",
                json!({"clampControl":16,"clampMinimum":5}),
                None,
                &token,
            )
            .await
            .unwrap();
        let handle = session.cooling();
        let shared = session.status.clone();
        let original = shared.lock().unwrap().values[&16];
        let capture = session.capture(
            Exposure {
                microseconds: 6_000_000,
                ..exposure()
            },
            &token,
        );
        let commands = async {
            exposing(&shared).await;
            assert!(matches!(
                handle
                    .submit(16, -10, Duration::from_secs(5))
                    .unwrap()
                    .wait()
                    .await,
                Err(cooling::CoolingError::Uncertain { .. })
            ));
        };
        let (result, ()) = tokio::join!(capture, commands);
        let error = result.err().unwrap();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::UncertainControl { .. })
        ));
        assert!(!retryable(&error));
        assert_eq!(session.snapshot().values[&16], original);
        assert_eq!(session.snapshot().observations[&16].value, original);
        assert!(!session.snapshot().control_connection_available);
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "capture.retry")
        );
        assert!(matches!(
            handle.submit(17, 1, Duration::from_secs(1)),
            Err(cooling::CoolingError::Unavailable)
        ));
        session.close().await;
    }
    #[tokio::test]
    async fn idle_cooling_acknowledges_and_teardown_rejects_queued_requests() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut sel = selection(direct);
            if direct {
                sel.name = "ZWO ASI585MM Pro".into();
            }
            let mut session = Session::new(sel, runtime(), log()).unwrap();
            session.connect(&token).await.unwrap();
            let handle = session.cooling();
            let receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
            session.service_cooling(&token).await.unwrap();
            assert_eq!(receipt.wait().await, Ok(-10));
            assert_eq!(session.snapshot().values[&16], -10);
            let receipt = handle.submit(16, -20, Duration::from_secs(5)).unwrap();
            session.close().await;
            assert_eq!(
                receipt.wait().await,
                Err(cooling::CoolingError::Unavailable)
            );
            assert!(!handle.pending());
        }
    }
    #[tokio::test]
    async fn live_cooler_target_and_disable_are_acknowledged_during_recovery_settle() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut sel = selection(direct);
            if direct {
                sel.name = "ZWO ASI585MM Pro".into();
            }
            let mut session = Session::new(sel, runtime(), log()).unwrap();
            session.connect(&token).await.unwrap();
            let handle = session.cooling();
            let shared = session.status.clone();
            let enabled = handle.submit(17, 1, Duration::from_secs(5)).unwrap();
            session.service_cooling(&token).await.unwrap();
            assert_eq!(enabled.wait().await, Ok(1));
            // Neither simulator meets this prior temperature/output, so settle
            // must remain active until the caller explicitly disables cooling.
            session.seed_recovery(Some(-30.), Some(80));
            let capture = session.capture(exposure(), &token);
            let commands = async {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if shared
                        .lock()
                        .unwrap()
                        .phase
                        .starts_with("Restoring cooling")
                    {
                        break;
                    }
                    assert!(Instant::now() < deadline);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                for (kind, value) in [(16, -15), (17, 0)] {
                    assert_eq!(
                        handle
                            .submit(kind, value, Duration::from_secs(5))
                            .unwrap()
                            .wait()
                            .await,
                        Ok(value)
                    );
                }
            };
            let (frame, ()) = tokio::join!(capture, commands);
            let frame = frame.unwrap();
            assert_eq!(frame.metadata["controls"]["16"], -15);
            assert_eq!(frame.metadata["controls"]["17"], 0);
            assert_eq!(frame.metadata["recoveries"], 0);
            assert!(!handle.pending());
            session.close().await;
        }
    }
    #[tokio::test]
    async fn direct_usb_read_size_survives_worker_replacement_and_leaves_sdk_bandwidth_unchanged() {
        let token = CancellationToken::new();
        let mut sel = selection(true);
        sel.recovery.direct_read_chunk_kib = 64;
        let mut s = Session::new(sel, runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        assert!(!s.snapshot().controls[&6].writable);
        assert_eq!(s.snapshot().values[&6], 40);
        assert_eq!(
            s.capture(exposure(), &token).await.unwrap().metadata["readChunkKiB"],
            64
        );
        s.call("simulate-read-failures", json!({"count":3}), None, &token)
            .await
            .unwrap();
        let frame = s.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["recoveries"], 1);
        assert_eq!(frame.metadata["readChunkKiB"], 64);
        s.close().await;
    }
    #[test]
    fn direct_usb_read_size_configuration_preserves_key_default_and_bounds() {
        let old: RecoveryOptions = serde_json::from_value(json!({"directReadRetries":1})).unwrap();
        assert_eq!(old.direct_read_chunk_kib, 1024);
        let current: RecoveryOptions =
            serde_json::from_value(json!({"directReadChunkKiB":64})).unwrap();
        assert_eq!(current.direct_read_chunk_kib, 64);
        assert_eq!(
            serde_json::to_value(current).unwrap()["directReadChunkKiB"],
            64
        );
        for value in [0, 3, 512, 1024, 2048, u32::MAX] {
            let options = RecoveryOptions {
                direct_read_chunk_kib: value,
                ..RecoveryOptions::default()
            };
            assert_eq!(options.validate().is_ok(), matches!(value, 512 | 1024));
        }
    }
    #[tokio::test]
    async fn cooled_worker_recovery_restores_output_once_and_honors_warming_or_disable() {
        let token = CancellationToken::new();
        let mut sel = selection(true);
        sel.name = "ZWO ASI585MM Pro".into();
        let mut s = Session::new(sel, runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        Session::queue_control(&s.status, 16, 10).unwrap();
        Session::queue_control(&s.status, 17, 1).unwrap();
        s.refresh(&token).await.unwrap();
        s.call(
            "resume-cooling",
            json!({"power":40,"temperature":25.0,"previousTarget":10}),
            None,
            &token,
        )
        .await
        .unwrap();
        s.refresh(&token).await.unwrap();
        assert_eq!(s.snapshot().values[&15], 40);
        s.call("simulate-read-failures", json!({"count":3}), None, &token)
            .await
            .unwrap();
        let frame = s.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["recoveries"], 1);
        assert_eq!(s.snapshot().values[&15], 40);
        assert!(s.cooling_seeded);
        // Ordinary refreshes must not repeatedly reseed the regulator.
        s.call(
            "resume-cooling",
            json!({"power":30,"temperature":25.0,"previousTarget":10}),
            None,
            &token,
        )
        .await
        .unwrap();
        s.refresh(&token).await.unwrap();
        assert_eq!(s.snapshot().values[&15], 30);
        s.invalidate().await;
        Session::queue_control(&s.status, 16, 11).unwrap();
        s.refresh(&token).await.unwrap();
        assert_eq!(s.snapshot().values[&15], 0); // A warmer requested target wins.
        s.invalidate().await;
        Session::queue_control(&s.status, 17, 0).unwrap();
        s.refresh(&token).await.unwrap();
        assert_eq!(s.snapshot().values[&17], 0);
        assert!(!s.cooling_seeded);
        s.close().await;
    }

    #[tokio::test]
    async fn managed_white_balance_survives_worker_recovery_and_retains_locked_gains() {
        use crate::white_balance::{Gains, Mode, Output, Settings};
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut s = Session::new(selection(direct), runtime(), log()).unwrap();
            s.connect(&token).await.unwrap();
            assert_eq!(s.snapshot().white_balance_capabilities["supported"], true);
            let settings = Settings {
                mode: Mode::Manual,
                gains: Gains { red: 2., blue: 0.5 },
                output: Output::Corrected,
            };
            s.set_white_balance(settings, &token).await.unwrap();
            s.set_white_balance(
                Settings {
                    mode: Mode::Locked,
                    ..settings
                },
                &token,
            )
            .await
            .unwrap();
            assert!(Session::queue_control(&s.status, 3, 50).is_err());
            let e = Exposure {
                dark: false,
                ..exposure()
            };
            let first = s.capture(e.clone(), &token).await.unwrap();
            assert_eq!(first.metadata["whiteBalance"]["applied"], true);
            s.invalidate().await;
            let recovered = s.capture(e, &token).await.unwrap();
            assert_eq!(first.pixels, recovered.pixels);
            assert_eq!(
                s.snapshot().white_balance.unwrap(),
                Settings {
                    mode: Mode::Locked,
                    ..settings
                }
            );
            s.set_white_balance(
                Settings {
                    mode: Mode::Once,
                    ..Settings::default()
                },
                &token,
            )
            .await
            .unwrap();
            s.capture(
                Exposure {
                    dark: false,
                    ..exposure()
                },
                &token,
            )
            .await
            .unwrap();
            let effective = s.snapshot().white_balance.unwrap();
            assert_eq!(effective.mode, Mode::Locked);
            s.invalidate().await;
            s.capture(exposure(), &token).await.unwrap();
            assert_eq!(s.snapshot().white_balance.unwrap(), effective);
            s.invalidate().await;
        }
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
    async fn direct_worker_acknowledges_capture_cooling_over_production_framing() {
        // Transport primitive only: Session's common acknowledged control queue
        // is a separate requirement. Never discover or activate physical cameras.
        for mode in ["still", "video"] {
            let token = CancellationToken::new();
            let mut worker = runtime().spawn(true, log()).await.unwrap();
            worker
                .call("open", json!({"name":"ZWO ASI585MM Pro"}), 15., &token)
                .await
                .unwrap();
            worker
                .call(
                    "start",
                    json!({"mode":mode,"maxFps":120.0,"width":64,"height":64,
                "x":0,"y":0,"bin":1,"microseconds":6_000_000,"dark":false}),
                    15.,
                    &token,
                )
                .await
                .unwrap();
            for (control, value) in [(16, -10), (17, 1), (16, -15)] {
                worker
                    .call("set", json!({"control":control,"value":value}), 15., &token)
                    .await
                    .unwrap();
                assert_eq!(
                    worker
                        .call("get", json!({"control":control}), 15., &token)
                        .await
                        .unwrap()
                        .0,
                    value
                );
                assert_eq!(
                    worker
                        .call("status", Value::Null, 15., &token)
                        .await
                        .unwrap()
                        .0,
                    1
                );
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            while worker
                .call("status", Value::Null, 15., &token)
                .await
                .unwrap()
                .0
                == 1
            {
                assert!(Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let (metadata, pixels) = worker
                .call_image("download", Value::Null, 15., &token, 8192)
                .await
                .unwrap();
            assert_eq!(metadata["mode"], mode);
            assert_eq!(
                pixels,
                (0..4096u16).flat_map(u16::to_le_bytes).collect::<Vec<_>>()
            );
            worker
                .call("close", Value::Null, 15., &token)
                .await
                .unwrap();
            worker.kill().await;
        }
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
    async fn usb_escalation_is_opt_in_bounded_and_obeys_exposure_limits() {
        for (threshold, retries, seconds, resets) in [
            (0, 3, 0.01, 0),
            (2, 3, 0.01, 1),
            (1, 0, 0.01, 0),
            (1, 3, 31., 0),
            (4, 3, 0.01, 0),
        ] {
            let events = Arc::new(Mutex::new(Vec::<String>::new()));
            let sink = events.clone();
            let mut sel = selection(false);
            sel.recovery.usb_reset_after_failures = threshold;
            sel.recovery.max_retries = retries;
            sel.recovery.ready_frame_download_retries = 0;
            let mut rt = runtime();
            rt.sdk_simulation = Some(json!({"instant":true,"fault":"download"}));
            let mut session = Session::new(
                sel,
                rt,
                Arc::new(move |_, event, _| sink.lock().unwrap().push(event.into())),
            )
            .unwrap();
            let token = CancellationToken::new();
            session.connect(&token).await.unwrap();
            assert!(
                session
                    .capture(
                        Exposure {
                            microseconds: (seconds * 1e6) as u64,
                            ..exposure()
                        },
                        &token
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| e.as_str() == "usb.reset")
                    .count(),
                resets
            );
            session.close().await;
        }
    }
    #[tokio::test]
    async fn usb_recovery_restores_controls_and_cancellation_never_resets() {
        let token = CancellationToken::new();
        let mut sel = selection(false);
        sel.recovery.usb_reset_after_failures = 1;
        sel.recovery.ready_frame_download_retries = 0;
        let mut session = Session::new(sel, runtime(), log()).unwrap();
        session.connect(&token).await.unwrap();
        Session::queue_control(&session.status, 0, 230).unwrap();
        session
            .call("fault", json!({"kind":"download"}), None, &token)
            .await
            .unwrap();
        let frame = session.capture(exposure(), &token).await.unwrap();
        assert_eq!(frame.metadata["usbResets"], 1);
        assert_eq!(frame.metadata["recoveries"], 1);
        assert_eq!(frame.metadata["controls"]["0"], 230);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            session
                .capture(exposure(), &cancelled)
                .await
                .err()
                .unwrap()
                .is::<Failure>()
        );
        session.close().await;
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
        let status = s.snapshot();
        assert_eq!(status.retry.downloads, 1);
        assert_eq!(status.retry.recaptures, 0);
        assert!(status.recovery_info().contains("state: Idle; retries: 1"));
        assert!(status.retry.last_failure.as_deref().unwrap().contains("11"));
        s.capture(exposure(), &token).await.unwrap();
        assert_eq!(s.snapshot().retry.downloads, 0);
        assert_eq!(s.snapshot().retry.last_failure, status.retry.last_failure);
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
                assert_eq!(s.snapshot().retry.recaptures, 1);
                assert!(
                    s.snapshot()
                        .recovery_info()
                        .contains("state: Idle; retries: 1")
                );
            } else {
                assert_eq!(s.snapshot().retry.recaptures, 0);
                assert!(
                    s.snapshot()
                        .recovery_info()
                        .contains("state: Error; retries: 0")
                );
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
        assert_eq!(s.snapshot().retry.usb_reads, 2);
        assert!(
            s.snapshot()
                .recovery_info()
                .contains("state: Idle; retries: 2")
        );
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
    async fn intentional_abort_keeps_worker_cooling_and_does_not_retry() {
        for direct in [false, true] {
            let token = CancellationToken::new();
            let mut sel = selection(direct);
            if direct {
                sel.name = "ZWO ASI6200MM Pro".into();
            }
            let mut rt = runtime();
            rt.sdk_simulation = Some(json!({"instant":false,"temperature":250,"coolerPower":70}));
            let events = Arc::new(Mutex::new(Vec::new()));
            let recorded = events.clone();
            let mut s = Session::new(
                sel,
                rt,
                Arc::new(move |_, event, _| recorded.lock().unwrap().push(event.to_owned())),
            )
            .unwrap();
            s.connect(&token).await.unwrap();
            Session::queue_control(&s.status, 16, 25).unwrap();
            Session::queue_control(&s.status, 17, 1).unwrap();
            s.refresh(&token).await.unwrap();
            if direct {
                s.call(
                    "resume-cooling",
                    json!({"power":70,"temperature":25.,"previousTarget":25}),
                    None,
                    &token,
                )
                .await
                .unwrap();
                s.refresh(&token).await.unwrap();
            }
            let pid = s.snapshot().process_id;
            let abort = CancellationToken::new();
            let cancel = abort.clone();
            let status = s.status.clone();
            let cancellation = tokio::spawn(async move {
                exposing(&status).await;
                cancel.cancel();
            });
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                s.capture(
                    Exposure {
                        microseconds: 600_000_000,
                        ..exposure()
                    },
                    &abort,
                ),
            )
            .await
            .unwrap();
            cancellation.await.unwrap();
            assert!(matches!(
                result.err().unwrap().downcast_ref::<Failure>(),
                Some(Failure::Cancelled)
            ));
            assert_eq!(s.snapshot().process_id, pid);
            assert!(s.snapshot().control_connection_available);
            assert_eq!(s.snapshot().values[&17], 1);
            assert_eq!(s.snapshot().values[&15], 70);
            assert!(s.snapshot().retry.last_failure.is_none());
            assert_eq!(
                s.capture(exposure(), &token).await.unwrap().metadata["recoveries"],
                0
            );
            assert_eq!(s.snapshot().process_id, pid);
            assert!(
                !events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e == "capture.failed" || e == "capture.retry")
            );
            s.close().await;
        }
    }
    #[tokio::test]
    async fn thermal_cleanup_does_not_restore_white_balance_or_reactivate_controls() {
        let token = CancellationToken::new();
        let mut s = Session::new(selection(false), runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        s.set_white_balance(
            crate::white_balance::Settings {
                mode: crate::white_balance::Mode::Manual,
                ..Default::default()
            },
            &token,
        )
        .await
        .unwrap();
        s.invalidate().await;
        s.status.lock().unwrap().connected = false;
        s.open_worker(&token, OpenPurpose::ThermalShutdown)
            .await
            .unwrap();
        assert!(!s.snapshot().connected);
        assert!(!s.snapshot().control_connection_available);
        assert_eq!(s.snapshot().phase, "Closing camera");
        assert!(s.cooling().submit(17, 1, Duration::from_secs(1)).is_err());
        let actual = s
            .call("white-balance", Value::Null, None, &token)
            .await
            .unwrap()
            .0;
        assert_eq!(actual["managed"], false);
        s.close().await;
    }
    #[tokio::test]
    async fn failed_abort_retires_worker_before_another_capture() {
        let token = CancellationToken::new();
        let mut s = Session::new(selection(true), runtime(), log()).unwrap();
        s.connect(&token).await.unwrap();
        let pid = s.snapshot().process_id;
        s.call("simulation", json!({"cleanupFailure":true}), None, &token)
            .await
            .unwrap();
        let abort = CancellationToken::new();
        let cancel = abort.clone();
        let status = s.status.clone();
        let cancellation = tokio::spawn(async move {
            exposing(&status).await;
            cancel.cancel();
        });
        let result = s
            .capture(
                Exposure {
                    microseconds: 600_000_000,
                    ..exposure()
                },
                &abort,
            )
            .await;
        cancellation.await.unwrap();
        assert!(matches!(
            result.err().unwrap().downcast_ref::<Failure>(),
            Some(Failure::Cancelled)
        ));
        assert!(!s.snapshot().control_connection_available);
        assert_eq!(s.snapshot().process_id, None);
        assert!(
            s.snapshot()
                .retry
                .last_failure
                .unwrap()
                .contains("Stop was not acknowledged")
        );
        s.capture(exposure(), &token).await.unwrap();
        assert_ne!(s.snapshot().process_id, pid);
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
        assert_eq!(frame.metadata["retainedReadRetries"], 2);
        assert_eq!(frame.metadata["readRecoveries"].as_u64().unwrap_or(0), 0);
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
