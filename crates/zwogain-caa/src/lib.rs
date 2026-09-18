//! ZWO CAA control through OS HID reports, with no vendor SDK.
//!
//! A connection serializes command/reply pairs through `&mut self`. Motion
//! writes are never retried: an uncertain write may already have moved hardware.
//! Logical sync is local to this connection and never resets the mechanical zero.

use anyhow::{Result, bail, ensure};
use serde::Serialize;
use std::{
    thread,
    time::{Duration, Instant},
};

mod temperature;
pub mod transport;

pub const VENDOR_ID: u16 = 0x03c3;
pub const PRODUCT_ID: u16 = 0x1f20;

/// Reports include the report ID at byte zero, on every platform.
pub trait Transport {
    fn set_output(&mut self, report: &[u8]) -> Result<()>;
    fn get_input(&mut self) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: u8,
    pub moving: bool,
    pub direction: u8,
    pub mechanical_degrees: f64,
    pub logical_degrees: f64,
    pub temperature_adc: u16,
    pub temperature_c: Option<f64>,
    pub limit_degrees: u16,
    /// 0=none, 1=general, 2=stall, 3=timeout, 4=limit.
    pub error: u8,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Settings {
    pub beep: bool,
    pub reverse: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub firmware: [u8; 3],
    pub model: String,
    pub serial: String,
    pub alias: String,
}

pub struct Caa<T: Transport> {
    io: T,
    offset: f64,
    reverse: bool,
    pending_reverse: Option<(f64, f64)>,
}

fn header(command: u8) -> [u8; 16] {
    let mut r = [0; 16];
    r[..4].copy_from_slice(&[3, 0x7e, 0x5a, command]);
    r
}

fn validate_reply(r: &[u8], command: u8) -> Result<()> {
    ensure!(r.len() >= 16, "short CAA input report: {} bytes", r.len());
    ensure!(
        r[..4] == [1, 0x7e, 0x5a, command],
        "unexpected CAA reply header: {:02x?}",
        &r[..4]
    );
    Ok(())
}

fn angle(value: f64) -> Result<()> {
    ensure!(
        value.is_finite() && (0.0..=360.0).contains(&value),
        "angle must be finite and between 0 and 360 degrees"
    );
    Ok(())
}

fn wrap_mechanical(value: f64) -> f64 {
    let wrapped = value.rem_euclid(360.0);
    // Mechanical 360 is a distinct cable-limit endpoint from zero. Match the
    // SDK's inclusive normalization instead of wrapping an exact 360 to zero.
    if wrapped == 0.0 && value > 0.0 {
        360.0
    } else {
        wrapped
    }
}

impl<T: Transport> Caa<T> {
    /// Reads settings only. Opening never stops or starts motion.
    pub fn connect(io: T) -> Result<Self> {
        let mut caa = Self {
            io,
            offset: 0.0,
            reverse: false,
            pending_reverse: None,
        };
        caa.reverse = caa.settings()?.reverse;
        Ok(caa)
    }

    fn query(&mut self, command: u8) -> Result<Vec<u8>> {
        let mut r = header(2);
        r[4] = command;
        self.io.set_output(&r)?;
        // SDK 1.5.9 waits for metadata; status and settings have no added delay.
        if !matches!(command, 3 | 8) {
            thread::sleep(Duration::from_millis(200));
        }
        let reply = self.io.get_input()?;
        validate_reply(&reply, command)?;
        Ok(reply)
    }

    fn write(&mut self, r: &[u8]) -> Result<()> {
        self.io.set_output(r)?;
        thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    pub fn settings(&mut self) -> Result<Settings> {
        let r = self.query(8)?;
        ensure!(r[4] <= 1 && r[5] <= 1, "invalid CAA boolean settings");
        let settings = Settings {
            beep: r[4] != 0,
            reverse: r[5] != 0,
        };
        if let Some((mechanical, logical)) = self.pending_reverse.take() {
            self.reverse = settings.reverse;
            self.offset = logical - self.sign() * mechanical;
        }
        Ok(settings)
    }

    pub fn status(&mut self) -> Result<Status> {
        // A failed reverse write may have reached hardware. Resolve its actual
        // setting before reporting or using a logical coordinate again.
        if self.pending_reverse.is_some() {
            self.settings()?;
        }
        let r = self.query(3)?;
        let mechanical = u32::from_be_bytes(r[6..10].try_into().unwrap()) as f64 / 10000.0;
        angle(mechanical)?;
        let adc = u16::from_be_bytes([r[11], r[12]]);
        Ok(Status {
            state: r[4],
            moving: r[4] != 0,
            direction: r[5],
            mechanical_degrees: mechanical,
            logical_degrees: (self.sign() * mechanical + self.offset).rem_euclid(360.0),
            temperature_adc: adc,
            temperature_c: temperature::from_adc(adc),
            limit_degrees: u16::from_be_bytes([r[13], r[14]]),
            error: r[15],
        })
    }

    fn idle(&mut self) -> Result<Status> {
        let s = self.status()?;
        ensure!(!s.moving, "CAA is moving (state {})", s.state);
        ensure!(s.error == 0, "CAA reports fault {}", s.error);
        Ok(s)
    }

    fn sign(&self) -> f64 {
        if self.reverse { -1.0 } else { 1.0 }
    }

    pub fn identity(&mut self) -> Result<Identity> {
        let info = self.query(4)?;
        let serial = self.query(12)?;
        let alias = self.query(13)?;
        Ok(Identity {
            firmware: info[4..7].try_into().unwrap(),
            model: String::from_utf8_lossy(&info[8..info.len().min(18)])
                .trim_end_matches('\0')
                .to_owned(),
            serial: serial[4..12].iter().map(|b| format!("{b:02x}")).collect(),
            alias: String::from_utf8_lossy(&alias[4..12])
                .trim_end_matches('\0')
                .to_owned(),
        })
    }

    pub fn move_mechanical(&mut self, degrees: f64) -> Result<()> {
        angle(degrees)?;
        let s = self.idle()?;
        ensure!(
            degrees <= f64::from(s.limit_degrees),
            "target exceeds CAA limit {}",
            s.limit_degrees
        );
        let mut r = header(3);
        r[4] = 1;
        r[5] = s.direction;
        // Match the SDK's float-to-integer conversion, including truncation.
        let units = ((degrees as f32) * 10000.0) as u32;
        r[6..10].copy_from_slice(&units.to_be_bytes());
        r[14..16].copy_from_slice(&s.limit_degrees.to_be_bytes());
        self.write(&r)
    }

    pub fn move_to(&mut self, degrees: f64) -> Result<()> {
        angle(degrees)?;
        if self.pending_reverse.is_some() {
            self.settings()?;
        }
        let target = wrap_mechanical((degrees - self.offset) * self.sign());
        self.move_mechanical(target)
    }

    pub fn move_relative(&mut self, degrees: f64) -> Result<()> {
        ensure!(
            degrees.is_finite() && degrees.abs() <= 360.0,
            "relative angle must be finite and within one turn"
        );
        let s = self.idle()?;
        self.move_mechanical(wrap_mechanical(
            s.mechanical_degrees + self.sign() * degrees,
        ))
    }

    /// Local logical sync, including zero. Unlike CAACurDegree(0), this does
    /// not rewrite the device's mechanical position or cable-wrap reference.
    pub fn sync(&mut self, degrees: f64) -> Result<()> {
        angle(degrees)?;
        let s = self.idle()?;
        self.offset = degrees - self.sign() * s.mechanical_degrees;
        Ok(())
    }

    /// Stop is available even when a status read fails; no preflight query.
    pub fn stop(&mut self) -> Result<()> {
        let mut r = header(3);
        r[4] = 2;
        self.write(&r)
    }

    pub fn set_beep(&mut self, enabled: bool) -> Result<()> {
        self.idle()?;
        let mut r = header(7);
        r[4] = u8::from(enabled);
        self.write(&r)?;
        ensure!(
            self.settings()?.beep == enabled,
            "CAA did not accept beep setting"
        );
        Ok(())
    }

    pub fn set_reverse(&mut self, enabled: bool) -> Result<()> {
        let s = self.idle()?;
        let mut r = header(9);
        r[4] = u8::from(enabled);
        self.pending_reverse = Some((s.mechanical_degrees, s.logical_degrees));
        let write_result = self.write(&r);
        let settings_result = self.settings();
        write_result?;
        let actual = settings_result?.reverse;
        ensure!(actual == enabled, "CAA did not accept reverse setting");
        Ok(())
    }

    /// Persistent device alias. Empty clears it; no firmware or serial writes.
    pub fn set_alias(&mut self, alias: &str) -> Result<()> {
        ensure!(
            alias.len() <= 8 && alias.bytes().all(|b| (0x20..=0x7e).contains(&b)),
            "alias must be at most eight printable ASCII characters"
        );
        self.idle()?;
        let mut r = header(13);
        r[4..4 + alias.len()].copy_from_slice(alias.as_bytes());
        self.write(&r)?;
        let reply = self.query(13)?;
        ensure!(reply[4..12] == r[4..12], "CAA did not accept alias");
        Ok(())
    }

    pub fn set_limit(&mut self, degrees: u16) -> Result<()> {
        ensure!(
            (1..=360).contains(&degrees),
            "limit must be 1..360 whole degrees"
        );
        let s = self.idle()?;
        ensure!(
            s.mechanical_degrees <= f64::from(degrees),
            "limit would exclude current position"
        );
        let mut r = header(3);
        r[5] = s.direction;
        r[6..8].copy_from_slice(&3000_u16.to_be_bytes());
        r[10] = 2;
        r[14..16].copy_from_slice(&degrees.to_be_bytes());
        self.write(&r)?;
        ensure!(
            self.status()?.limit_degrees == degrees,
            "CAA did not accept rotation limit"
        );
        Ok(())
    }

    /// Wait for a caller-specified mechanical target. On failure or deadline,
    /// attempt a stop, never resend the movement command.
    pub fn wait_for(&mut self, target: f64, timeout: Duration) -> Result<Status> {
        angle(target)?;
        ensure!(!timeout.is_zero(), "wait timeout must be positive");
        let deadline = Instant::now() + timeout;
        let result = (|| {
            loop {
                let s = self.status()?;
                ensure!(s.error == 0, "CAA motion fault {}", s.error);
                if !s.moving && (s.mechanical_degrees - target).abs() <= 0.1 {
                    return Ok(s);
                }
                if Instant::now() >= deadline {
                    bail!("CAA did not reach target before deadline");
                }
                thread::sleep(Duration::from_millis(100));
            }
        })();
        if result.is_err() {
            let _ = self.stop();
        }
        result
    }
}
