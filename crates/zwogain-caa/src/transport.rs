//! CAA selection over the shared native HID transport.
use crate::{PRODUCT_ID, Transport, VENDOR_ID};
use anyhow::Result;
pub use zwogain_hid::DeviceInfo;
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    zwogain_hid::enumerate(VENDOR_ID, PRODUCT_ID)
}
pub struct Device(zwogain_hid::Device);
impl Device {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Ok(Self(zwogain_hid::Device::open(
            info, VENDOR_ID, PRODUCT_ID,
        )?))
    }
}
impl Transport for Device {
    fn set_output(&mut self, report: &[u8]) -> Result<()> {
        self.0.set_output(report)
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        self.0.get_input()
    }
}
