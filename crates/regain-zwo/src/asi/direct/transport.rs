//! Shared camera operations. Only the USB transport depends on the OS.
use crate::asi::direct::transfer::{Budget, Failure};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
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

/// Temperature, power, acknowledged target and cooler enable, in that order.
pub type Telemetry = std::sync::Arc<std::sync::Mutex<Option<TelemetrySample>>>;
#[derive(Clone, Copy)]
pub struct TelemetrySample {
    pub values: [i64; 4],
    pub observed_at: [Instant; 4],
}
impl TelemetrySample {
    pub fn new(values: [i64; 4], observed: Instant) -> Self {
        Self {
            values,
            observed_at: [observed; 4],
        }
    }
    pub fn index(control: u32) -> Result<usize> {
        Ok(match control {
            8 => 0,
            15 => 1,
            16 => 2,
            17 => 3,
            _ => anyhow::bail!("Unsupported telemetry control"),
        })
    }
    pub fn observation(&self, control: u32) -> Result<regain_core::ControlObservationReply> {
        let index = Self::index(control)?;
        let age = Instant::now()
            .checked_duration_since(self.observed_at[index])
            .ok_or_else(|| anyhow::anyhow!("Invalid telemetry observation time"))?;
        Ok(regain_core::ControlObservationReply {
            value: self.values[index],
            age_seconds: age.as_secs_f64(),
        })
    }
}

pub struct Camera {
    device: RefCell<Option<platform::Device>>,
    environment: RefCell<Option<crate::asi::direct::environment::Environment>>,
    telemetry: Telemetry,
    cooling: super::environment::CoolingQueue,
    identity: DeviceInfo,
    transfer_timeout: Cell<Duration>,
    read_chunk_bytes: Cell<usize>,
    phase: Cell<&'static str>,
}
impl Camera {
    pub fn locator(&self) -> String {
        self.identity.locator()
    }
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Ok(Self {
            device: RefCell::new(Some(platform::Device::open(info)?)),
            environment: RefCell::new(None),
            telemetry: Telemetry::default(),
            cooling: super::environment::CoolingQueue::default(),
            identity: info.clone(),
            transfer_timeout: Cell::new(Duration::from_secs(60)),
            read_chunk_bytes: Cell::new(1024 * 1024),
            phase: Cell::new("idle"),
        })
    }
    pub fn enable_environment(
        &self,
        auxiliary: bool,
        heater: bool,
        output: super::environment::CoolerOutput,
    ) -> Result<()> {
        *self.environment.borrow_mut() = Some(crate::asi::direct::environment::Environment::open(
            self, auxiliary, heater, output,
        )?);
        self.publish_environment()?;
        Ok(())
    }
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry.clone()
    }
    pub(super) fn cooling_queue(&self) -> super::environment::CoolingQueue {
        self.cooling.clone()
    }
    fn publish_environment(&self) -> Result<()> {
        if let Some(environment) = self.environment.borrow().as_ref() {
            *self.telemetry.lock().unwrap() = Some(TelemetrySample {
                values: [
                    environment.get(8)?,
                    environment.get(15)?,
                    environment.get(16)?,
                    environment.get(17)?,
                ],
                observed_at: environment.observed_at(),
            });
        }
        Ok(())
    }
    pub fn has_environment(&self) -> bool {
        self.environment.borrow().is_some()
    }
    pub fn service_environment(&self) -> Result<()> {
        self.cooling
            .service(|control, value| self.environment_control(control, Some(value)));
        if let Some(environment) = self.environment.borrow_mut().as_mut() {
            environment.service(self)?;
        }
        self.publish_environment()?;
        Ok(())
    }
    pub fn resume_cooling(&self, power: i64, prior: f64, previous_target: i64) -> Result<()> {
        self.environment
            .borrow_mut()
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("environment unavailable"))?
            .resume_cooling(self, power, prior, previous_target)?;
        self.publish_environment()
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
    pub fn read_chunk_size(&self, kib: u32) -> Result<()> {
        self.read_chunk_bytes
            .set(super::settings::read_chunk_bytes(kib)?);
        Ok(())
    }
    pub fn phase(&self, phase: &'static str) {
        let previous = self.phase.replace(phase);
        crate::asi::direct::diagnostics::details(
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
        self.read_frame_inner(length, first_timeout_ms, None, None)
    }
    pub fn read_video_frame(
        &self,
        length: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<u8>> {
        self.read_frame_inner(length, 5000, None, Some(cancel))
    }
    pub fn read_frame_checked(
        &self,
        length: usize,
        first_timeout_ms: u32,
        continuity: &mut crate::asi::direct::transfer::Continuity,
    ) -> Result<Vec<u8>> {
        self.read_frame_inner(length, first_timeout_ms, Some(continuity), None)
    }
    fn read_frame_inner(
        &self,
        length: usize,
        first_timeout_ms: u32,
        mut continuity: Option<&mut crate::asi::direct::transfer::Continuity>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
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
        // Keep the request boundaries identical throughout retained-frame retries.
        let chunk_bytes = self.read_chunk_bytes.get();
        let started = std::time::Instant::now();
        let budget = Budget::new(self.transfer_timeout.get());
        self.phase("downloading");
        for (number, chunk) in data.chunks_mut(chunk_bytes).enumerate() {
            ensure!(
                !cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)),
                "video read cancelled"
            );
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
                failure["completedBytes"] = serde_json::json!(number * chunk_bytes);
                failure["frameBytes"] = serde_json::json!(length);
                crate::asi::direct::diagnostics::details(
                    "warning",
                    "transfer.failed",
                    format_args!("USB read failed: {failure}"),
                    failure.clone(),
                );
                self.phase("transfer_failed");
                return Err(Failure(failure).into());
            }
            if let Some(proof) = continuity.as_mut() {
                proof.observe(number, number * chunk_bytes, length, chunk)?;
            }
        }
        self.phase("transfer_complete");
        crate::asi::direct::diagnostics::details(
            "debug",
            "transfer.complete",
            "USB frame read completed",
            serde_json::json!({"readChunkKiB":chunk_bytes / 1024,"frameBytes":length,
                "requests":length.div_ceil(chunk_bytes),"elapsedUs":started.elapsed().as_micros()}),
        );
        Ok(data)
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn independent_cached_observation_ages_never_reset_on_reads() {
        let now = Instant::now();
        let mut sample = TelemetrySample::new([215, 30, -10, 1], now - Duration::from_secs(20));
        sample.observed_at[2] = now - Duration::from_secs(5);
        let before = sample.observed_at;
        for _ in 0..3 {
            let temperature = sample.observation(8).unwrap();
            assert_eq!(temperature.value, 215);
            assert!(temperature.age_seconds >= 20.0);
            assert!(sample.observation(15).unwrap().age_seconds >= 20.0);
            assert!(sample.observation(16).unwrap().age_seconds >= 5.0);
            assert_eq!(sample.observed_at, before);
        }
        sample.observed_at[0] = now + Duration::from_secs(3600);
        assert!(sample.observation(8).is_err());
        for control in [0, 5, 21, u32::MAX] {
            assert!(sample.observation(control).is_err());
        }
    }
}
