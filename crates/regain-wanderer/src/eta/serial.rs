use crate::eta::{Telemetry, Transport, command, parse};
use anyhow::{Context, Result};
pub use regain_transport::serial::Port;
use regain_transport::serial::{self, ClearBuffer, FlowControl, Selection, SerialPort, Settings};
use std::{
    io::Write,
    time::{Duration, Instant},
};
/// USB candidates only; the device protocol verifies identity before use.
pub fn candidates() -> Result<Vec<Port>> {
    serial::candidates(0x1a86, 0x7523, Selection::PortPath)
}
pub struct Serial {
    port: Box<dyn SerialPort>,
}
impl Serial {
    pub fn open(name: &str) -> Result<Self> {
        let port = Settings {
            baud: 19200,
            flow: FlowControl::None,
            timeout: Duration::from_millis(100),
            dtr: false,
            rts: Some(false),
            settle: Duration::from_millis(0),
            clear_input: false,
        }
        .open(name)?;
        Ok(Self { port })
    }
}
impl Transport for Serial {
    fn read(&mut self) -> Result<Telemetry> {
        // Discard stale telemetry before measuring completion of a command.
        self.port.clear(ClearBuffer::Input)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let bytes = serial::read_until(&mut self.port, b'\n', 256, deadline)?;
            // Opening/clearing can land in the middle of a streaming frame.
            if let Ok(wire) = std::str::from_utf8(&bytes[..bytes.len() - 1])
                && wire.starts_with("WandererTilter")
            {
                return parse(wire);
            }
        }
    }

    fn send(&mut self, point: usize, target_um: i32) -> Result<()> {
        self.port
            .write_all(command(point, target_um)?.as_bytes())
            .context("Write ETA target; outcome may be uncertain")
    }
}
