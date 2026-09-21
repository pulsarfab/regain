use super::{DeviceInfo, report_lengths};
use crate::hid::Transport;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    os::fd::AsRawFd,
    path::Path,
};

pub fn enumerate(vendor_id: u16, product_id: u16) -> Result<Vec<DeviceInfo>> {
    let root = Path::new("/sys/class/hidraw");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut devices = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::read_to_string(entry.path().join("device/uevent"))?;
        if metadata.lines().any(|line| {
            line.eq_ignore_ascii_case(&format!("HID_ID=0003:{vendor_id:08X}:{product_id:08X}"))
        }) {
            devices.push(DeviceInfo {
                path: format!("/dev/{}", entry.file_name().to_string_lossy()),
            });
        }
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(devices)
}

pub struct Device {
    file: File,
    input_length: usize,
    output_length: usize,
}
impl Device {
    pub fn open(info: &DeviceInfo, vendor_id: u16, product_id: u16) -> Result<Self> {
        ensure!(
            enumerate(vendor_id, product_id)?
                .iter()
                .any(|d| d.path == info.path),
            "not an attached HID hidraw node"
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&info.path)
            .context("open HID hidraw; check udev permissions")?;
        // Advisory lock serializes cooperating native clients; HID has no
        // exclusive hidraw open. Do not run the vendor SDK concurrently.
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "HID already in use"
        );
        let name = Path::new(&info.path)
            .file_name()
            .context("invalid hidraw path")?;
        let descriptor = fs::read(
            Path::new("/sys/class/hidraw")
                .join(name)
                .join("device/report_descriptor"),
        )?;
        let (input_length, output_length) = report_lengths(&descriptor)?;
        Ok(Self {
            file,
            input_length,
            output_length,
        })
    }
    fn report(&self, number: u8, buffer: &mut [u8]) -> Result<usize> {
        // Linux asm-generic ioctl layout (x86_64/aarch64); HIDIOCGINPUT and
        // HIDIOCSOUTPUT send control transfers, not interrupt-stream reads.
        let request = 0xc0000000_u64
            | ((buffer.len() as u64) << 16)
            | (u64::from(b'H') << 8)
            | u64::from(number);
        // SAFETY: fd is live; ioctl size exactly matches the writable allocation.
        let count = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request as libc::c_ulong,
                buffer.as_mut_ptr(),
            )
        };
        ensure!(
            count >= 0,
            "HID hidraw report: {}",
            std::io::Error::last_os_error()
        );
        Ok(count as usize)
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
        let count = self.report(0x0b, &mut b)?;
        ensure!(count == b.len(), "short HID output report: {count}");
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        let mut b = vec![0; self.input_length];
        b[0] = 1;
        let count = self.report(0x0a, &mut b)?;
        ensure!(count <= b.len(), "oversized HID input report");
        b.truncate(count);
        Ok(b)
    }
}
