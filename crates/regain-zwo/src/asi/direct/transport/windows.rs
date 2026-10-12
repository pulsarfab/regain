use crate::asi::direct::{
    diagnostics::{self, UsbTrace},
    protocol,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    mem::size_of,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, Threading::*},
    },
    core::GUID,
};

const INTERFACE: GUID = GUID::from_u128(0xc5b27530_3592_4e87_9e99_c2bafd5e5692);

pub fn require_sdk_absent() -> Result<()> {
    let name: Vec<u16> = "ASICamera2.dll\0".encode_utf16().collect();
    ensure!(
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(name.as_ptr()) }
            .is_null(),
        "ASI SDK unexpectedly loaded into direct process"
    );
    Ok(())
}

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn error() -> std::io::Error {
    std::io::Error::last_os_error()
}

pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    // SAFETY: SDK-independent SetupAPI enumeration; structures initialized with
    // their native cbSize. Variable detail allocation is aligned for its header.
    unsafe {
        let raw = SetupDiGetClassDevsW(
            &INTERFACE,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        ensure!(raw != -1, "enumerate interfaces: {}", error());
        let set = DeviceSet(raw);
        let mut paths = Vec::new();
        for index in 0..128 {
            let mut data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set.0, null(), &INTERFACE, index, &mut data) == 0 {
                if GetLastError() == ERROR_NO_MORE_ITEMS {
                    return Ok(paths);
                }
                return Err(error()).context("enumerate interface");
            }
            let mut required = 0;
            let first = SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &data,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
            ensure!(
                first == 0
                    && GetLastError() == ERROR_INSUFFICIENT_BUFFER
                    && (8..=65536).contains(&required),
                "invalid interface detail size"
            );
            let mut storage = vec![0_u64; (required as usize).div_ceil(8)];
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &data,
                detail,
                required,
                null_mut(),
                null_mut(),
            ) == 0
            {
                return Err(error()).context("get interface detail");
            }
            let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            let text = std::slice::from_raw_parts(
                std::ptr::addr_of!((*detail).DevicePath).cast::<u16>(),
                (required as usize - offset) / 2,
            );
            let end = text
                .iter()
                .position(|&v| v == 0)
                .context("unterminated interface path")?;
            paths.push(DeviceInfo(text[..=end].to_vec()));
        }
        bail!("too many driver interfaces")
    }
}

#[derive(Clone)]
pub struct DeviceInfo(Vec<u16>);
impl DeviceInfo {
    /// Opaque selection key; callers must still verify the camera serial.
    pub fn locator(&self) -> String {
        String::from_utf16_lossy(&self.0)
            .trim_end_matches('\0')
            .to_ascii_lowercase()
    }
    pub fn recovery_target(
        &self,
        product: u16,
        serial: String,
    ) -> Result<regain_transport::usb::Target> {
        // SetupAPI USB interface symbolic links contain the physical instance ID.
        let path = String::from_utf16_lossy(&self.0);
        let pieces: Vec<_> = path.trim_end_matches('\0').split('#').collect();
        ensure!(
            pieces.len() == 4 && pieces[0].eq_ignore_ascii_case("\\\\?\\usb"),
            "Unexpected USB interface path"
        );
        let mut target = regain_transport::usb::Target {
            vendor: 0x03c3,
            product,
            serial,
            location: format!("USB\\{}\\{}", pieces[1], pieces[2]),
            address: None,
            generation: None,
        };
        target.bind_generation()?;
        Ok(target)
    }
    pub fn same_interface(&self, other: &Self) -> bool {
        String::from_utf16_lossy(&self.0).eq_ignore_ascii_case(&String::from_utf16_lossy(&other.0))
    }
    pub fn matches(&self, vendor: u16, product: u16) -> bool {
        String::from_utf16_lossy(&self.0)
            .to_ascii_lowercase()
            .contains(&format!("vid_{vendor:04x}&pid_{product:04x}"))
    }
}
pub struct Device(Handle);
impl Drop for Device {
    fn drop(&mut self) {
        // Every request has drained before the exclusive device can be dropped.
        diagnostics::flush_usb();
    }
}
struct Completion {
    bytes: usize,
    error: u32,
    cancel_requested: bool,
    deadline_expired: bool,
    cancellation_reason: Option<&'static str>,
    wait_error: Option<u32>,
}
impl Completion {
    fn details(&self) -> Value {
        json!({"win32Error":self.error,"receivedBytes":self.bytes,
            "cancelRequested":self.cancel_requested,"deadlineExpired":self.deadline_expired,
            "cancellationReason":self.cancellation_reason,"waitError":self.wait_error,
            "lateCompletion":self.cancel_requested && self.error == 0,
            "commandEffectUncertain":self.cancel_requested,"terminalCompletionObserved":true})
    }
    fn ensure_success(&self, operation: &str, expected: usize) -> Result<()> {
        if self.error == 0 && !self.cancel_requested {
            return Ok(());
        }
        let category = if self.deadline_expired {
            "timeout"
        } else if self.error == ERROR_DEVICE_NOT_CONNECTED {
            "disconnected"
        } else if self.error == ERROR_OPERATION_ABORTED {
            "cancelled"
        } else {
            "io"
        };
        let mut failure = crate::asi::direct::transfer::Failure::new(
            category,
            expected,
            self.bytes,
            self.deadline_expired,
        );
        failure
            .0
            .as_object_mut()
            .unwrap()
            .extend(self.details().as_object().unwrap().clone());
        failure.0["operation"] = json!(operation);
        Err(failure.into())
    }
}

// Each box is allocated before submission and is never moved out while the
// driver may use its pointers. The frame Vec's allocation also stays fixed.
// No request, buffer or handle is sent to another Rust thread.
struct PendingRead {
    header: UnsafeCell<[u8; protocol::HEADER]>,
    overlap: UnsafeCell<OVERLAPPED>,
    event: Handle,
    bytes: UnsafeCell<u32>,
    offset: usize,
    length: usize,
    immediate: bool,
    trace: UsbTrace,
}

pub struct ReadBatch<'a> {
    device: &'a Device,
    data: Vec<u8>,
    pending: VecDeque<Box<PendingRead>>,
    chunk_bytes: usize,
    next_offset: usize,
    completed_bytes: usize,
}

fn queue_depth(length: usize, chunk_bytes: usize) -> Result<usize> {
    ensure!(
        length > 0 && length <= 128 * 1024 * 1024,
        "invalid queued frame size"
    );
    ensure!(
        chunk_bytes > 0 && chunk_bytes <= 1024 * 1024,
        "invalid queued read chunk size"
    );
    Ok(length.div_ceil(chunk_bytes).min(128))
}

fn queued_read_failure(
    code: u32,
    nt: u32,
    usb: u32,
    received: usize,
    expected: usize,
) -> Option<&'static str> {
    if code == ERROR_OPERATION_ABORTED {
        Some("cancelled")
    } else if code == ERROR_DEVICE_NOT_CONNECTED {
        Some("disconnected")
    } else if code != 0 || nt != 0 || usb != 0 {
        Some("io")
    } else if received < expected {
        Some("short_read")
    } else if received > expected {
        Some("overflow")
    } else {
        None
    }
}

// Cancellation is requested for every operation before waiting on any one.
// Returning false deliberately keeps uncompleted requests in their queue: the
// caller must terminate with allocations intact, never drop DMA targets.
fn drain_requests<T>(
    pending: &mut VecDeque<T>,
    timeout: Duration,
    mut cancel: impl FnMut(&T),
    mut terminal: impl FnMut(&mut T, Duration) -> bool,
) -> bool {
    for request in pending.iter() {
        cancel(request);
    }
    let deadline = Instant::now() + timeout;
    while let Some(request) = pending.front_mut() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if terminal(request, remaining.min(Duration::from_millis(20))) {
            pending.pop_front();
        }
    }
    true
}

impl ReadBatch<'_> {
    fn submit_next(&mut self) -> Result<()> {
        if self.next_offset == self.data.len() {
            return Ok(());
        }
        let length = self.chunk_bytes.min(self.data.len() - self.next_offset);
        let raw = unsafe { CreateEventW(null(), 1, 0, null()) };
        ensure!(!raw.is_null(), "create queued read event: {}", error());
        let event = Handle(raw);
        let mut request = Box::new(PendingRead {
            header: UnsafeCell::new([0; protocol::HEADER]),
            overlap: UnsafeCell::new(unsafe { std::mem::zeroed() }),
            event,
            bytes: UnsafeCell::new(0),
            offset: self.next_offset,
            length,
            immediate: false,
            trace: UsbTrace::start(
                "bulk",
                json!({"transport":"windows-cyusb3", "endpoint":"0x81",
                "direction":"in", "requestedBytes":length, "chunk":self.next_offset / self.chunk_bytes,
                "frameOffset":self.next_offset, "frameBytes":self.data.len(), "queued":true}),
            ),
        });
        request.header.get_mut()[13] = 0x81;
        request.overlap.get_mut().hEvent = request.event.0;
        // SAFETY: all pointers refer to stable owned heap allocations. The
        // batch drains every submitted request before freeing any allocation,
        // including partial submission failure and unwinding from callbacks.
        let ok = unsafe {
            DeviceIoControl(
                self.device.0.0,
                protocol::BULK,
                request.header.get().cast(),
                protocol::HEADER as u32,
                self.data.as_mut_ptr().add(request.offset).cast(),
                length as u32,
                request.bytes.get(),
                request.overlap.get(),
            )
        };
        let code = if ok == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        request.trace.event(
            "submitted",
            json!({"win32Error":code, "pending":code == ERROR_IO_PENDING,
            "synchronous":ok != 0}),
        );
        if ok == 0 && code != ERROR_IO_PENDING {
            request.trace.event(
                "complete",
                json!({"win32Error":code,"receivedBytes":0,
                    "submitted":false,"terminalCompletionObserved":true}),
            );
            let category = if code == ERROR_DEVICE_NOT_CONNECTED {
                "disconnected"
            } else {
                "io"
            };
            let mut failure =
                crate::asi::direct::transfer::Failure::new(category, length, 0, false);
            failure.0["win32Error"] = json!(code);
            failure.0["submitted"] = json!(false);
            failure.0["chunk"] = json!(request.offset / self.chunk_bytes);
            failure.0["completedBytes"] = json!(self.completed_bytes);
            failure.0["frameBytes"] = json!(self.data.len());
            return Err(failure.into());
        }
        request.immediate = ok != 0;
        self.next_offset += length;
        self.pending.push_back(request);
        Ok(())
    }

    fn failure(
        &self,
        category: &str,
        length: usize,
        received: usize,
        expired: bool,
    ) -> anyhow::Error {
        let mut failure =
            crate::asi::direct::transfer::Failure::new(category, length, received, expired);
        failure.0["chunk"] = json!(self.completed_bytes / self.chunk_bytes);
        failure.0["completedBytes"] = json!(self.completed_bytes);
        failure.0["frameBytes"] = json!(self.data.len());
        failure.into()
    }

    /// Harvest the exact frame after the owner has driven integration; the
    /// transfer budget begins here, not at queueing. Observe accepted chunks
    /// before refill to preserve pixel continuity for any frozen-frame replay.
    /// Unlike the SDK's rolling queue, never submit speculative next-frame reads.
    pub fn finish_observing(
        mut self,
        timeout: Duration,
        mut service: impl FnMut() -> Result<()>,
        mut observe: impl FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<Vec<u8>> {
        ensure!(!timeout.is_zero(), "queued frame timeout must be nonzero");
        let started = Instant::now();
        let mut front_started = started;
        while let Some(request) = self.pending.front() {
            service()?;
            if started.elapsed() >= timeout {
                return Err(self.failure("budget_exhausted", request.length, 0, true));
            }
            if front_started.elapsed() >= Duration::from_secs(5) {
                return Err(self.failure("timeout", request.length, 0, true));
            }
            let wait_ms = timeout
                .saturating_sub(started.elapsed())
                .min(Duration::from_secs(5).saturating_sub(front_started.elapsed()))
                .min(Duration::from_millis(20))
                .as_millis()
                .max(1) as u32;
            let wait = if request.immediate {
                WAIT_OBJECT_0
            } else {
                unsafe { WaitForSingleObject(request.event.0, wait_ms) }
            };
            if wait == WAIT_TIMEOUT {
                continue;
            }
            if wait != WAIT_OBJECT_0 {
                return Err(error()).context("wait queued USB read");
            }
            let request = self.pending.front_mut().unwrap();
            let done = unsafe {
                GetOverlappedResult(
                    self.device.0.0,
                    request.overlap.get(),
                    request.bytes.get(),
                    0,
                )
            };
            let code = if done != 0 {
                0
            } else {
                unsafe { GetLastError() }
            };
            if code == ERROR_IO_INCOMPLETE {
                // A signalled event alone is not a terminal-completion proof.
                eprintln!(
                    "Queued USB read signalled before terminal completion; terminating worker"
                );
                std::process::exit(125);
            }
            let request = self.pending.pop_front().unwrap();
            let received = if code == 0 {
                unsafe { *request.bytes.get() as usize }
            } else {
                0
            };
            let (nt, usb) = protocol::status(unsafe { &*request.header.get() })?;
            request.trace.event("complete", json!({"win32Error":code,"ntStatus":format!("0x{nt:08x}"),
                "usbStatus":format!("0x{usb:08x}"),"receivedBytes":received,"terminalCompletionObserved":true}));
            if let Some(category) = queued_read_failure(code, nt, usb, received, request.length) {
                let mut failure = crate::asi::direct::transfer::Failure::new(
                    category,
                    request.length,
                    received,
                    false,
                );
                failure.0["win32Error"] = json!(code);
                failure.0["ntStatus"] = json!(nt);
                failure.0["usbStatus"] = json!(usb);
                failure.0["terminalCompletionObserved"] = json!(true);
                failure.0["chunk"] = json!(request.offset / self.chunk_bytes);
                failure.0["completedBytes"] = json!(self.completed_bytes);
                failure.0["frameBytes"] = json!(self.data.len());
                // This slice belongs only to a terminally completed request;
                // all other pending DMA targets are disjoint from it.
                let completed = unsafe {
                    std::slice::from_raw_parts(
                        self.data.as_ptr().add(request.offset),
                        request.length,
                    )
                };
                failure.observe_short_read(completed);
                return Err(failure.into());
            }
            let completed = unsafe {
                std::slice::from_raw_parts(self.data.as_ptr().add(request.offset), request.length)
            };
            observe(request.offset / self.chunk_bytes, completed)?;
            self.completed_bytes += request.length;
            self.submit_next()?;
            front_started = Instant::now();
        }
        ensure!(
            self.completed_bytes == self.data.len(),
            "queued frame did not complete every byte"
        );
        Ok(std::mem::take(&mut self.data))
    }

    fn cancel_and_drain(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let drained = drain_requests(
            &mut self.pending,
            Duration::from_secs(2),
            |request| unsafe {
                let ok = CancelIoEx(self.device.0.0, request.overlap.get());
                let code = if ok == 0 { GetLastError() } else { 0 };
                request
                    .trace
                    .event("cancel", json!({"win32Error":code,"cancelRequested":true}));
            },
            |request, remaining| {
                let wait = unsafe {
                    WaitForSingleObject(request.event.0, remaining.as_millis().max(1) as u32)
                };
                if wait != WAIT_OBJECT_0 && !request.immediate {
                    return false;
                }
                let done = unsafe {
                    GetOverlappedResult(
                        self.device.0.0,
                        request.overlap.get(),
                        request.bytes.get(),
                        0,
                    )
                };
                let code = if done != 0 {
                    0
                } else {
                    unsafe { GetLastError() }
                };
                let terminal = code != ERROR_IO_INCOMPLETE;
                if terminal {
                    let (nt, usb) =
                        protocol::status(unsafe { &*request.header.get() }).unwrap_or_default();
                    request.trace.event("complete", json!({"win32Error":code,"receivedBytes":if done != 0 {unsafe {*request.bytes.get()}} else {0},
                        "ntStatus":format!("0x{nt:08x}"),"usbStatus":format!("0x{usb:08x}"),
                        "cancelRequested":true,"terminalCompletionObserved":true}));
                }
                terminal
            },
        );
        if !drained {
            eprintln!(
                "Queued USB cancellation did not drain; terminating worker with buffers intact"
            );
            std::process::exit(125);
        }
    }
}

impl Drop for ReadBatch<'_> {
    fn drop(&mut self) {
        self.cancel_and_drain();
    }
}

impl Device {
    /// Queue a bounded first-pass frame on the already exclusive device handle.
    /// Controls may run on this same owner thread while requests are pending;
    /// pipe reset, close, and retained replay must wait until finish/drop drains.
    pub fn queue_frame(&self, length: usize, chunk_bytes: usize) -> Result<ReadBatch<'_>> {
        let depth = queue_depth(length, chunk_bytes)?;
        let mut batch = ReadBatch {
            device: self,
            data: vec![0; length],
            pending: VecDeque::with_capacity(depth),
            chunk_bytes,
            next_offset: 0,
            completed_bytes: 0,
        };
        for _ in 0..depth {
            batch.submit_next()?;
        }
        Ok(batch)
    }
    /// Cypress documented port operations, scoped to this device's upstream
    /// port. CLI research only; never part of automatic capture recovery.
    pub fn port_operation(&self, cycle: bool) -> Result<()> {
        let done = self.request(
            if cycle { 0x220028 } else { 0x220030 },
            &mut [],
            Some(&mut []),
            5000,
        )?;
        done.ensure_success("USB port operation", 0)?;
        Ok(())
    }
    pub fn vendor(&self, request: u8, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        let mut data = protocol::descriptor_request(0, length);
        data[0] = if length == 0 { 0x40 } else { 0xc0 };
        data[1] = request;
        data[2..4].copy_from_slice(&value.to_le_bytes());
        data[4..6].copy_from_slice(&index.to_le_bytes());
        let done = self.request(protocol::CONTROL, &mut data, None, 5000)?;
        done.ensure_success(&format!("vendor {request:02x}/{value:04x}"), data.len())?;
        Ok(protocol::descriptor_payload(&data, done.bytes, length as usize)?.to_vec())
    }

    pub fn reset_pipe(&self) -> Result<()> {
        for code in [0x220044, 0x22002c] {
            let done = self.request(code, &mut [0x81], Some(&mut []), 2000)?;
            done.ensure_success("pipe operation", 0)?;
        }
        let done = self.request(0x220038, &mut [0x81, 0, 0, 0x10, 0], None, 2000)?;
        done.ensure_success("transfer-size configuration", 5)?;
        Ok(())
    }

    pub fn read_chunk(&self, chunk: &mut [u8], timeout: u32) -> Result<()> {
        let mut header = [0; protocol::HEADER];
        header[13] = 0x81;
        let done = self
            .request(protocol::BULK, &mut header, Some(chunk), timeout)
            .map_err(|error| {
                let code = error
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error());
                let category = if code == Some(ERROR_DEVICE_NOT_CONNECTED as i32) {
                    "disconnected"
                } else {
                    "io"
                };
                let mut failure =
                    crate::asi::direct::transfer::Failure::new(category, chunk.len(), 0, false);
                failure.0["win32Error"] = json!(code);
                failure.0["cause"] = json!(format!("{error:#}"));
                anyhow::Error::new(failure)
            })?;
        let (nt, usb) = protocol::status(&header)?;
        if !(done.error == 0
            && !done.cancel_requested
            && nt == 0
            && usb == 0
            && done.bytes == chunk.len())
        {
            let category = if done.deadline_expired {
                "timeout"
            } else if done.cancel_requested {
                "io"
            } else if done.error == ERROR_OPERATION_ABORTED {
                "cancelled"
            } else if done.error == ERROR_DEVICE_NOT_CONNECTED {
                "disconnected"
            } else if done.error != 0 || nt != 0 || usb != 0 {
                "io"
            } else if done.bytes < chunk.len() {
                "short_read"
            } else {
                "overflow"
            };
            let mut failure = crate::asi::direct::transfer::Failure::new(
                category,
                chunk.len(),
                done.bytes,
                done.deadline_expired,
            );
            failure
                .0
                .as_object_mut()
                .unwrap()
                .extend(done.details().as_object().unwrap().clone());
            failure.0["ntStatus"] = json!(nt);
            failure.0["usbStatus"] = json!(usb);
            failure.0["terminalCompletionObserved"] = json!(true);
            failure.observe_short_read(chunk);
            return Err(failure.into());
        }
        Ok(())
    }

    pub fn open(info: &DeviceInfo) -> Result<Self> {
        let path = &info.0;
        ensure!(path.last() == Some(&0), "unterminated path");
        // Request exclusive ownership. Do not fall back to sharing with the SDK.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "open driver exclusively: {}",
            error()
        );
        Ok(Self(Handle(handle)))
    }

    fn request(
        &self,
        code: u32,
        input: &mut [u8],
        output: Option<&mut [u8]>,
        timeout_ms: u32,
    ) -> Result<Completion> {
        let out_size = output.as_ref().map_or(input.len(), |v| v.len());
        let mut fields = json!({"transport":"windows-cyusb3","ioctl":format!("0x{code:06x}"),
            "inputBytes":input.len(),"outputBytes":out_size,"timeoutMs":timeout_ms});
        let control = if code == protocol::CONTROL && input.len() >= protocol::HEADER {
            let request = input[1];
            let length = u16::from_le_bytes([input[6], input[7]]);
            let mut setup = diagnostics::control_fields(
                request,
                u16::from_le_bytes([input[2], input[3]]),
                u16::from_le_bytes([input[4], input[5]]),
                length,
            );
            setup["direction"] = json!(if input[0] & 0x80 != 0 { "in" } else { "out" });
            fields
                .as_object_mut()
                .unwrap()
                .extend(setup.as_object().unwrap().clone());
            Some((request, length))
        } else {
            None
        };
        if code == protocol::BULK {
            fields["endpoint"] = json!("0x81");
            fields["direction"] = json!("in");
            fields["requestedBytes"] = json!(out_size);
        }
        let operation = match code {
            protocol::CONTROL => "control",
            protocol::BULK => "bulk",
            protocol::VERSION => "driver-version",
            0x220044 => "abort-pipe",
            0x22002c => "reset-pipe",
            0x220038 => "set-transfer-size",
            0x220028 => "cycle-port",
            0x220030 => "reset-port",
            _ => "driver-operation",
        };
        if matches!(code, 0x220044 | 0x22002c | 0x220038) && input.len() <= 8 {
            fields["parameters"] = json!(input);
        }
        let trace = UsbTrace::start(operation, fields);
        let result = self.request_inner(code, input, output, timeout_ms, &trace);
        let mut completion = match &result {
            Ok(done) => done.details(),
            Err(error) => {
                json!({"win32Error":error.downcast_ref::<std::io::Error>().and_then(|e| e.raw_os_error()),
                "error":format!("{error:#}")})
            }
        };
        // request_inner cannot return while the driver still owns these bytes.
        if matches!(code, protocol::CONTROL | protocol::BULK) && input.len() >= protocol::HEADER {
            let (nt, usb) = protocol::status(input)?;
            completion["ntStatus"] = json!(format!("0x{nt:08x}"));
            completion["usbStatus"] = json!(format!("0x{usb:08x}"));
        }
        if let (Some((request, length)), Ok(done)) = (control, &result)
            && done.error == 0
            && !done.cancel_requested
            && let Ok(payload) =
                protocol::descriptor_payload(input, done.bytes, usize::from(length))
        {
            completion.as_object_mut().unwrap().extend(
                diagnostics::control_payload(request, length, payload)
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
        trace.event("complete", completion);
        result
    }

    fn request_inner(
        &self,
        code: u32,
        input: &mut [u8],
        output: Option<&mut [u8]>,
        timeout_ms: u32,
        trace: &UsbTrace,
    ) -> Result<Completion> {
        let out_size = output.as_ref().map_or(input.len(), |v| v.len());
        let out_ptr = output.map_or(input.as_mut_ptr(), |v| v.as_mut_ptr());
        let event_raw = unsafe { CreateEventW(null(), 1, 0, null()) };
        ensure!(!event_raw.is_null(), "create event: {}", error());
        let event = Handle(event_raw);
        let mut overlap: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlap.hEvent = event.0;
        let mut bytes = 0;
        // SAFETY: input/output, event and OVERLAPPED stay allocated until terminal
        // completion, including cancellation. No buffer can escape this function.
        unsafe {
            let ok = DeviceIoControl(
                self.0.0,
                code,
                if input.is_empty() {
                    null_mut()
                } else {
                    input.as_mut_ptr().cast()
                },
                input.len() as u32,
                if out_size == 0 {
                    null_mut()
                } else {
                    out_ptr.cast()
                },
                out_size as u32,
                &mut bytes,
                &mut overlap,
            );
            let submission_error = if ok == 0 { GetLastError() } else { 0 };
            trace.event(
                "submitted",
                json!({"win32Error":submission_error,
                "pending":submission_error == ERROR_IO_PENDING,"synchronous":ok != 0}),
            );
            if ok == 0 && submission_error != ERROR_IO_PENDING {
                return Err(std::io::Error::from_raw_os_error(submission_error as i32))
                    .context("submit driver request");
            }
            let mut cancel_requested = false;
            let mut deadline_expired = false;
            let mut cancellation_reason = None;
            let mut wait_error = None;
            let wait_result = if ok == 0 {
                WaitForSingleObject(event.0, timeout_ms)
            } else {
                WAIT_OBJECT_0
            };
            if wait_result != WAIT_OBJECT_0 {
                cancel_requested = true;
                deadline_expired = wait_result == WAIT_TIMEOUT;
                cancellation_reason = Some(if deadline_expired {
                    "deadline_expired"
                } else {
                    "wait_failed"
                });
                wait_error = (wait_result == WAIT_FAILED).then(|| GetLastError());
                let cancelled = CancelIoEx(self.0.0, &overlap);
                let cancel_error = if cancelled == 0 { GetLastError() } else { 0 };
                trace.event(
                    "cancel",
                    json!({"win32Error":cancel_error,"cancelRequested":true,
                        "deadlineExpired":deadline_expired,"cancellationReason":cancellation_reason,
                        "waitResult":wait_result,"waitError":wait_error}),
                );
                // A cancellation request is not completion. Never free pending memory.
                if WaitForSingleObject(event.0, 2000) != WAIT_OBJECT_0 {
                    eprintln!(
                        "Driver did not drain cancellation; terminating worker with buffers intact"
                    );
                    std::process::exit(125);
                }
            }
            let completed = GetOverlappedResult(self.0.0, &overlap, &mut bytes, 0);
            let code = if completed != 0 { 0 } else { GetLastError() };
            if code == ERROR_IO_INCOMPLETE {
                eprintln!("Driver signalled before terminal completion; terminating worker");
                std::process::exit(125);
            }
            ensure!(
                bytes as usize <= out_size || code != 0,
                "driver returned an invalid byte count"
            );
            Ok(Completion {
                bytes: if code == 0 { bytes as usize } else { 0 },
                error: code,
                cancel_requested,
                deadline_expired,
                cancellation_reason,
                wait_error,
            })
        }
    }

    fn descriptor(&self, kind: u8, length: u16) -> Result<Vec<u8>> {
        let mut data = protocol::descriptor_request(kind, length);
        let done = self.request(protocol::CONTROL, &mut data, None, 5000)?;
        done.ensure_success("descriptor", data.len())?;
        Ok(protocol::descriptor_payload(&data, done.bytes, length as usize)?.to_vec())
    }

    pub fn probe(&self) -> Result<Value> {
        let mut version = [0; 4];
        let done = self.request(protocol::VERSION, &mut version, None, 5000)?;
        done.ensure_success("driver version query", version.len())?;
        ensure!(
            done.bytes == version.len(),
            "driver version query returned an invalid byte count"
        );
        let device = self.descriptor(1, 18)?;
        let prefix = self.descriptor(2, 9)?;
        let config = self.descriptor(2, protocol::config_length(&prefix)? as u16)?;
        protocol::describe(&device, &config, u32::from_le_bytes(version))
    }

    pub fn cancel_read(&self) -> Result<Value> {
        let mut header = [0; protocol::HEADER];
        header[13] = 0x81;
        // DIRECT ABI: data in the separate output buffer; header offset/length 0.
        let mut bytes = vec![0; 16384];
        let done = self.request(protocol::BULK, &mut header, Some(&mut bytes), 100)?;
        let (nt, usb) = protocol::status(&header)?;
        Ok(
            json!({"requestedBytes":bytes.len(),"receivedBytes":done.bytes,
            "cancelRequested":done.cancel_requested,"win32Error":done.error,
            "ntStatus":format!("0x{nt:08x}"),"usbdStatus":format!("0x{usb:08x}"),
            "terminalCompletionObserved":true,"imageAccepted":false}),
        )
    }
}

#[cfg(test)]
mod queued_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn late_control_success_is_a_typed_timeout_with_uncertain_effect() {
        for error in [0, ERROR_OPERATION_ABORTED] {
            let done = Completion {
                bytes: if error == 0 { protocol::HEADER } else { 0 },
                error,
                cancel_requested: true,
                deadline_expired: true,
                cancellation_reason: Some("deadline_expired"),
                wait_error: None,
            };
            let error_value = done
                .ensure_success("vendor bd/0026", protocol::HEADER)
                .unwrap_err();
            let details = crate::asi::direct::transfer::Failure::details(&error_value);
            assert_eq!(details["category"], "timeout");
            assert_eq!(details["deadlineExpired"], true);
            assert_eq!(details["win32Error"], error);
            assert_eq!(details["lateCompletion"], error == 0);
            assert_eq!(details["cancellationReason"], "deadline_expired");
            assert_eq!(details["commandEffectUncertain"], true);
            assert_eq!(details["terminalCompletionObserved"], true);
        }
        let success = Completion {
            bytes: protocol::HEADER,
            error: 0,
            cancel_requested: false,
            deadline_expired: false,
            cancellation_reason: None,
            wait_error: None,
        };
        success
            .ensure_success("vendor bd/0026", protocol::HEADER)
            .unwrap();
        assert_eq!(success.details()["lateCompletion"], false);
        assert_eq!(success.details()["commandEffectUncertain"], false);
    }

    #[test]
    fn failed_wait_is_not_misreported_as_an_expired_deadline() {
        let done = Completion {
            bytes: 0,
            error: ERROR_OPERATION_ABORTED,
            cancel_requested: true,
            deadline_expired: false,
            cancellation_reason: Some("wait_failed"),
            wait_error: Some(ERROR_INVALID_HANDLE),
        };
        let error = done
            .ensure_success("vendor bc/0000", protocol::HEADER)
            .unwrap_err();
        let details = crate::asi::direct::transfer::Failure::details(&error);
        assert_eq!(details["deadlineExpired"], false);
        assert_eq!(details["cancellationReason"], "wait_failed");
        assert_eq!(details["waitError"], ERROR_INVALID_HANDLE);
        assert_eq!(details["win32Error"], ERROR_OPERATION_ABORTED);
    }

    struct MockRead {
        number: usize,
        incomplete_polls: usize,
        events: Rc<RefCell<Vec<String>>>,
    }
    impl Drop for MockRead {
        fn drop(&mut self) {
            self.events
                .borrow_mut()
                .push(format!("free {}", self.number));
        }
    }

    #[test]
    fn cancel_all_precedes_wait_and_free_requires_terminal_completion() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut reads: VecDeque<_> = (0..3)
            .map(|number| MockRead {
                number,
                incomplete_polls: 1,
                events: events.clone(),
            })
            .collect();
        let drained = drain_requests(
            &mut reads,
            Duration::from_secs(1),
            |read| {
                read.events
                    .borrow_mut()
                    .push(format!("cancel {}", read.number))
            },
            |read, timeout| {
                assert!(timeout <= Duration::from_millis(20));
                let terminal = read.incomplete_polls == 0;
                read.events
                    .borrow_mut()
                    .push(format!("terminal {} {terminal}", read.number));
                read.incomplete_polls = read.incomplete_polls.saturating_sub(1);
                terminal
            },
        );
        assert!(drained);
        assert!(reads.is_empty());
        assert_eq!(
            &*events.borrow(),
            &[
                "cancel 0",
                "cancel 1",
                "cancel 2",
                "terminal 0 false",
                "terminal 0 true",
                "free 0",
                "terminal 1 false",
                "terminal 1 true",
                "free 1",
                "terminal 2 false",
                "terminal 2 true",
                "free 2",
            ]
        );
    }

    #[test]
    fn undrained_timeout_retains_every_buffer() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut reads: VecDeque<_> = (0..2)
            .map(|number| MockRead {
                number,
                incomplete_polls: 0,
                events: events.clone(),
            })
            .collect();
        assert!(!drain_requests(
            &mut reads,
            Duration::ZERO,
            |read| read
                .events
                .borrow_mut()
                .push(format!("cancel {}", read.number)),
            |_, _| panic!("expired drain must not wait")
        ));
        assert_eq!(reads.len(), 2);
        assert_eq!(&*events.borrow(), &["cancel 0", "cancel 1"]);
        assert!(drain_requests(
            &mut reads,
            Duration::from_secs(1),
            |_| {},
            |_, _| true
        ));
        assert_eq!(
            &*events.borrow(),
            &["cancel 0", "cancel 1", "free 0", "free 1"]
        );
    }

    #[test]
    fn default_full_frame_matches_sdk_depth_and_tiny_chunks_are_bounded() {
        assert_eq!(queue_depth(9576 * 6388 * 2, 1024 * 1024).unwrap(), 117);
        assert_eq!(queue_depth(9576 * 6388 * 2, 1024).unwrap(), 128);
        assert_eq!(queue_depth(128 * 128 * 2, 1024 * 1024).unwrap(), 1);
        assert!(queue_depth(0, 1024).is_err());
        assert!(queue_depth(1024, 0).is_err());
        assert!(queue_depth(129 * 1024 * 1024, 1024).is_err());
    }

    #[test]
    fn completed_short_frame_with_trailer_is_rejected() {
        let buffer = [0x7e, 0x5a, 7, 0, 9, 0, 0xf0, 0x3c];
        let category = queued_read_failure(0, 0, 0, buffer.len(), 1024).unwrap();
        assert_eq!(category, "short_read");
        let mut failure =
            crate::asi::direct::transfer::Failure::new(category, 1024, buffer.len(), false);
        failure.observe_short_read(&buffer);
        assert_eq!(failure.0["frameTrailerObserved"], true);
        assert_eq!(failure.0["category"], "short_read");
        assert_eq!(queued_read_failure(0, 0, 0, 1024, 1024), None);
        assert_eq!(
            queued_read_failure(ERROR_OPERATION_ABORTED, 0, 0, 1024, 1024),
            Some("cancelled")
        );
        assert_eq!(queued_read_failure(0, 1, 0, 1024, 1024), Some("io"));
    }
}
