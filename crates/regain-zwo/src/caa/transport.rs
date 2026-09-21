//! CAA selection over the shared native HID transport.
use crate::caa::{PRODUCT_ID, Transport, VENDOR_ID};
pub use crate::hid::DeviceInfo;
use anyhow::Result;
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    crate::hid::enumerate(VENDOR_ID, PRODUCT_ID)
}
pub struct Device(crate::hid::Device);
impl Device {
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Ok(Self(crate::hid::Device::open(info, VENDOR_ID, PRODUCT_ID)?))
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
