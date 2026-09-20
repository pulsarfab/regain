use crate::Transport;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serialport::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits,
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct Port {
    pub port: String,
    pub serial: String,
}
/// Candidate RP2040 CDC ports. Always verify GFRM + GPRD before actuating.
pub fn candidates() -> Result<Vec<Port>> {
    Ok(serialport::available_ports()?
        .into_iter()
        .filter_map(|p| {
            if let SerialPortType::UsbPort(usb) = p.port_type
                && usb.vid == 0x2e8a
                && usb.pid == 0x000a
            {
                usb.serial_number
                    .filter(|s| !s.is_empty())
                    .map(|serial| Port {
                        port: p.port_name,
                        serial,
                    })
            } else {
                None
            }
        })
        .collect())
}
pub struct Serial {
    port: Box<dyn SerialPort>,
}
impl Serial {
    pub fn open(port: &str) -> Result<Self> {
        let mut port = serialport::new(port, 115200)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::Hardware)
            .timeout(Duration::from_millis(50))
            .open()
            .context("Open OFP2 serial port exclusively")?;
        port.write_data_terminal_ready(true)?;
        std::thread::sleep(Duration::from_millis(1500));
        port.clear(ClearBuffer::Input)?;
        Ok(Self { port })
    }
}
/// Read a bounded ASCII response; malformed/partial frames fail the session.
pub fn read_frame(reader: &mut impl Read) -> Result<String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut bytes = Vec::new();
    loop {
        ensure!(Instant::now() < deadline, "Serial response timed out");
        ensure!(
            bytes.len() < MAX_FRAME,
            "Serial response exceeds {MAX_FRAME} bytes"
        );
        let mut byte = [0];
        match reader.read_exact(&mut byte) {
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
            Err(e) => return Err(e).context("Read OFP2 response"),
        }
        ensure!(
            byte[0].is_ascii_graphic() || byte[0] == b' ',
            "Non-ASCII serial response"
        );
        bytes.push(byte[0]);
        if byte[0] == b')' {
            break;
        }
    }
    let wire = String::from_utf8(bytes)?;
    if wire.starts_with('!') {
        bail!("OFP2 rejected command: {wire}");
    }
    ensure!(wire.starts_with('('), "Invalid serial frame: {wire}");
    let payload = &wire[1..wire.len() - 1];
    ensure!(
        !payload.is_empty() && !payload.contains(['(', ')']),
        "Invalid serial payload: {wire}"
    );
    Ok(payload.into())
}
impl Transport for Serial {
    fn exchange(&mut self, command: &str) -> Result<String> {
        ensure!(
            !command.is_empty()
                && command
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "Invalid command"
        );
        std::thread::sleep(Duration::from_millis(50));
        self.port.set_timeout(TIMEOUT)?;
        self.port
            .write_all(format!("[{command}]").as_bytes())
            .context("Write OFP2 command; outcome may be uncertain")?;
        self.port.set_timeout(Duration::from_millis(50))?;
        read_frame(&mut self.port)
    }
}
