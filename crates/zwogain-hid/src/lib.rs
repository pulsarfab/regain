pub mod transport;
pub use transport::{Device, DeviceInfo, enumerate};
/// Reports include their report ID at byte zero on all platforms.
pub trait Transport {
    fn set_output(&mut self, report: &[u8]) -> anyhow::Result<()>;
    fn get_input(&mut self) -> anyhow::Result<Vec<u8>>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn set_output(&mut self, report: &[u8]) -> anyhow::Result<()> {
        (**self).set_output(report)
    }
    fn get_input(&mut self) -> anyhow::Result<Vec<u8>> {
        (**self).get_input()
    }
}
