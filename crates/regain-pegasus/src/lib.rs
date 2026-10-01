//! Pegasus device protocols and simulations.
pub mod falcon;
pub mod fc3;
pub mod serial;

pub trait Transport {
    fn exchange(&mut self, command: &str) -> anyhow::Result<String>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn exchange(&mut self, command: &str) -> anyhow::Result<String> {
        (**self).exchange(command)
    }
}
