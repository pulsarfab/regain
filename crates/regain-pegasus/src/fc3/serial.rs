use crate::fc3::Transport;
use anyhow::{Context, Result, ensure};
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
    serial::candidates(0x303a, 0x9000, Selection::UsbSerial)
}
pub struct Serial {
    port: Box<dyn SerialPort>,
}
impl Serial {
    pub fn open(port: &str) -> Result<Self> {
        let port = Settings {
            baud: 115200,
            flow: FlowControl::None,
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
    let mut bytes = serial::read_until(reader, b'\n', MAX_FRAME, deadline)?;
    bytes.pop(); // newline is not part of the FocusCube3 payload.
    ensure!(
        bytes
            .iter()
            .all(|b| b.is_ascii_graphic() || *b == b' ' || *b == b'\r'),
        "Non-ASCII serial response"
    );
    let wire = String::from_utf8(bytes)?;
    let wire = wire.strip_suffix('\r').unwrap_or(&wire);
    ensure!(
        !wire.is_empty() && !wire.contains('\r'),
        "Invalid serial frame"
    );
    Ok(wire.into())
}

impl Transport for Serial {
    fn exchange(&mut self, command: &str) -> Result<String> {
        ensure!(
            !command.is_empty()
                && command.bytes().all(|b| b.is_ascii_uppercase()
                    || b.is_ascii_digit()
                    || b == b':'
                    || b == b'#'),
            "Invalid command"
        );
        std::thread::sleep(Duration::from_millis(50));
        self.port.set_timeout(TIMEOUT)?;
        self.port
            .write_all(format!("{command}\n").as_bytes())
            .context("Write FocusCube3 command; outcome may be uncertain")?;
        self.port.set_timeout(Duration::from_millis(50))?;
        read_frame(&mut self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing() {
        assert_eq!(read_frame(&mut &b"FA:1\r\n"[..]).unwrap(), "FA:1");
        for bytes in [b"\n".as_slice(), b"abc\rxyz\n", b"\xff\n", b"partial"] {
            assert!(read_frame(&mut &*bytes).is_err());
        }
        assert!(read_frame(&mut &vec![b'x'; 257][..]).is_err());
    }
}
