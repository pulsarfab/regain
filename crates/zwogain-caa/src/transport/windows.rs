use super::DeviceInfo;
use crate::{PRODUCT_ID, Transport, VENDOR_ID};
use anyhow::{Context, Result, ensure};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
    },
    core::GUID,
};

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

pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    // SAFETY: all SetupAPI storage has native alignment and checked byte lengths.
    unsafe {
        let mut guid: GUID = std::mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = DeviceSet(SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ));
        ensure!(
            set.0 != -1,
            "HID enumeration: {}",
            std::io::Error::last_os_error()
        );
        let mut result = Vec::new();
        for index in 0..4096 {
            let mut data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set.0, null(), &guid, index, &mut data) == 0 {
                ensure!(
                    GetLastError() == ERROR_NO_MORE_ITEMS,
                    "HID enumeration failed"
                );
                return Ok(result);
            }
            let mut required = 0;
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &data,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
            ensure!(
                GetLastError() == ERROR_INSUFFICIENT_BUFFER && (8..=65536).contains(&required),
                "invalid HID path size"
            );
            let mut storage = vec![0_u64; (required as usize).div_ceil(8)];
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
                "HID path lookup failed"
            );
            let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            let text = std::slice::from_raw_parts(
                std::ptr::addr_of!((*detail).DevicePath).cast::<u16>(),
                (required as usize - offset) / 2,
            );
            let end = text
                .iter()
                .position(|v| *v == 0)
                .context("unterminated HID path")?;
            let path = String::from_utf16_lossy(&text[..end]);
            if path
                .to_ascii_lowercase()
                .contains(&format!("vid_{VENDOR_ID:04x}&pid_{PRODUCT_ID:04x}"))
            {
                result.push(DeviceInfo { path });
            }
        }
        anyhow::bail!("too many HID interfaces")
    }
}

pub struct Device {
    handle: Handle,
    input_length: usize,
    output_length: usize,
}
impl Device {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        ensure!(!info.path.contains('\0'), "invalid HID path");
        let path: Vec<u16> = info.path.encode_utf16().chain([0]).collect();
        // Exclusive access prevents another SDK connection consuming our replies.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        ensure!(
            raw != INVALID_HANDLE_VALUE,
            "open CAA exclusively: {}",
            std::io::Error::last_os_error()
        );
        let handle = Handle(raw);
        // SAFETY: SDK-free HID metadata; the preparsed allocation is freed even
        // when capability parsing fails, before returning any error.
        unsafe {
            let mut attrs: HIDD_ATTRIBUTES = std::mem::zeroed();
            attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
            ensure!(
                HidD_GetAttributes(handle.0, &mut attrs),
                "read HID identity"
            );
            ensure!(
                attrs.VendorID == VENDOR_ID && attrs.ProductID == PRODUCT_ID,
                "not a ZWO CAA"
            );
            let mut preparsed = 0;
            ensure!(
                HidD_GetPreparsedData(handle.0, &mut preparsed),
                "read HID descriptor"
            );
            let mut caps: HIDP_CAPS = std::mem::zeroed();
            let status = HidP_GetCaps(preparsed, &mut caps);
            HidD_FreePreparsedData(preparsed);
            ensure!(status == HIDP_STATUS_SUCCESS, "parse HID capabilities");
            let input_length = caps.InputReportByteLength as usize;
            let output_length = caps.OutputReportByteLength as usize;
            ensure!(
                (16..=128).contains(&input_length) && (16..=128).contains(&output_length),
                "unexpected CAA report lengths {input_length}/{output_length}"
            );
            Ok(Self {
                handle,
                input_length,
                output_length,
            })
        }
    }
}
impl Transport for Device {
    fn set_output(&mut self, report: &[u8]) -> Result<()> {
        ensure!(
            report.len() <= self.output_length && report.first() == Some(&3),
            "invalid CAA output report"
        );
        let mut buffer = vec![0; self.output_length];
        buffer[..report.len()].copy_from_slice(report);
        // SAFETY: handle and buffer remain alive for this synchronous HID call.
        ensure!(
            unsafe {
                HidD_SetOutputReport(self.handle.0, buffer.as_ptr().cast(), buffer.len() as u32)
            },
            "CAA output report: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        let mut buffer = vec![0; self.input_length];
        buffer[0] = 1;
        // SAFETY: exclusive handle, writable buffer of the descriptor's length.
        ensure!(
            unsafe {
                HidD_GetInputReport(
                    self.handle.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                )
            },
            "CAA input report: {}",
            std::io::Error::last_os_error()
        );
        Ok(buffer)
    }
}
