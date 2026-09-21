use anyhow::{Context, Result, ensure};
pub use serialport::{ClearBuffer, FlowControl, SerialPort};
use serialport::{DataBits, Parity, SerialPortType, StopBits};
use std::{
    io::Read,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Port {
    pub port: String,
    /// Selection key: a USB serial when available, or an explicitly chosen port path.
    pub serial: String,
}
#[derive(Debug, Clone, Copy)]
pub enum Selection {
    UsbSerial,
    PortPath,
}
/// These are candidates only. The vendor protocol must verify identity before use.
pub fn candidates(vid: u16, pid: u16, selection: Selection) -> Result<Vec<Port>> {
    Ok(serialport::available_ports()?
        .into_iter()
        .filter_map(|p| {
            let SerialPortType::UsbPort(usb) = p.port_type else {
                return None;
            };
            if usb.vid != vid || usb.pid != pid {
                return None;
            }
            let serial = match selection {
                Selection::UsbSerial => usb.serial_number.filter(|s| !s.is_empty())?,
                Selection::PortPath => p.port_name.clone(),
            };
            Some(Port {
                port: p.port_name,
                serial,
            })
        })
        .collect())
}
/// Explicit 8N1 settings: opening a port must not silently reset a device or change flow control.
pub struct Settings {
    pub baud: u32,
    pub flow: FlowControl,
    pub timeout: Duration,
    pub dtr: bool,
    pub rts: Option<bool>,
    pub settle: Duration,
    pub clear_input: bool,
}
impl Settings {
    pub fn open(&self, path: &str) -> Result<Box<dyn SerialPort>> {
        let mut port = serialport::new(path, self.baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(self.flow)
            .timeout(self.timeout)
            .open()
            .with_context(|| format!("Open serial port {path} exclusively"))?;
        port.write_data_terminal_ready(self.dtr)?;
        if let Some(rts) = self.rts {
            port.write_request_to_send(rts)?;
        }
        if !self.settle.is_zero() {
            std::thread::sleep(self.settle);
        }
        if self.clear_input {
            port.clear(ClearBuffer::Input)?;
        }
        Ok(port)
    }
}
/// Read one byte by a fixed overall deadline, tolerating short OS read timeouts.
/// The reader itself must have a bounded read timeout. No writes are retried here.
pub fn read_byte(reader: &mut impl Read, deadline: Instant) -> Result<u8> {
    loop {
        ensure!(Instant::now() < deadline, "Serial response timed out");
        let mut byte = [0];
        match reader.read_exact(&mut byte) {
            Ok(()) => return Ok(byte[0]),
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
            Err(e) => return Err(e).context("Read serial response"),
        }
    }
}
/// Read a frame including its delimiter. The size bound includes that delimiter.
pub fn read_until(
    reader: &mut impl Read,
    delimiter: u8,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        ensure!(bytes.len() < max, "Serial response exceeds {max} bytes");
        let byte = read_byte(reader, deadline)?;
        bytes.push(byte);
        if byte == delimiter {
            return Ok(bytes);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_frames_do_not_consume_the_next_response() {
        let mut input = &b"abc\nnext\n"[..];
        let end = Instant::now() + Duration::from_secs(1);
        assert_eq!(read_until(&mut input, b'\n', 4, end).unwrap(), b"abc\n");
        assert_eq!(input, b"next\n");
        assert!(read_until(&mut input, b'\n', 4, end).is_err());
        assert!(read_until(&mut &b"partial"[..], b'\n', 20, end).is_err());
        assert!(read_byte(&mut &b"x"[..], Instant::now()).is_err());
    }
    #[test]
    fn transient_reads_do_not_restart_the_deadline() {
        struct Timeout;
        impl Read for Timeout {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::TimedOut.into())
            }
        }
        assert!(read_byte(&mut Timeout, Instant::now() + Duration::from_millis(2)).is_err());
    }
}
