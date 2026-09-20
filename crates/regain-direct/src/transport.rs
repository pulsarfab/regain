//! Shared camera operations. Only the USB transport depends on the OS.
use crate::transfer::{Budget, Failure};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    time::Duration,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use native as platform;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("PulsarFab regain supports Windows, Linux, and macOS");

pub use platform::{DeviceInfo, enumerate, require_sdk_absent};

pub type Telemetry = std::sync::Arc<std::sync::Mutex<Option<[i64; 2]>>>;

pub struct Camera {
    device: RefCell<Option<platform::Device>>,
    environment: RefCell<Option<crate::environment::Environment>>,
    telemetry: Telemetry,
    identity: DeviceInfo,
    transfer_timeout: Cell<Duration>,
    phase: Cell<&'static str>,
}
impl Camera {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Ok(Self {
            device: RefCell::new(Some(platform::Device::open(info)?)),
            environment: RefCell::new(None),
            telemetry: Telemetry::default(),
            identity: info.clone(),
            transfer_timeout: Cell::new(Duration::from_secs(60)),
            phase: Cell::new("idle"),
        })
    }
    pub fn enable_environment(&self, auxiliary: bool) -> Result<()> {
        *self.environment.borrow_mut() =
            Some(crate::environment::Environment::open(self, auxiliary)?);
        self.publish_environment()?;
        Ok(())
    }
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry.clone()
    }
    fn publish_environment(&self) -> Result<()> {
        if let Some(environment) = self.environment.borrow().as_ref() {
            *self.telemetry.lock().unwrap() = Some([environment.get(8)?, environment.get(15)?]);
        }
        Ok(())
    }
    pub fn has_environment(&self) -> bool {
        self.environment.borrow().is_some()
    }
    pub fn service_environment(&self) -> Result<()> {
        if let Some(environment) = self.environment.borrow_mut().as_mut() {
            environment.service(self)?;
        }
        self.publish_environment()?;
        Ok(())
    }
    pub fn environment_control(&self, control: u32, value: Option<i64>) -> Result<i64> {
        let mut state = self.environment.borrow_mut();
        let environment = state
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("environment unavailable"))?;
        if let Some(value) = value {
            environment.set(self, control, value)?;
        }
        environment.service(self)?;
        let result = environment.get(control);
        drop(state);
        self.publish_environment()?;
        result
    }

    pub fn vendor(&self, request: u8, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        self.device
            .borrow()
            .as_ref()
            .context("USB handle is closed")?
            .vendor(request, value, index, length)
    }
    pub fn reset_pipe(&self) -> Result<()> {
        self.device
            .borrow()
            .as_ref()
            .context("USB handle is closed")?
            .reset_pipe()
    }
    #[cfg(windows)]
    pub fn research_port_operation(&self, cycle: bool) -> Result<()> {
        self.device
            .borrow()
            .as_ref()
            .context("USB handle is closed")?
            .port_operation(cycle)
    }
    pub fn probe(&self) -> Result<Value> {
        self.device
            .borrow()
            .as_ref()
            .context("USB handle is closed")?
            .probe()
    }
    pub fn cancel_read(&self) -> Result<Value> {
        self.device
            .borrow()
            .as_ref()
            .context("USB handle is closed")?
            .cancel_read()
    }
    pub fn transfer_timeout(&self, seconds: f64) -> Result<()> {
        ensure!(
            seconds.is_finite() && seconds > 0.0 && seconds <= 3600.0,
            "invalid transfer deadline"
        );
        self.transfer_timeout.set(Duration::from_secs_f64(seconds));
        Ok(())
    }
    pub fn phase(&self, phase: &'static str) {
        let previous = self.phase.replace(phase);
        crate::diagnostics::details(
            "debug",
            "capture.phase",
            format_args!("{previous} -> {phase}"),
            serde_json::json!({"previous":previous,"phase":phase}),
        );
    }
    /// Research only: same enumerated device, no sensor/FPGA initialization.
    /// Every request is synchronous here and has drained before it returns.
    pub fn reopen_retained(&self, delay: Duration) -> Result<()> {
        let serial = self.vendor(0xc8, 0, 0, 8)?;
        self.reopen_same_camera(&serial, delay)
    }
    /// All requests have completed or drained before reaching this boundary.
    /// Re-enumerate the original interface, then verify the pre-exposure serial
    /// before allowing any register write. Never pick another enumeration index.
    pub fn reopen_same_camera(&self, serial: &[u8], delay: Duration) -> Result<()> {
        ensure!(
            serial.len() == 8 && serial.iter().any(|&b| b != 0),
            "retained reopen requires camera identity"
        );
        self.phase("reopening_retained");
        drop(self.device.borrow_mut().take());
        std::thread::sleep(delay);
        let mut candidates = enumerate()?
            .into_iter()
            .filter(|info| info.same_interface(&self.identity));
        let current = candidates
            .next()
            .context("original camera interface has not returned")?;
        ensure!(candidates.next().is_none(), "ambiguous camera interface");
        *self.device.borrow_mut() = Some(platform::Device::open(&current)?);
        let observed = self.vendor(0xc8, 0, 0, 8);
        if !observed.as_ref().is_ok_and(|value| value == serial) {
            drop(self.device.borrow_mut().take());
            observed?;
            anyhow::bail!("camera identity changed during retained reopen");
        }
        if let Some(environment) = self.environment.borrow_mut().as_mut() {
            environment.restore(self)?;
        }
        self.publish_environment()?;
        Ok(())
    }
    pub fn read_frame(&self, length: usize) -> Result<Vec<u8>> {
        self.read_frame_wait(length, 5000)
    }
    pub fn read_frame_wait(&self, length: usize, first_timeout_ms: u32) -> Result<Vec<u8>> {
        self.read_frame_inner(length, first_timeout_ms, None)
    }
    pub fn read_frame_checked(
        &self,
        length: usize,
        first_timeout_ms: u32,
        continuity: &mut crate::transfer::Continuity,
    ) -> Result<Vec<u8>> {
        self.read_frame_inner(length, first_timeout_ms, Some(continuity))
    }
    fn read_frame_inner(
        &self,
        length: usize,
        first_timeout_ms: u32,
        mut continuity: Option<&mut crate::transfer::Continuity>,
    ) -> Result<Vec<u8>> {
        ensure!(
            (5000..=15000).contains(&first_timeout_ms),
            "invalid frame wait"
        );
        ensure!(
            length > 0 && length <= 128 * 1024 * 1024,
            "invalid frame size"
        );
        let mut data = vec![0; length];
        let budget = Budget::new(self.transfer_timeout.get());
        self.phase("downloading");
        for (number, chunk) in data.chunks_mut(1024 * 1024).enumerate() {
            self.service_environment()?;
            let requested_timeout = if number == 0 { first_timeout_ms } else { 5000 };
            let result = match budget.timeout_ms(requested_timeout) {
                Some(timeout) => self
                    .device
                    .borrow()
                    .as_ref()
                    .context("USB handle is closed")?
                    .read_chunk(chunk, timeout),
                None => Err(Failure::new("budget_exhausted", chunk.len(), 0, true).into()),
            };
            let result = result.and_then(|_| {
                if budget.timeout_ms(1).is_none() {
                    Err(Failure::new("budget_exhausted", chunk.len(), chunk.len(), true).into())
                } else {
                    Ok(())
                }
            });
            if let Err(error) = result {
                let mut failure = Failure::details(&error);
                if failure.is_null() {
                    failure = serde_json::json!({"category":"io","cause":format!("{error:#}")});
                }
                failure["phase"] = serde_json::json!(self.phase.get());
                failure["chunk"] = serde_json::json!(number);
                failure["completedBytes"] = serde_json::json!(number * 1024 * 1024);
                failure["frameBytes"] = serde_json::json!(length);
                crate::diagnostics::details(
                    "warning",
                    "transfer.failed",
                    format_args!("USB read failed: {failure}"),
                    failure.clone(),
                );
                self.phase("transfer_failed");
                return Err(Failure(failure).into());
            }
            if let Some(proof) = continuity.as_mut() {
                proof.observe(number, number * 1024 * 1024, length, chunk)?;
            }
        }
        self.phase("transfer_complete");
        Ok(data)
    }
}
