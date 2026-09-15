//! Shared camera operations. Only the USB transport depends on the OS.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use native as platform;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("ZWOgain supports Windows, Linux, and macOS");

pub use platform::{DeviceInfo, enumerate, require_sdk_absent};

pub type Telemetry = std::sync::Arc<std::sync::Mutex<Option<[i64; 2]>>>;

pub struct Camera(
    platform::Device,
    std::cell::RefCell<Option<crate::environment::Environment>>,
    Telemetry,
);
impl Camera {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Ok(Self(
            platform::Device::open(info)?,
            std::cell::RefCell::new(None),
            Telemetry::default(),
        ))
    }
    pub fn enable_environment(&self, auxiliary: bool) -> Result<()> {
        *self.1.borrow_mut() = Some(crate::environment::Environment::open(self, auxiliary)?);
        self.publish_environment()?;
        Ok(())
    }
    pub fn telemetry(&self) -> Telemetry {
        self.2.clone()
    }
    fn publish_environment(&self) -> Result<()> {
        if let Some(environment) = self.1.borrow().as_ref() {
            *self.2.lock().unwrap() = Some([environment.get(8)?, environment.get(15)?]);
        }
        Ok(())
    }
    pub fn has_environment(&self) -> bool {
        self.1.borrow().is_some()
    }
    pub fn service_environment(&self) -> Result<()> {
        if let Some(environment) = self.1.borrow_mut().as_mut() {
            environment.service(self)?;
        }
        self.publish_environment()?;
        Ok(())
    }
    pub fn environment_control(&self, control: u32, value: Option<i64>) -> Result<i64> {
        let mut state = self.1.borrow_mut();
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
        self.0.vendor(request, value, index, length)
    }
    pub fn reset_pipe(&self) -> Result<()> {
        self.0.reset_pipe()
    }
    pub fn probe(&self) -> Result<Value> {
        self.0.probe()
    }
    pub fn cancel_read(&self) -> Result<Value> {
        self.0.cancel_read()
    }
    pub fn read_frame(&self, length: usize) -> Result<Vec<u8>> {
        self.read_frame_wait(length, 5000)
    }
    pub fn read_frame_wait(&self, length: usize, first_timeout_ms: u32) -> Result<Vec<u8>> {
        ensure!(
            (5000..=15000).contains(&first_timeout_ms),
            "invalid frame wait"
        );
        ensure!(
            length > 0 && length <= 128 * 1024 * 1024,
            "invalid frame size"
        );
        let mut data = vec![0; length];
        for (number, chunk) in data.chunks_mut(1024 * 1024).enumerate() {
            self.service_environment()?;
            self.0
                .read_chunk(chunk, if number == 0 { first_timeout_ms } else { 5000 })
                .with_context(|| format!("bulk chunk {number}"))?;
        }
        Ok(data)
    }
}
