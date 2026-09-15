//! Linux usbfs and macOS IOKit. No libusb or vendor SDK is loaded.
use crate::protocol;
use anyhow::{Context, Result, ensure};
use nusb::{
    MaybeFuture,
    transfer::{Bulk, Completion, ControlIn, ControlOut, ControlType, In, Recipient},
};
use serde_json::{Value, json};
use std::{cell::RefCell, ffi::CStr, time::Duration};

pub struct DeviceInfo(nusb::DeviceInfo);
impl DeviceInfo {
    pub fn matches(&self, vendor: u16, product: u16) -> bool {
        self.0.vendor_id() == vendor && self.0.product_id() == product
    }
}

pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    Ok(nusb::list_devices()
        .wait()
        .context("enumerate USB devices")?
        .filter(|d| d.vendor_id() == 0x03c3)
        .map(DeviceInfo)
        .collect())
}

pub struct Device {
    device: nusb::Device,
    _interface: nusb::Interface,
    input: RefCell<nusb::Endpoint<Bulk, In>>,
    info: Value,
}

impl Device {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        let device = info
            .0
            .open()
            .wait()
            .context("open USB camera (check permissions and other camera apps)")?;
        let config = device
            .active_configuration()
            .context("camera has no active USB configuration")?;
        let mut description =
            protocol::describe(device.device_descriptor().as_bytes(), config.as_bytes(), 0)?;
        description["driverVersionRaw"] = Value::Null;
        description["transport"] = json!(if cfg!(target_os = "linux") {
            "linux-usbfs"
        } else {
            "macos-iokit"
        });
        // Use only the observed endpoint in its default alternate setting. Do not
        // reconfigure or detach a kernel driver to seize an occupied camera.
        let number = capture_interface(&description)?;
        let interface = device
            .claim_interface(number)
            .wait()
            .context("claim camera interface exclusively")?;
        let input = interface
            .endpoint::<Bulk, In>(0x81)
            .context("open bulk-IN endpoint 0x81")?;
        ensure!(
            matches!(input.max_packet_size(), 64 | 512 | 1024),
            "unexpected bulk packet size"
        );
        Ok(Self {
            device,
            _interface: interface,
            input: RefCell::new(input),
            info: description,
        })
    }

    pub fn vendor(&self, request: u8, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        let timeout = Duration::from_secs(5);
        if length == 0 {
            self.device
                .control_out(
                    ControlOut {
                        control_type: ControlType::Vendor,
                        recipient: Recipient::Device,
                        request,
                        value,
                        index,
                        data: &[],
                    },
                    timeout,
                )
                .wait()
                .with_context(|| format!("vendor OUT {request:02x}/{value:04x}"))?;
            Ok(Vec::new())
        } else {
            let data = self
                .device
                .control_in(
                    ControlIn {
                        control_type: ControlType::Vendor,
                        recipient: Recipient::Device,
                        request,
                        value,
                        index,
                        length,
                    },
                    timeout,
                )
                .wait()
                .with_context(|| format!("vendor IN {request:02x}/{value:04x}"))?;
            ensure!(
                data.len() == usize::from(length),
                "short vendor response: {}/{length}",
                data.len()
            );
            Ok(data)
        }
    }

    pub fn reset_pipe(&self) -> Result<()> {
        let mut endpoint = self.input.borrow_mut();
        ensure!(
            endpoint.pending() == 0,
            "cannot reset an endpoint with pending transfers"
        );
        // Cancellation is drained by transfer(). This resets both host and
        // device data toggles; Windows transfer-size IOCTLs do not apply here.
        endpoint
            .clear_halt()
            .wait()
            .context("clear bulk endpoint halt")
    }

    fn transfer(&self, length: usize, timeout_ms: u32) -> Result<(Completion, bool)> {
        let mut endpoint = self.input.borrow_mut();
        ensure!(
            endpoint.pending() == 0,
            "previous USB transfer is still pending"
        );
        // Native IN requests must be packet-aligned. The last frame chunk may
        // be shorter; accept it only when the actual byte count is exact.
        let size = request_size(length, endpoint.max_packet_size())?;
        let buffer = endpoint.allocate(size);
        endpoint.submit(buffer);
        if let Some(done) =
            endpoint.wait_next_complete(Duration::from_millis(u64::from(timeout_ms)))
        {
            return Ok((done, false));
        }
        endpoint.cancel_all();
        if let Some(done) = endpoint.wait_next_complete(Duration::from_secs(2)) {
            return Ok((done, true));
        }
        // A cancellation request is not terminal completion. Exit without
        // dropping any buffers if the OS fails to drain, as on Windows.
        eprintln!("USB cancellation did not drain; terminating worker with buffers intact");
        std::process::exit(125);
    }

    pub fn read_chunk(&self, chunk: &mut [u8], timeout_ms: u32) -> Result<()> {
        let (done, expired) = self.transfer(chunk.len(), timeout_ms)?;
        validate_completion(&done, expired, chunk.len())?;
        chunk.copy_from_slice(&done.buffer);
        Ok(())
    }

    pub fn probe(&self) -> Result<Value> {
        Ok(self.info.clone())
    }

    pub fn cancel_read(&self) -> Result<Value> {
        let (done, expired) = self.transfer(16384, 100)?;
        Ok(
            json!({"requestedBytes":16384,"receivedBytes":done.actual_len,
            "cancelRequested":expired,"transferError":done.status.err().map(|e| e.to_string()),
            "terminalCompletionObserved":true,"imageAccepted":false}),
        )
    }
}

fn capture_interface(info: &Value) -> Result<u8> {
    let candidates: Vec<_> = info["endpoints"]
        .as_array()
        .context("missing endpoints")?
        .iter()
        .filter(|e| e["address"] == 0x81 && e["attributes"] == 2 && e["alternate"] == 0)
        .collect();
    ensure!(
        candidates.len() == 1,
        "expected one bulk-IN endpoint 0x81 in alternate setting 0"
    );
    Ok(u8::try_from(
        candidates[0]["interface"]
            .as_u64()
            .context("invalid interface")?,
    )?)
}

fn request_size(length: usize, packet: usize) -> Result<usize> {
    ensure!(
        length > 0 && length <= 1024 * 1024,
        "invalid bulk chunk size"
    );
    ensure!(
        matches!(packet, 64 | 512 | 1024),
        "invalid bulk packet size"
    );
    Ok(length.div_ceil(packet) * packet)
}

fn validate_completion(done: &Completion, expired: bool, expected: usize) -> Result<()> {
    ensure!(
        !expired
            && done.status.is_ok()
            && done.actual_len == expected
            && done.buffer.len() == expected,
        "bulk failed: {:?}, bytes {}/{expected}, deadlineExpired {expired}",
        done.status,
        done.actual_len
    );
    Ok(())
}

fn is_sdk(name: &CStr) -> bool {
    name.to_string_lossy()
        .to_ascii_lowercase()
        .contains("libasicamera2.")
}

#[cfg(target_os = "linux")]
pub fn require_sdk_absent() -> Result<()> {
    unsafe extern "C" fn inspect(
        info: *mut libc::dl_phdr_info,
        _: usize,
        found: *mut libc::c_void,
    ) -> libc::c_int {
        // SAFETY: dl_iterate_phdr supplies a valid descriptor and the live bool
        // passed below; dlpi_name is a null-terminated loader-owned string.
        unsafe {
            if !(*info).dlpi_name.is_null() && is_sdk(CStr::from_ptr((*info).dlpi_name)) {
                *found.cast::<bool>() = true;
                return 1;
            }
        }
        0
    }
    let mut found = false;
    // SAFETY: callback does not retain pointers or unwind across the C ABI.
    unsafe {
        libc::dl_iterate_phdr(Some(inspect), (&mut found as *mut bool).cast());
    }
    ensure!(!found, "ASI SDK unexpectedly loaded into direct process");
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn require_sdk_absent() -> Result<()> {
    unsafe extern "C" {
        fn _dyld_image_count() -> u32;
        fn _dyld_get_image_name(index: u32) -> *const libc::c_char;
    }
    // SAFETY: this executable does not load/unload libraries concurrently.
    // dyld owns the returned null-terminated names; null entries are skipped.
    unsafe {
        for index in 0.._dyld_image_count() {
            let name = _dyld_get_image_name(index);
            ensure!(
                name.is_null() || !is_sdk(CStr::from_ptr(name)),
                "ASI SDK unexpectedly loaded into direct process"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusb::transfer::{Buffer, TransferError};

    #[test]
    fn final_chunk_is_packet_aligned_but_never_accepts_padding_or_partial_data() {
        assert_eq!(request_size(8208, 1024).unwrap(), 9216);
        assert_eq!(request_size(1024 * 1024, 512).unwrap(), 1024 * 1024);
        assert!(request_size(0, 512).is_err());
        assert!(request_size(8192, 0).is_err());
        let mut done = Completion {
            buffer: vec![0; 8208].into(),
            actual_len: 8208,
            status: Ok(()),
        };
        assert!(validate_completion(&done, false, 8208).is_ok());
        assert!(validate_completion(&done, true, 8208).is_err());
        done.status = Err(TransferError::Cancelled);
        assert!(validate_completion(&done, false, 8208).is_err());
        done.status = Ok(());
        done.actual_len = 8192;
        assert!(validate_completion(&done, false, 8208).is_err());
        done.actual_len = 9216;
        done.buffer = Buffer::from(vec![0; 9216]);
        assert!(validate_completion(&done, false, 8208).is_err());
    }

    #[test]
    fn claims_only_the_observed_default_bulk_interface() {
        let endpoint = json!({"address":129,"attributes":2,"alternate":0,"interface":3});
        assert_eq!(
            capture_interface(&json!({"endpoints":[endpoint]})).unwrap(),
            3
        );
        assert!(capture_interface(&json!({"endpoints":[endpoint,endpoint]})).is_err());
        let mut other = endpoint;
        other["alternate"] = json!(1);
        assert!(capture_interface(&json!({"endpoints":[other]})).is_err());
    }
}
