//! EFW/EAF control reports verified against owned SDK traces. No vendor DLL.
//! Commands are serialized and motion writes are never replayed after errors.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    thread,
    time::{Duration, Instant},
};
pub use zwogain_hid::Transport;
pub mod simulation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Efw,
    Eaf,
}
impl Kind {
    pub fn product_id(self) -> u16 {
        match self {
            Self::Efw => 0x1f01,
            Self::Eaf => 0x1f10,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Efw => "efw",
            Self::Eaf => "eaf",
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub kind: Kind,
    pub serial: String,
    pub model: String,
    pub firmware: [u8; 3],
}
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub position: i32,
    pub moving: bool,
    pub slots: u8,
    pub max_step: u32,
    pub backlash: u8,
    pub beep: bool,
    pub reverse: bool,
    pub hand_control: bool,
    pub temperature_c: Option<f64>,
    pub error: u8,
}

pub struct Accessory<T: Transport> {
    transport: T,
    pub kind: Kind,
    pub identity: Identity,
}
/// Tracks one explicit calibration. Provisional slot counts must never replace
/// the saved filter configuration, and an initial stale idle is not completion.
pub struct Calibration {
    pub slots: u8,
    deadline: Instant,
    saw_moving: bool,
}
impl Calibration {
    pub fn observe(&mut self, status: &Status, now: Instant) -> Result<bool> {
        ensure!(
            now < self.deadline,
            "EFW calibration timed out after 90 seconds; inspect hardware before reconnecting"
        );
        ensure!(
            status.error == 0,
            "EFW calibration hardware error {}",
            status.error
        );
        self.saw_moving |= status.moving;
        if !status.moving && self.saw_moving {
            ensure!(
                status.slots == self.slots,
                "EFW calibration slot count changed from {} to {}; inspect the wheel",
                self.slots,
                status.slots
            );
            ensure!(
                status.position == 0,
                "EFW calibration did not finish at slot 1"
            );
            return Ok(true);
        }
        Ok(false)
    }
}
impl<T: Transport> Accessory<T> {
    pub fn open(transport: T, kind: Kind) -> Result<Self> {
        let mut device = Self {
            transport,
            kind,
            identity: Identity {
                kind,
                serial: String::new(),
                model: String::new(),
                firmware: [0; 3],
            },
        };
        let firmware = device.query(4)?;
        device.identity.firmware.copy_from_slice(&firmware[4..7]);
        device.identity.model = String::from_utf8_lossy(&firmware[8..16])
            .trim_end_matches('\0')
            .to_string();
        if kind == Kind::Eaf {
            ensure!(
                device.identity.firmware >= [3, 3, 6],
                "EAF firmware before 3.3.6 uses a different position format"
            );
            ensure!(
                device.identity.model == "EAFN",
                "unverified EAF model {}",
                device.identity.model
            );
        }
        let serial = device.query(12)?;
        device.identity.serial = serial[4..12].iter().map(|b| format!("{b:02x}")).collect();
        ensure!(
            serial[4..12].iter().any(|b| *b != 0),
            "device has no usable serial number"
        );
        device.status()?;
        Ok(device)
    }
    fn query(&mut self, selector: u8) -> Result<Vec<u8>> {
        let mut command = [0; 16];
        command[..5].copy_from_slice(&[3, 0x7e, 0x5a, 2, selector]);
        self.transport.set_output(&command)?;
        if self.kind == Kind::Efw && selector != 1 {
            thread::sleep(Duration::from_millis(200));
        }
        let reply = self.transport.get_input()?;
        ensure!(
            reply.len() >= 16 && reply[..4] == [1, 0x7e, 0x5a, selector],
            "invalid or stale HID reply for selector {selector}"
        );
        Ok(reply)
    }
    pub fn status(&mut self) -> Result<Status> {
        let kind = self.kind;
        decode_status(kind, &self.query(if kind == Kind::Efw { 1 } else { 3 })?)
    }
    pub fn move_to(&mut self, position: i32, unidirectional: bool) -> Result<()> {
        let status = self.status()?;
        ensure!(!status.moving && !status.hand_control, "device is moving");
        ensure!(status.error == 0, "device error {}", status.error);
        match self.kind {
            Kind::Efw => {
                ensure!(
                    (0..i32::from(status.slots)).contains(&position),
                    "invalid filter position"
                );
                let mut command = [0; 16];
                command[..6].copy_from_slice(&[
                    3,
                    0x7e,
                    0x5a,
                    1,
                    if unidirectional { 3 } else { 2 },
                    position as u8 + 1,
                ]);
                self.transport.set_output(&command)?;
                thread::sleep(Duration::from_millis(200));
            }
            Kind::Eaf => {
                ensure!(
                    position >= 0 && position as u32 <= status.max_step,
                    "position exceeds EAF travel limit"
                );
                let mut command = eaf_command(&status);
                command[4] = 1;
                command[7..10].copy_from_slice(&(position as u32).to_be_bytes()[1..]);
                self.transport.set_output(&command)?;
            }
        }
        Ok(())
    }
    pub fn calibrate(&mut self) -> Result<Calibration> {
        ensure!(self.kind == Kind::Efw, "calibration applies to EFW only");
        let status = self.status()?;
        ensure!(
            !status.moving && status.error == 0,
            "EFW must be idle without errors to calibrate"
        );
        let mut command = [0; 16];
        command[..5].copy_from_slice(&[3, 0x7e, 0x5a, 1, 1]);
        // One write only: a transport error may still mean motion started.
        self.transport.set_output(&command)?;
        Ok(Calibration {
            slots: status.slots,
            deadline: Instant::now() + Duration::from_secs(90),
            saw_moving: false,
        })
    }
    pub fn halt(&mut self) -> Result<()> {
        ensure!(self.kind == Kind::Eaf, "EFW has no verified halt command");
        let status = self.status()?;
        ensure!(
            !status.hand_control,
            "release the EAF hand controller to stop motion"
        );
        self.transport.set_output(&eaf_command(&status))?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let stopped = self.status()?;
            ensure!(
                !stopped.hand_control,
                "release the EAF hand controller to stop motion"
            );
            if !stopped.moving {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "EAF did not confirm halt within one second"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn settings(
        &mut self,
        beep: Option<bool>,
        reverse: Option<bool>,
        backlash: Option<u8>,
        max_step: Option<u32>,
    ) -> Result<()> {
        ensure!(self.kind == Kind::Eaf, "settings apply to EAF only");
        let mut s = self.status()?;
        ensure!(
            !s.moving && !s.hand_control && s.error == 0,
            "EAF must be idle without errors"
        );
        if let Some(v) = beep {
            s.beep = v;
        }
        if let Some(v) = reverse {
            s.reverse = v;
        }
        if let Some(v) = backlash {
            s.backlash = v;
        }
        if let Some(v) = max_step {
            ensure!(
                v > 0 && v <= 600_000 && v >= s.position as u32,
                "invalid travel limit"
            );
            s.max_step = v;
        }
        let mut command = eaf_command(&s);
        if max_step.is_some() {
            command[10] = 2;
        }
        self.transport.set_output(&command)?;
        let actual = self.status()?;
        ensure!(
            actual.beep == s.beep
                && actual.reverse == s.reverse
                && actual.backlash == s.backlash
                && actual.max_step == s.max_step,
            "EAF settings readback mismatch"
        );
        Ok(())
    }
}
fn eaf_command(s: &Status) -> [u8; 16] {
    let mut b = [0; 16];
    b[..4].copy_from_slice(&[3, 0x7e, 0x5a, 3]);
    b[5] = s.backlash;
    b[6] = (s.max_step >> 16) as u8;
    b[7..10].copy_from_slice(&(s.position as u32).to_be_bytes()[1..]);
    b[13] = u8::from(s.beep) | (u8::from(s.reverse) << 1);
    b[14..16].copy_from_slice(&(s.max_step as u16).to_be_bytes());
    b
}
pub fn decode_status(kind: Kind, b: &[u8]) -> Result<Status> {
    ensure!(
        b.len() >= 16 && b[..4] == [1, 0x7e, 0x5a, if kind == Kind::Efw { 1 } else { 3 }],
        "invalid status report"
    );
    let mut s = Status {
        position: 0,
        moving: false,
        slots: 0,
        max_step: 0,
        backlash: 0,
        beep: false,
        reverse: false,
        hand_control: false,
        temperature_c: None,
        error: 0,
    };
    match kind {
        Kind::Efw => {
            // This implementation targets the single-disc wheel, not dual-disc models.
            ensure!(b[12] == 0, "dual-disc EFW is not supported");
            s.slots = b[9];
            ensure!((1..=16).contains(&s.slots), "invalid EFW slot count");
            s.error = if b[4] == 6 { b[5].max(1) } else { 0 };
            s.moving = b[4] != 1;
            if !s.moving {
                ensure!(
                    b[6] == b[7] && b[7] == b[8] && (1..=s.slots).contains(&b[8]),
                    "EFW sensors disagree"
                );
            }
            s.position = if s.moving { -1 } else { i32::from(b[8]) - 1 };
        }
        Kind::Eaf => {
            s.moving = b[4] == 1;
            s.error = if b[4] > 1 { b[4] } else { b[13] >> 4 };
            s.position = i32::from_be_bytes([0, b[7], b[8], b[9]]);
            s.max_step = u32::from_be_bytes([0, b[6], b[14], b[15]]);
            ensure!(
                s.position <= 600_000 && s.max_step > 0 && s.max_step <= 600_000,
                "invalid EAF position or limit"
            );
            s.backlash = b[5];
            s.beep = b[13] & 1 != 0;
            s.reverse = b[13] & 2 != 0;
            s.hand_control = b[13] & 4 != 0;
            let temperature = f64::from(u16::from_be_bytes([b[11], b[12]])) / 100.0 - 300.0;
            s.temperature_c = if temperature > -273.0 && !s.hand_control {
                Some(temperature)
            } else {
                None
            };
        }
    }
    Ok(s)
}
