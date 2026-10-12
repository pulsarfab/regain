//! Linux usbfs and macOS IOKit. No libusb or vendor SDK is loaded.
use crate::asi::direct::{
    diagnostics::{self, UsbTrace},
    protocol,
};
use anyhow::{Context, Result, ensure};
use nusb::{
    MaybeFuture,
    transfer::{Bulk, Completion, ControlIn, ControlOut, ControlType, In, Recipient},
};
use serde_json::{Value, json};
use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    ffi::CStr,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct DeviceInfo(nusb::DeviceInfo);
impl DeviceInfo {
    /// Physical topology key, stable across device-address changes after reset.
    /// This is a candidate selector, never a substitute for serial verification.
    pub fn locator(&self) -> String {
        format!(
            "{}:{:?}:{:04x}:{:04x}",
            self.0.bus_id(),
            self.0.port_chain(),
            self.0.vendor_id(),
            self.0.product_id()
        )
    }
    pub fn recovery_target(
        &self,
        product: u16,
        serial: String,
    ) -> Result<regain_transport::usb::Target> {
        #[cfg(target_os = "linux")]
        {
            Ok(regain_transport::usb::Target {
                vendor: 0x03c3,
                product,
                serial,
                location: self
                    .0
                    .sysfs_path()
                    .file_name()
                    .and_then(|s| s.to_str())
                    .context("No physical USB path")?
                    .into(),
                address: Some(self.0.device_address()),
                generation: None,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (product, serial);
            anyhow::bail!("USB recovery is supported on Windows and Linux only")
        }
    }
    pub fn same_interface(&self, other: &Self) -> bool {
        self.0.vendor_id() == other.0.vendor_id()
            && self.0.product_id() == other.0.product_id()
            && self.0.bus_id() == other.0.bus_id()
            && self.0.port_chain() == other.0.port_chain()
            && !self.0.port_chain().is_empty()
    }
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
impl Drop for Device {
    fn drop(&mut self) {
        diagnostics::flush_usb();
    }
}

struct PendingRead {
    offset: usize,
    length: usize,
    trace: UsbTrace,
}

/// Owns the endpoint's queued buffers until each has terminally completed.
/// Control requests use the separate nusb Device and remain on this owner thread.
pub struct ReadBatch<'a> {
    endpoint: RefMut<'a, nusb::Endpoint<Bulk, In>>,
    data: Vec<u8>,
    chunks: VecDeque<PendingRead>,
    chunk_bytes: usize,
    next_offset: usize,
    completed_bytes: usize,
}

impl ReadBatch<'_> {
    fn submit_next(&mut self) -> Result<()> {
        if self.next_offset == self.data.len() {
            return Ok(());
        }
        let length = self.chunk_bytes.min(self.data.len() - self.next_offset);
        let size = request_size(length, self.endpoint.max_packet_size())?;
        let buffer = self.endpoint.allocate(size);
        let trace = UsbTrace::start(
            "bulk",
            json!({"transport":native_transport(),"endpoint":"0x81",
            "direction":"in","requestedBytes":length,"submittedBytes":size,
            "chunk":self.next_offset / self.chunk_bytes,"frameOffset":self.next_offset,
            "frameBytes":self.data.len(),"queued":true}),
        );
        self.endpoint.submit(buffer);
        trace.event("submitted", json!({"pending":true}));
        self.chunks.push_back(PendingRead {
            offset: self.next_offset,
            length,
            trace,
        });
        self.next_offset += length;
        Ok(())
    }

    fn failure(&self, category: &str, length: usize, expired: bool) -> anyhow::Error {
        let mut failure = crate::asi::direct::transfer::Failure::new(category, length, 0, expired);
        failure.0["chunk"] = json!(self.completed_bytes / self.chunk_bytes);
        failure.0["completedBytes"] = json!(self.completed_bytes);
        failure.0["frameBytes"] = json!(self.data.len());
        failure.into()
    }

    /// The timeout covers harvesting after integration, excluding queue time.
    /// Keep continuity evidence from every accepted first-pass chunk. Observer
    /// errors follow the same cancel-and-drain path as transfer/caller failures.
    pub fn finish_observing(
        mut self,
        timeout: Duration,
        mut service: impl FnMut() -> Result<()>,
        mut observe: impl FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<Vec<u8>> {
        ensure!(!timeout.is_zero(), "queued frame timeout must be nonzero");
        let started = Instant::now();
        let mut front_started = started;
        while let Some(request) = self.chunks.front() {
            let length = request.length;
            service()?;
            if started.elapsed() >= timeout {
                return Err(self.failure("budget_exhausted", length, true));
            }
            if front_started.elapsed() >= Duration::from_secs(5) {
                return Err(self.failure("timeout", length, true));
            }
            let wait = timeout
                .saturating_sub(started.elapsed())
                .min(Duration::from_secs(5).saturating_sub(front_started.elapsed()))
                .min(Duration::from_millis(20));
            let Some(done) = self.endpoint.wait_next_complete(wait) else {
                continue;
            };
            let request = self.chunks.pop_front().unwrap();
            let (offset, length) = (request.offset, request.length);
            trace_completion(&request.trace, &done, false);
            if let Err(error) = validate_completion(&done, false, length) {
                let mut details = crate::asi::direct::transfer::Failure::details(&error);
                if !details.is_null() {
                    details["chunk"] = json!(offset / self.chunk_bytes);
                    details["completedBytes"] = json!(self.completed_bytes);
                    details["frameBytes"] = json!(self.data.len());
                    return Err(crate::asi::direct::transfer::Failure(details).into());
                }
                return Err(error);
            }
            observe(offset / self.chunk_bytes, &done.buffer)?;
            self.data[offset..offset + length].copy_from_slice(&done.buffer);
            self.completed_bytes += length;
            self.submit_next()?;
            front_started = Instant::now();
        }
        ensure!(
            self.completed_bytes == self.data.len() && self.endpoint.pending() == 0,
            "queued frame did not complete every byte"
        );
        Ok(std::mem::take(&mut self.data))
    }

    fn cancel_and_drain(&mut self) {
        if self.endpoint.pending() == 0 {
            return;
        }
        for request in &self.chunks {
            request
                .trace
                .event("cancel", json!({"cancelRequested":true}));
        }
        self.endpoint.cancel_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.endpoint.pending() != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                eprintln!(
                    "Queued USB cancellation did not drain; terminating worker with buffers intact"
                );
                std::process::exit(125);
            }
            if let Some(done) = self
                .endpoint
                .wait_next_complete(remaining.min(Duration::from_millis(20)))
                && let Some(request) = self.chunks.pop_front()
            {
                trace_completion(&request.trace, &done, true);
            }
        }
        self.chunks.clear();
    }
}

impl Drop for ReadBatch<'_> {
    fn drop(&mut self) {
        self.cancel_and_drain();
    }
}

fn native_transport() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux-usbfs"
    } else {
        "macos-iokit"
    }
}

fn trace_completion(trace: &UsbTrace, done: &Completion, cancel_requested: bool) {
    trace.event(
        "complete",
        json!({"nativeStatus":format!("{:?}",done.status),
        "receivedBytes":done.actual_len,"bufferBytes":done.buffer.len(),
        "cancelRequested":cancel_requested,"terminalCompletionObserved":true}),
    );
}

impl Device {
    /// Queue exactly one first-pass frame, with at most 128 outstanding reads.
    /// Endpoint ownership prevents a reset or retained read before this batch
    /// finishes/drains, while device control/environment requests remain usable.
    pub fn queue_frame(&self, length: usize, chunk_bytes: usize) -> Result<ReadBatch<'_>> {
        ensure!(
            length > 0 && length <= 128 * 1024 * 1024,
            "invalid queued frame size"
        );
        ensure!(
            chunk_bytes > 0 && chunk_bytes <= 1024 * 1024,
            "invalid queued read chunk size"
        );
        let endpoint = self
            .input
            .try_borrow_mut()
            .context("bulk endpoint already borrowed")?;
        ensure!(
            endpoint.pending() == 0,
            "previous USB transfer is still pending"
        );
        let depth = length.div_ceil(chunk_bytes).min(128);
        let mut batch = ReadBatch {
            endpoint,
            data: vec![0; length],
            chunks: VecDeque::with_capacity(depth),
            chunk_bytes,
            next_offset: 0,
            completed_bytes: 0,
        };
        for _ in 0..depth {
            batch.submit_next()?;
        }
        Ok(batch)
    }
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
        let mut fields = diagnostics::control_fields(request, value, index, length);
        fields["transport"] = json!(native_transport());
        fields["timeoutMs"] = json!(5000);
        let trace = UsbTrace::start("control", fields);
        let result = self.vendor_inner(request, value, index, length);
        let mut completion = match &result {
            Ok(data) => {
                json!({"nativeStatus":"Ok(())","receivedBytes":data.len(),"terminalCompletionObserved":true})
            }
            Err(error) => {
                json!({"nativeStatus":"error","error":format!("{error:#}"),"terminalCompletionObserved":true})
            }
        };
        if let Ok(data) = &result {
            completion.as_object_mut().unwrap().extend(
                diagnostics::control_payload(request, length, data)
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
        trace.event("complete", completion);
        result
    }

    fn vendor_inner(&self, request: u8, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
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
        let trace = UsbTrace::start(
            "clear-halt",
            json!({"transport":native_transport(),"endpoint":"0x81"}),
        );
        let result = self.reset_pipe_inner();
        trace.event(
            "complete",
            match &result {
                Ok(()) => json!({"nativeStatus":"Ok(())"}),
                Err(error) => json!({"nativeStatus":"error","error":format!("{error:#}")}),
            },
        );
        result
    }

    fn reset_pipe_inner(&self) -> Result<()> {
        let mut endpoint = self
            .input
            .try_borrow_mut()
            .context("bulk endpoint already borrowed")?;
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
        let mut endpoint = self
            .input
            .try_borrow_mut()
            .context("bulk endpoint already borrowed")?;
        ensure!(
            endpoint.pending() == 0,
            "previous USB transfer is still pending"
        );
        // Native IN requests must be packet-aligned. The last frame chunk may
        // be shorter; accept it only when the actual byte count is exact.
        let size = request_size(length, endpoint.max_packet_size())?;
        let buffer = endpoint.allocate(size);
        let trace = UsbTrace::start(
            "bulk",
            json!({"transport":native_transport(),"endpoint":"0x81",
            "direction":"in","requestedBytes":length,"submittedBytes":size,"timeoutMs":timeout_ms,"queued":false}),
        );
        endpoint.submit(buffer);
        trace.event("submitted", json!({"pending":true}));
        if let Some(done) =
            endpoint.wait_next_complete(Duration::from_millis(u64::from(timeout_ms)))
        {
            trace_completion(&trace, &done, false);
            return Ok((done, false));
        }
        trace.event(
            "cancel",
            json!({"cancelRequested":true,"timeoutMs":timeout_ms}),
        );
        endpoint.cancel_all();
        if let Some(done) = endpoint.wait_next_complete(Duration::from_secs(2)) {
            trace_completion(&trace, &done, true);
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
    if !(!expired
        && done.status.is_ok()
        && done.actual_len == expected
        && done.buffer.len() == expected)
    {
        use nusb::transfer::TransferError;
        let category = if expired {
            "timeout"
        } else {
            match done.status {
                Err(TransferError::Cancelled) => "cancelled",
                Err(TransferError::Disconnected) => "disconnected",
                Err(TransferError::Stall) => "stall",
                Err(_) => "io",
                Ok(()) if done.actual_len < expected => "short_read",
                Ok(()) => "overflow",
            }
        };
        let mut failure = crate::asi::direct::transfer::Failure::new(
            category,
            expected,
            done.actual_len,
            expired,
        );
        failure.0["nativeStatus"] = json!(format!("{:?}", done.status));
        failure.observe_short_read(&done.buffer);
        failure.0["terminalCompletionObserved"] = json!(true);
        return Err(failure.into());
    }
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

    #[test]
    fn queued_short_frame_trailer_is_evidence_not_an_accepted_image() {
        let bytes = vec![0x7e, 0x5a, 7, 0, 9, 0, 0xf0, 0x3c];
        let done = Completion {
            actual_len: bytes.len(),
            buffer: bytes.into(),
            status: Ok(()),
        };
        let error = validate_completion(&done, false, 1024).unwrap_err();
        let details = crate::asi::direct::transfer::Failure::details(&error);
        assert_eq!(details["category"], "short_read");
        assert_eq!(details["frameTrailerObserved"], true);
        assert_eq!(details["terminalCompletionObserved"], true);
    }
}
