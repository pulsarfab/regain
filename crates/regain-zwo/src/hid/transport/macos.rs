use super::DeviceInfo;
use crate::hid::Transport;
use anyhow::{Result, ensure};
use std::{
    ffi::{c_char, c_void},
    ptr::null,
};

type Ref = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFRetain(value: Ref) -> Ref;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: isize, value: *mut c_void) -> bool;
    fn CFSetGetCount(set: Ref) -> isize;
    fn CFSetGetValues(set: Ref, values: *mut Ref);
}
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: Ref, options: u32) -> Ref;
    fn IOHIDManagerSetDeviceMatching(manager: Ref, dictionary: Ref);
    fn IOHIDManagerCopyDevices(manager: Ref) -> Ref;
    fn IOHIDDeviceGetProperty(device: Ref, key: Ref) -> Ref;
    fn IOHIDDeviceGetService(device: Ref) -> u32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IOHIDDeviceOpen(device: Ref, options: u32) -> i32;
    fn IOHIDDeviceClose(device: Ref, options: u32) -> i32;
    fn IOHIDDeviceSetReport(device: Ref, kind: u32, id: isize, data: *const u8, size: isize)
    -> i32;
    fn IOHIDDeviceGetReport(
        device: Ref,
        kind: u32,
        id: isize,
        data: *mut u8,
        size: *mut isize,
    ) -> i32;
}
struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CFRelease(self.0);
            }
        }
    }
}

fn number(device: Ref, name: &std::ffi::CStr) -> Result<i64> {
    // SAFETY: property is borrowed from the live device; key is a retained CF
    // string. Check the dynamic type before reading its integer value.
    unsafe {
        let key = Owned(CFStringCreateWithCString(null(), name.as_ptr(), 0x08000100));
        ensure!(!key.0.is_null(), "create HID property key");
        let property = IOHIDDeviceGetProperty(device, key.0);
        ensure!(
            !property.is_null() && CFGetTypeID(property) == CFNumberGetTypeID(),
            "missing numeric HID property {name:?}"
        );
        let mut result = 0_i64;
        ensure!(
            CFNumberGetValue(property, 4, (&mut result as *mut i64).cast()),
            "read HID property"
        );
        Ok(result)
    }
}

fn devices(vendor_id: u16, product_id: u16) -> Result<Vec<(DeviceInfo, Owned)>> {
    // SAFETY: manager and set are retained for enumeration; selected devices
    // are retained separately before their containing set is released.
    unsafe {
        let manager = Owned(IOHIDManagerCreate(null(), 0));
        ensure!(!manager.0.is_null(), "create HID manager");
        IOHIDManagerSetDeviceMatching(manager.0, null());
        let set = Owned(IOHIDManagerCopyDevices(manager.0));
        if set.0.is_null() {
            return Ok(Vec::new());
        }
        let count = CFSetGetCount(set.0);
        ensure!((0..=4096).contains(&count), "invalid HID device count");
        let mut handles = vec![null(); count as usize];
        CFSetGetValues(set.0, handles.as_mut_ptr());
        let mut result = Vec::new();
        for device in handles {
            if number(device, c"VendorID").ok() != Some(i64::from(vendor_id))
                || number(device, c"ProductID").ok() != Some(i64::from(product_id))
            {
                continue;
            }
            let mut id = 0;
            ensure!(
                IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(device), &mut id) == 0,
                "read HID registry identity"
            );
            result.push((
                DeviceInfo {
                    path: format!("IOHID:{id:016x}"),
                },
                Owned(CFRetain(device)),
            ));
        }
        result.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        Ok(result)
    }
}
pub fn enumerate(vendor_id: u16, product_id: u16) -> Result<Vec<DeviceInfo>> {
    Ok(devices(vendor_id, product_id)?
        .into_iter()
        .map(|(info, _)| info)
        .collect())
}

pub struct Device {
    handle: Owned,
    input_length: usize,
    output_length: usize,
}
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            IOHIDDeviceClose(self.handle.0, 1);
        }
    }
}
impl Device {
    pub fn open(info: &DeviceInfo, vendor_id: u16, product_id: u16) -> Result<Self> {
        let (_, handle) = devices(vendor_id, product_id)?
            .into_iter()
            .find(|(d, _)| d.path == info.path)
            .ok_or_else(|| anyhow::anyhow!("HID removed"))?;
        let input_length = number(handle.0, c"MaxInputReportSize")? as usize;
        let output_length = number(handle.0, c"MaxOutputReportSize")? as usize;
        ensure!(
            (16..=128).contains(&input_length) && (16..=128).contains(&output_length),
            "unexpected HID HID sizes {input_length}/{output_length}"
        );
        // Seize only the specifically selected HID, never other HID devices.
        ensure!(
            unsafe { IOHIDDeviceOpen(handle.0, 1) } == 0,
            "open HID exclusively"
        );
        Ok(Self {
            handle,
            input_length,
            output_length,
        })
    }
}
impl Transport for Device {
    fn set_output(&mut self, report: &[u8]) -> Result<()> {
        ensure!(
            report.len() <= self.output_length && report.first() == Some(&3),
            "invalid HID output report"
        );
        let mut b = vec![0; self.output_length];
        b[..report.len()].copy_from_slice(report);
        let code =
            unsafe { IOHIDDeviceSetReport(self.handle.0, 1, 3, b.as_ptr(), b.len() as isize) };
        ensure!(code == 0, "HID output report IOReturn {code:#x}");
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        let mut b = vec![0; self.input_length];
        b[0] = 1;
        let mut count = b.len() as isize;
        let code = unsafe { IOHIDDeviceGetReport(self.handle.0, 0, 1, b.as_mut_ptr(), &mut count) };
        ensure!(code == 0, "HID input report IOReturn {code:#x}");
        ensure!(
            (0..=b.len() as isize).contains(&count),
            "invalid HID report size"
        );
        b.truncate(count as usize);
        Ok(b)
    }
}
