use crate::ofp2::Transport;
use anyhow::{Context, Result, bail, ensure};
pub use regain_transport::serial::Port;
use regain_transport::serial::{self, FlowControl, Selection, SerialPort, Settings};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME: usize = 256;

/// USB candidates only; the device protocol verifies identity before use.
pub fn candidates() -> Result<Vec<Port>> {
    serial::candidates(0x2e8a, 0x000a, Selection::UsbSerial)
}
pub struct Serial {
    port: Box<dyn SerialPort>,
}
impl Serial {
    pub fn open(port: &str) -> Result<Self> {
        let port = Settings {
            baud: 115200,
            flow: FlowControl::Hardware,
            timeout: Duration::from_millis(50),
            dtr: true,
            rts: None,
            settle: Duration::from_millis(1500),
            clear_input: true,
        }
        .open(port)?;
        Ok(Self { port })
    }
}
/// Read a bounded ASCII response; malformed/partial frames fail the session.
pub fn read_frame(reader: &mut impl Read) -> Result<String> {
    let deadline = Instant::now() + TIMEOUT;
    let bytes = serial::read_until(reader, b')', MAX_FRAME, deadline)?;
    ensure!(
        bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' '),
        "Non-ASCII serial response"
    );
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
