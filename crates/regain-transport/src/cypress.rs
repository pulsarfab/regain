//! Windows Cypress driver's public transfer ABI, independent of camera commands.
//! Every overlapped request reaches terminal completion before its storage is freed.
use anyhow::{Context, Result, ensure};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
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

const HEADER: usize = 38;
/// An IN control transfer completed with only its header and an empty device
/// reply. Callers may interpret this only where their protocol documents it.
#[derive(Debug)]
pub struct EmptyControlReply;
impl std::fmt::Display for EmptyControlReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cypress control input has no reply")
    }
}
impl std::error::Error for EmptyControlReply {}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
fn error() -> std::io::Error {
    std::io::Error::last_os_error()
}

pub fn enumerate(interface: u128, vendor: u16, product: u16) -> Result<Vec<String>> {
    let guid = GUID::from_u128(interface);
    // SAFETY: SetupAPI structures carry native sizes and aligned owned buffers.
    unsafe {
        let raw = SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        ensure!(raw != -1, "Enumerate Cypress interfaces: {}", error());
        let set = DeviceSet(raw);
        let mut paths = Vec::new();
        for index in 0..128 {
            let mut data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set.0, null(), &guid, index, &mut data) == 0 {
                if GetLastError() == ERROR_NO_MORE_ITEMS {
                    return Ok(paths);
                }
                return Err(error()).context("Enumerate Cypress interface");
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
                "Invalid interface size"
            );
            let mut storage = vec![0u64; (required as usize).div_ceil(8)];
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            ensure!(
                SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &data,
                    detail,
                    required,
                    null_mut(),
                    null_mut()
                ) != 0,
                "Read interface: {}",
                error()
            );
            let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            let text = std::slice::from_raw_parts(
                std::ptr::addr_of!((*detail).DevicePath).cast::<u16>(),
                (required as usize - offset) / 2,
            );
            let end = text
                .iter()
                .position(|&v| v == 0)
                .context("Unterminated interface")?;
            let path = String::from_utf16(&text[..end])?;
            if path
                .to_ascii_lowercase()
                .contains(&format!("vid_{vendor:04x}&pid_{product:04x}"))
            {
                paths.push(path)
            }
        }
        anyhow::bail!("Too many Cypress interfaces")
    }
}

pub struct Device {
    handle: Handle,
    trace: Option<fn(serde_json::Value)>,
}
impl Device {
    pub fn open(path: &str) -> Result<Self> {
        ensure!(!path.contains('\0'), "Invalid interface path");
        let path: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: terminated path; this device exclusively owns the handle.
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
            "Open Cypress device exclusively: {}",
            error()
        );
        Ok(Self {
            handle: Handle(handle),
            trace: None,
        })
    }
    pub fn set_trace(&mut self, trace: fn(serde_json::Value)) {
        self.trace = Some(trace);
    }
    fn request(
        &self,
        code: u32,
        input: &mut [u8],
        output: Option<&mut [u8]>,
        timeout_ms: u32,
    ) -> Result<usize> {
        let started = std::time::Instant::now();
        let mut record = serde_json::json!({"operation":"Cypress USB","ioctl":code,"timeoutMs":timeout_ms,"requestedBytes":output.as_ref().map_or(input.len(),|v|v.len())});
        if code == 0x220020 {
            record["setup"] = serde_json::json!(&input[..8]);
            if input[0] & 128 == 0 {
                record["payload"] = serde_json::json!(&input[38..]);
            }
        }
        let result = self.request_inner(code, input, output, timeout_ms);
        record["elapsedUs"] = serde_json::json!(started.elapsed().as_micros());
        match &result {
            Ok(n) => {
                record["receivedBytes"] = serde_json::json!(n);
                if code == 0x220020 && input[0] & 128 != 0 {
                    record["payload"] = serde_json::json!(&input[38..]);
                }
            }
            Err(e) => record["error"] = serde_json::json!(format!("{e:#}")),
        }
        if let Some(trace) = self.trace {
            trace(record)
        }
        result
    }
    fn request_inner(
        &self,
        code: u32,
        input: &mut [u8],
        output: Option<&mut [u8]>,
        timeout_ms: u32,
    ) -> Result<usize> {
        let out_size = output.as_ref().map_or(input.len(), |v| v.len());
        let out = output.map_or(input.as_mut_ptr(), |v| v.as_mut_ptr());
        ensure!(
            input.len() <= u32::MAX as usize && out_size <= u32::MAX as usize,
            "Oversized driver request"
        );
        let raw = unsafe { CreateEventW(null(), 1, 0, null()) };
        ensure!(!raw.is_null(), "Create USB event: {}", error());
        let event = Handle(raw);
        let mut overlap: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlap.hEvent = event.0;
        let mut bytes = 0;
        // SAFETY: all buffers, event and OVERLAPPED remain live until terminal
        // completion. On a broken driver that cannot drain, exit without unwind.
        unsafe {
            let ok = DeviceIoControl(
                self.handle.0,
                code,
                input.as_mut_ptr().cast(),
                input.len() as u32,
                out.cast(),
                out_size as u32,
                &mut bytes,
                &mut overlap,
            );
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return Err(error()).context("Submit USB request");
            }
            let wait = if ok != 0 {
                WAIT_OBJECT_0
            } else {
                WaitForSingleObject(event.0, timeout_ms)
            };
            if wait != WAIT_OBJECT_0 {
                CancelIoEx(self.handle.0, &overlap);
                if WaitForSingleObject(event.0, 2000) != WAIT_OBJECT_0 {
                    eprintln!(
                        "USB cancellation did not drain; terminating worker with buffers intact"
                    );
                    std::process::exit(125);
                }
            }
            let complete = GetOverlappedResult(self.handle.0, &overlap, &mut bytes, 0);
            let failure = if complete != 0 { 0 } else { GetLastError() };
            if failure == ERROR_IO_INCOMPLETE {
                std::process::exit(125)
            }
            // QHY's camera UART returns this bounded terminal completion while
            // its wheel is moving. Preserve the exact condition; unplug/timeouts
            // and arbitrary driver failures must never become a busy response.
            if wait == WAIT_OBJECT_0
                && failure == ERROR_GEN_FAILURE
                && code == 0x220020
                && bytes == HEADER as u32
                && input.len() == HEADER + 1
                && input[0] & 0x80 != 0
                && input[14..22] == [0; 8]
            {
                return Err(EmptyControlReply.into());
            }
            ensure!(
                wait == WAIT_OBJECT_0,
                "USB request deadline/wait failed ({wait:#x}); command effect uncertain"
            );
            ensure!(
                failure == 0,
                "USB request failed: {}",
                std::io::Error::from_raw_os_error(failure as i32)
            );
            ensure!(bytes as usize <= out_size, "Invalid USB completion count");
        }
        if matches!(code, 0x220020 | 0x22004b) {
            ensure!(input.len() >= HEADER, "Missing transfer header");
            let nt = u32::from_le_bytes(input[14..18].try_into().unwrap());
            let usb = u32::from_le_bytes(input[18..22].try_into().unwrap());
            ensure!(
                nt == 0 && usb == 0,
                "USB driver status NT={nt:#x} USBD={usb:#x}"
            );
        }
        Ok(bytes as usize)
    }
    pub fn control(
        &self,
        direction: u8,
        request: u8,
        value: u16,
        index: u16,
        payload: &mut [u8],
        timeout_ms: u32,
    ) -> Result<()> {
        ensure!(payload.len() <= u16::MAX as usize, "Control too large");
        let mut data = vec![0; HEADER + payload.len()];
        data[0] = direction;
        data[1] = request;
        data[2..4].copy_from_slice(&value.to_le_bytes());
        data[4..6].copy_from_slice(&index.to_le_bytes());
        data[6..8].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        data[8..12].copy_from_slice(&timeout_ms.div_ceil(1000).max(1).to_le_bytes());
        data[30..34].copy_from_slice(&(HEADER as u32).to_le_bytes());
        data[34..38].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        data[HEADER..].copy_from_slice(payload);
        let received = self.request(0x220020, &mut data, None, timeout_ms)?;
        ensure!(
            received == data.len(),
            "Short USB control reply: {received}/{}",
            data.len()
        );
        payload.copy_from_slice(&data[HEADER..]);
        Ok(())
    }
    pub fn read(&self, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize> {
        ensure!(
            endpoint & 0x80 != 0 && !data.is_empty(),
            "Invalid bulk input"
        );
        let mut header = [0; HEADER];
        header[13] = endpoint;
        self.request(0x22004b, &mut header, Some(data), timeout_ms)
    }
    pub fn transfer_size(&self, endpoint: u8, size: u32) -> Result<()> {
        let mut data = [0; 5];
        data[0] = endpoint;
        data[1..].copy_from_slice(&size.to_le_bytes());
        self.request(0x220038, &mut data, None, 5000)?;
        Ok(())
    }
    pub fn abort_pipe(&self, endpoint: u8) -> Result<()> {
        self.request(0x220044, &mut [endpoint], Some(&mut []), 5000)?;
        Ok(())
    }
}
