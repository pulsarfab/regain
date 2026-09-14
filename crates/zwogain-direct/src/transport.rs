use crate::protocol;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
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

pub fn enumerate() -> Result<Vec<Vec<u16>>> {
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
            paths.push(text[..=end].to_vec());
        }
        bail!("too many driver interfaces")
    }
}

pub struct Camera(Handle);
struct Completion {
    bytes: usize,
    error: u32,
    cancel_requested: bool,
}
impl Camera {
    pub fn vendor(&self, request: u8, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        let mut data = protocol::descriptor_request(0, length);
        data[0] = if length == 0 { 0x40 } else { 0xc0 };
        data[1] = request;
        data[2..4].copy_from_slice(&value.to_le_bytes());
        data[4..6].copy_from_slice(&index.to_le_bytes());
        let done = self.request(protocol::CONTROL, &mut data, None, 5000)?;
        ensure!(
            done.error == 0 && !done.cancel_requested,
            "vendor {request:02x}/{value:04x} failed: {}",
            done.error
        );
        Ok(protocol::descriptor_payload(&data, done.bytes, length as usize)?.to_vec())
    }

    pub fn reset_pipe(&self) -> Result<()> {
        for code in [0x220044, 0x22002c] {
            let done = self.request(code, &mut [0x81], Some(&mut []), 2000)?;
            ensure!(
                done.error == 0 && !done.cancel_requested,
                "pipe operation failed: {}",
                done.error
            );
        }
        let done = self.request(0x220038, &mut [0x81, 0, 0, 0x10, 0], None, 2000)?;
        ensure!(
            done.error == 0 && !done.cancel_requested,
            "transfer-size configuration failed"
        );
        Ok(())
    }

    pub fn read_frame(&self, length: usize) -> Result<Vec<u8>> {
        ensure!(
            length > 0 && length <= 128 * 1024 * 1024,
            "invalid research frame size"
        );
        let mut data = vec![0; length];
        for (number, chunk) in data.chunks_mut(1024 * 1024).enumerate() {
            let mut header = [0; protocol::HEADER];
            header[13] = 0x81;
            let done = self.request(protocol::BULK, &mut header, Some(chunk), 5000)?;
            let (nt, usb) = protocol::status(&header)?;
            ensure!(
                done.error == 0
                    && !done.cancel_requested
                    && nt == 0
                    && usb == 0
                    && done.bytes == chunk.len(),
                "bulk chunk {number} failed: Win32 {}, NT {nt:08x}, USB {usb:08x}, bytes {}/{}",
                done.error,
                done.bytes,
                chunk.len()
            );
        }
        Ok(data)
    }

    pub fn open(path: &[u16]) -> Result<Self> {
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
                input.as_mut_ptr().cast(),
                input.len() as u32,
                out_ptr.cast(),
                out_size as u32,
                &mut bytes,
                &mut overlap,
            );
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return Err(error()).context("submit driver request");
            }
            let mut cancel_requested = false;
            if ok == 0 && WaitForSingleObject(event.0, timeout_ms) != WAIT_OBJECT_0 {
                cancel_requested = true;
                CancelIoEx(self.0.0, &overlap);
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
            })
        }
    }

    fn descriptor(&self, kind: u8, length: u16) -> Result<Vec<u8>> {
        let mut data = protocol::descriptor_request(kind, length);
        let done = self.request(protocol::CONTROL, &mut data, None, 5000)?;
        ensure!(
            done.error == 0 && !done.cancel_requested,
            "descriptor failed/timed out: Win32 {}",
            done.error
        );
        Ok(protocol::descriptor_payload(&data, done.bytes, length as usize)?.to_vec())
    }

    pub fn probe(&self) -> Result<Value> {
        let mut version = [0; 4];
        let done = self.request(protocol::VERSION, &mut version, None, 5000)?;
        ensure!(
            done.error == 0 && !done.cancel_requested && done.bytes == 4,
            "driver version query failed"
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
