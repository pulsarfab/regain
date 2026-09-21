use crate::{Telemetry, Transport, command, parse};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serialport::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits,
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Serialize)]
pub struct Port {
    pub port: String,
    pub serial: String,
}
/// CH340 is a candidate only: accept a device after observing its M54 telemetry.
/// This hardware supplies no unique serial, so the selection key is the port path.
pub fn candidates() -> Result<Vec<Port>> {
    Ok(serialport::available_ports()?.into_iter().filter(|p| matches!(&p.port_type, SerialPortType::UsbPort(u) if u.vid==0x1a86 && u.pid==0x7523)).map(|p| Port { serial: p.port_name.clone(), port: p.port_name }).collect())
}
pub struct Serial {
    port: Box<dyn SerialPort>,
}
impl Serial {
    pub fn open(name: &str) -> Result<Self> {
        let mut port = serialport::new(name, 19200)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(Duration::from_millis(100))
            .open()
            .context("Open ETA serial port exclusively")?;
        port.write_data_terminal_ready(false)?;
        port.write_request_to_send(false)?;
        Ok(Self { port })
    }
}
impl Transport for Serial {
    fn read(&mut self) -> Result<Telemetry> {
        // Discard stale telemetry before measuring completion of a command.
        self.port.clear(ClearBuffer::Input)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut bytes = Vec::new();
        loop {
            ensure!(Instant::now() < deadline, "ETA telemetry timed out");
            let mut b = [0];
            match self.port.read_exact(&mut b) {
                Ok(()) => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(e).context("Read ETA telemetry"),
            }
            ensure!(bytes.len() < 256, "ETA frame exceeds 256 bytes");
            if b[0] == b'\n' {
                // Opening/clearing can land in the middle of a streaming frame.
                if let Ok(wire) = std::str::from_utf8(&bytes)
                    && wire.starts_with("WandererTilter")
                {
                    return parse(wire);
                }
                bytes.clear();
            } else {
                bytes.push(b[0]);
            }
        }
    }
    fn send(&mut self, point: usize, target_um: i32) -> Result<()> {
        self.port
            .write_all(command(point, target_um)?.as_bytes())
            .context("Write ETA target; outcome may be uncertain")
    }
}
