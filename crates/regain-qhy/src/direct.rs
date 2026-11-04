//! QHY600 U3G20 reference implementation, derived from SDK USB traces.
//! Never loads the QHY SDK. Validated scope: full-sensor mode 0 on the traced revision.
use anyhow::{Context, Result, ensure};
use regain_transport::cypress::{Device, PendingRead, enumerate};
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const INTERFACE: u128 = 0xae18aa60_7f6a_11d4_97dd_00010229b959;
const WIDTH: usize = 9600;
const HEIGHT: usize = 6422;
const LEADING_ROWS: usize = 34;
const VIDEO_CHUNK: usize = 512 * 1024;

struct Video {
    pending: Option<PendingRead>,
    wire: Vec<u8>,
    candidate: Option<usize>,
    latest: Option<Vec<u8>>,
    sequence: u64,
    dropped: u64,
    progress: Instant,
    microseconds: u32,
    settling_until: Instant,
    discard: u32,
}

/// Match SDK's historical overlapping hexadecimal formatting. Preserve the full
/// UID separately: this lossy display ID alone is not proof of unique hardware.
fn sdk_serial(uid: &[u8]) -> String {
    let mut out = String::from("QHY600M-");
    for byte in &uid[..uid.len() - 1] {
        out.push(format!("{byte:x}").chars().next().unwrap());
    }
    out.push_str(&format!("{:x}", uid[uid.len() - 1]));
    out
}
pub struct Camera {
    device: Device,
    pub identity: serde_json::Value,
    active: bool,
    video: Option<Box<Video>>,
    last_exposure_us: u32,
}
impl Camera {
    /// Camera UART bridge traced from IsQHYCCDCFWPlugged/GetCFWStatus.
    pub fn wheel_query(&self, command: &[u8], length: usize) -> Result<Option<Vec<u8>>> {
        ensure!(
            matches!(command, b"VRS" | b"MXP" | b"NOW"),
            "Invalid wheel query"
        );
        self.device
            .control(0x40, 0xc1, 0, 0, &mut command.to_vec(), 5000)?;
        std::thread::sleep(Duration::from_millis(110));
        let mut reply = vec![0; length];
        match self.device.control(0xc0, 0xc3, 0, 0, &mut reply, 5000) {
            Ok(()) => Ok(Some(reply)),
            Err(error)
                if command == b"NOW"
                    && error.is::<regain_transport::cypress::EmptyControlReply>() =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    pub fn wheel_move(&self, position: u8) -> Result<()> {
        ensure!(position < 16, "Invalid wheel position");
        self.device.control(
            0x40,
            0xc1,
            0,
            0,
            &mut [b"0123456789ABCDEF"[position as usize]],
            5000,
        )?;
        std::thread::sleep(Duration::from_millis(25));
        Ok(())
    }
    pub fn open(path: &str) -> Result<Self> {
        let mut device = Device::open(path)?;
        device.set_trace(crate::diagnostics::record);
        let mut descriptor = [0; 18];
        device.control(0x80, 6, 0x100, 0, &mut descriptor, 5000)?;
        ensure!(
            descriptor[8..12] == [0x18, 0x16, 0x01, 0xc6],
            "Not the supported QHY600 USB identity"
        );
        let mut fw = [0; 10];
        device.control(0xc0, 0xc2, 0, 0, &mut fw, 5000)?;
        ensure!(
            fw[..3] == [0x18, 0x0a, 0x12],
            "Unvalidated QHY600 firmware: {fw:02x?}"
        );
        let mut uid = [0; 16];
        // SDK ScanQHYCCD reads this EEPROM page before choosing the model.
        // Until its fields are decoded across variants, admit only this traced
        // QHY600M hardware signature; PID C601 alone also covers other variants.
        let mut model_page = [0; 16];
        device.control(0xc0, 0xca, 0, 8, &mut model_page, 5000)?;
        ensure!(
            model_page[..8] == [0, 0, 0xaf, 7, 0xe8, 3, 0x16, 0],
            "Unvalidated QHY600 variant; use the SDK backend"
        );
        device.control(0xc0, 0xca, 0x10, 0x10, &mut uid, 5000)?;
        ensure!(
            uid.iter().any(|v| *v != 0) && uid.iter().any(|v| *v != 255),
            "Invalid QHY UID"
        );
        let identity = json!({"name":"QHY600M","serial":sdk_serial(&uid),"uid":uid.iter().map(|v|format!("{v:02x}")).collect::<String>(),"firmware":fw,"path":path,"width":WIDTH,"height":HEIGHT});
        Ok(Self {
            device,
            identity,
            active: false,
            video: None,
            last_exposure_us: 5_000_000,
        })
    }
    fn read(&self, request: u8, index: u16, len: usize) -> Result<Vec<u8>> {
        let mut data = vec![0; len];
        self.device
            .control(0xc0, request, 0, index, &mut data, 5000)?;
        Ok(data)
    }
    fn register(&self, request: u8, index: u16, value: u16) -> Result<()> {
        // B8/B9 carry the register value in wValue; the one-byte data stage
        // is ignored. The SDK sends uninitialized stack data there; send zero.
        self.device
            .control(0x40, request, value, index, &mut [0], 5000)
    }
    fn command(&self, op: u8, fields: &[u8], length: usize) -> Result<()> {
        let mut data = vec![0; length];
        data[0] = op;
        data[1..1 + fields.len()].copy_from_slice(fields);
        self.device.control(0x40, 0xd1, 0, 0, &mut data, 5000)
    }
    pub fn capture(&mut self, microseconds: u32) -> Result<Vec<u8>> {
        self.start(microseconds)?;
        let deadline = Instant::now()
            + Duration::from_micros(u64::from(microseconds))
            + Duration::from_secs(30);
        while !self.ready()? {
            ensure!(Instant::now() < deadline, "QHY readiness deadline");
            std::thread::sleep(Duration::from_millis(100));
        }
        self.download()
    }
    pub fn temperature_power(&self) -> Result<(f64, f64)> {
        let data = self.read(0xf2, 0, 4)?;
        // F2: sign byte, magnitude in tenths (big endian u16), native PWM byte.
        ensure!(data[0] <= 1, "Invalid QHY temperature sign");
        let temp = f64::from(u16::from_be_bytes([data[1], data[2]])) / 10.
            * if data[0] == 1 { -1. } else { 1. };
        ensure!(
            (-60.0..=80.0).contains(&temp),
            "QHY temperature outside plausible bounds"
        );
        Ok((temp, f64::from(data[3]) * 100. / 255.))
    }
    pub fn manual_power(&self, percent: f64) -> Result<()> {
        ensure!(
            percent.is_finite() && (0.0..=100.0).contains(&percent),
            "Invalid cooler power"
        );
        // SDK Set MANULPWM sends floor(native 0..255 / 2). F2 returns native PWM.
        let pwm = ((percent * 255. / 100.).round() as u16) / 2;
        self.device
            .control(0x40, 0xf4, 0, 0, &mut pwm.to_be_bytes(), 5000)
    }
    pub fn gain(&self, value: u16) -> Result<()> {
        ensure!(value <= 200, "QHY photographic gain outside 0..200");
        let effective = if value > 25 { value - 25 } else { value };
        let analog = (2400 + 40 * effective).min(4000);
        let digital = 8 + effective.saturating_sub(40);
        let mut fields = [0; 16];
        fields[..2].copy_from_slice(&analog.to_be_bytes());
        fields[2..4].copy_from_slice(&digital.to_be_bytes());
        fields[15] = u8::from(value > 25);
        self.command(0xa4, &fields, 32)
    }
    pub fn offset(&self, value: u16) -> Result<()> {
        ensure!(value <= 255, "Invalid QHY offset");
        self.command(0xa8, &(value * 16).to_be_bytes(), 16)
    }
    pub fn traffic(&self, value: u8) -> Result<()> {
        ensure!(value <= 60, "Invalid USB traffic");
        self.command(0xa5, &[value], 16)
    }
    pub fn initialize(&mut self) -> Result<()> {
        self.initialize_mode(false)
    }
    fn initialize_mode(&mut self, video: bool) -> Result<()> {
        self.device.transfer_size(0x81, 512 * 1024)?;
        // InitQHYCCD consumes one 16 KiB endpoint block before initialization.
        // This clears residual transport data when changing camera owners.
        self.device.read(0x81, &mut [0; 16384], 5000)?;
        // Read the same feature/status pages as SDK initialization.
        self.read(0xd5, 7, 64)?;
        self.read(0xd5, 2, 64)?;
        self.command(0xa0, &[u8::from(!video)], 16)?; // SDK stream selection
        if !video {
            self.register(0xb9, 0x62, 0)?;
            // Register 0x62 itself takes ~1.4 s. The SDK waits another 200 ms
            // after completion, not another full register round-trip duration.
            std::thread::sleep(Duration::from_millis(200));
            self.command(0xa3, &5_000_000u32.to_be_bytes(), 16)?;
            std::thread::sleep(Duration::from_millis(210));
            self.last_exposure_us = 5_000_000;
        }
        // Photographic mode 0, gain30. A4 combines analogue/digital gain and
        // read-mode fields. Remaining bytes in SDK trace are unused stack data.
        self.command(
            0xa4,
            &[0x0a, 0x28, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            32,
        )?;
        self.command(0xa8, &(30u16 * 16).to_be_bytes(), 16)?; // offset in sixteenth units
        self.command(0xa5, &[50], 16)?; // USB traffic/horizontal blanking
        self.command(0xa9, &[255], 16)?; // observed single-frame initialization field
        self.register(0xb9, 0x27, 5)?;
        self.command(0xa7, &[1], 16)?; // RAW16
        Ok(())
    }
    pub fn video_running(&self) -> bool {
        self.video.is_some()
    }
    pub fn start_video(&mut self, microseconds: u32) -> Result<()> {
        ensure!(
            !self.active && self.video.is_none(),
            "QHY acquisition already active"
        );
        ensure!(
            (1..=30_000_000).contains(&microseconds),
            "QHY video exposure outside 1 us..30 s"
        );
        let previous = self.last_exposure_us;
        self.initialize_mode(true)?;
        self.command(0xa2, &[0, 0, 0, 0, 0, 0x19, 0x38, 0, 0x19, 0], 16)?;
        self.command(0xa3, &microseconds.to_be_bytes(), 16)?;
        self.last_exposure_us = microseconds;
        // Live SDK reads this feature page at a stream boundary. Its meaning
        // is not established; retain the observed read, without inventing writes.
        self.read(0xd5, 0x24, 64)?;
        self.video = Some(Box::new(Video {
            pending: None,
            wire: Vec::new(),
            candidate: None,
            latest: None,
            sequence: 0,
            dropped: 0,
            progress: Instant::now(),
            microseconds,
            settling_until: Instant::now()
                + Duration::from_micros(u64::from(previous) + u64::from(microseconds)),
            discard: 2,
        }));
        self.active = true;
        Ok(())
    }
    /// Pump bounded asynchronous reads so thermal control and cancellation can
    /// run while the camera integrates. At most one completed image is retained.
    pub fn pump_video(&mut self) -> Result<()> {
        let Some(video) = self.video.as_mut() else {
            return Ok(());
        };
        let wire_bytes = (HEIGHT + LEADING_ROWS) * WIDTH * 2;
        ensure!(
            video.progress.elapsed()
                < Duration::from_micros(u64::from(video.microseconds)) + Duration::from_secs(30),
            "QHY video frame deadline exceeded"
        );
        for _ in 0..32 {
            if video.pending.is_none() {
                video.pending = Some(self.device.begin_read(0x81, VIDEO_CHUNK)?);
            }
            let Some(chunk) = video.pending.as_mut().unwrap().poll()? else {
                break;
            };
            video.pending = None;
            if video.wire.capacity() < wire_bytes + 4 * 1024 * 1024 + VIDEO_CHUNK {
                video
                    .wire
                    .try_reserve_exact(
                        wire_bytes + 4 * 1024 * 1024 + VIDEO_CHUNK - video.wire.len(),
                    )
                    .context("Cannot allocate QHY video frame")?;
            }
            video.wire.extend_from_slice(&chunk);
            if let Some(at) = crate::framing::locate(&video.wire, &mut video.candidate, wire_bytes)?
            {
                let end = at + wire_bytes + 8;
                // Preserve the next frame's partial header/payload; a USB read
                // need not end on a frame boundary.
                let tail = video.wire[end..].to_vec();
                let mut pixels = std::mem::replace(&mut video.wire, tail);
                video.candidate = None;
                video.sequence += 1;
                video.progress = Instant::now();
                if video.discard > 0 || Instant::now() < video.settling_until {
                    video.discard = video.discard.saturating_sub(1);
                    video.dropped += 1;
                    continue;
                }
                pixels.copy_within(at + LEADING_ROWS * WIDTH * 2..at + wire_bytes, 0);
                pixels.truncate(WIDTH * HEIGHT * 2);
                for pixel in pixels.chunks_exact_mut(2) {
                    pixel.swap(0, 1);
                }
                if video.latest.replace(pixels).is_some() {
                    video.dropped += 1;
                }
            }
            ensure!(
                video.wire.len() < wire_bytes + 4 * 1024 * 1024,
                "QHY video frame exceeded bounded prefix"
            );
        }
        Ok(())
    }
    pub fn video_status(&self) -> serde_json::Value {
        self.video.as_ref().map_or(serde_json::Value::Null, |v| {
            json!({"rawSequence":v.sequence,"discardedFrames":v.dropped,
            "lastFrameAgeMilliseconds":v.progress.elapsed().as_millis(),"sensorTimestamp":null})
        })
    }
    pub fn start(&mut self, microseconds: u32) -> Result<()> {
        ensure!(!self.active, "QHY prior acquisition requires cleanup");
        ensure!(
            (1..=3_600_000_000).contains(&microseconds),
            "Exposure outside QHY600 SDK range"
        );
        // A2: full-frame vertical readout includes 34 additional rows; the SDK
        // strips those rows. 0x1900 is the observed full-sensor span field.
        self.command(0xa2, &[0, 0, 0, 0, 0, 0x19, 0x38, 0, 0x19, 0], 16)?;
        self.command(0xa3, &microseconds.to_be_bytes(), 16)?;
        self.last_exposure_us = microseconds;
        std::thread::sleep(Duration::from_millis(35));
        self.active = true;
        self.command(0xa6, &[0], 16)?;
        self.register(0xb9, 0x31, 1)?;
        // Register 0x31 blocks for ~556 ms in the device. No extra sleep.
        self.register(0xb8, 0x19e, 0x81)?;
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.video.is_some() {
            self.video.take(); // PendingRead cancels and drains before freeing.
            self.device.abort_pipe(0x81)?;
            self.active = false;
            return Ok(());
        }
        self.device.abort_pipe(0x81)?;
        self.register(0xb9, 0x23, 0)?;
        self.active = false;
        Ok(())
    }
    pub fn ready(&self) -> Result<bool> {
        ensure!(self.active, "No QHY exposure");
        if let Some(video) = &self.video {
            return Ok(video.latest.is_some());
        }
        let status = self.read(0xbc, 4, 4)?;
        Ok(
            u64::from(u32::from_le_bytes(status.try_into().unwrap())) * 32
                >= ((HEIGHT + LEADING_ROWS) * WIDTH * 2) as u64,
        )
    }
    pub fn download(&mut self) -> Result<Vec<u8>> {
        ensure!(self.active, "No QHY exposure");
        if let Some(video) = self.video.as_mut() {
            return video.latest.take().context("QHY video frame not ready");
        }
        // The SDK polls DDR occupancy in 32-byte units before the bulk drain.
        // Wait for the full wire image; never use elapsed integration alone as
        // proof that a frame is available.
        let wire_bytes = (HEIGHT + LEADING_ROWS) * WIDTH * 2;
        let deadline = Instant::now() + Duration::from_secs(30);
        ensure!(self.ready()?, "QHY image is not ready");
        // Reserve the whole bounded preamble/frame window before draining DDR.
        // Even the largest permitted prefix must not trigger capacity doubling.
        let mut wire = Vec::new();
        wire.try_reserve_exact(wire_bytes + 4 * 1024 * 1024 + 81920)
            .context("Cannot allocate QHY USB frame buffer")?;
        let mut chunk = crate::image::zeroed(81920)?;
        let mut start = None;
        loop {
            ensure!(Instant::now() < deadline, "QHY image deadline");
            let n = self.device.read(0x81, &mut chunk, 5000)?;
            ensure!(n > 0, "Empty QHY image transfer");
            wire.extend_from_slice(&chunk[..n]);
            if let Some(at) = crate::framing::locate(&wire, &mut start, wire_bytes)? {
                self.device.abort_pipe(0x81)?;
                self.register(0xb9, 0x23, 0)?;
                self.active = false;
                wire.copy_within(at + LEADING_ROWS * WIDTH * 2..at + wire_bytes, 0);
                wire.truncate(WIDTH * HEIGHT * 2);
                for p in wire.chunks_exact_mut(2) {
                    p.swap(0, 1);
                }
                return Ok(wire);
            }
            ensure!(
                wire.len() < wire_bytes + 4 * 1024 * 1024,
                "QHY frame marker not found within bounded prefix"
            );
        }
    }
}
pub fn run(args: Vec<String>) -> Result<()> {
    if args.is_empty() || args.iter().any(|a| a == "--serve") {
        return crate::worker::run(true, args);
    }
    let mut output = None;
    let mut frames = 3;
    let mut micros = 5_000_000;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--list" => {}
            "--inspect" => {}
            "--capture" => {}
            "--output" => output = Some(PathBuf::from(iter.next().context("Missing output")?)),
            "--frames" => {
                frames = iter
                    .next()
                    .context("Missing frame count")?
                    .parse::<usize>()?
            }
            "--microseconds" => micros = iter.next().context("Missing exposure")?.parse()?,
            _ => anyhow::bail!("Unknown QHY direct reference option {arg}"),
        }
    }
    ensure!((1..=20).contains(&frames), "Invalid frame count");
    let paths = enumerate(INTERFACE, 0x1618, 0xc601)?;
    ensure!(
        paths.len() == 1,
        "Expected exactly one QHY600 for reference capture"
    );
    let mut camera = Camera::open(&paths[0])?;
    println!("{}", camera.identity);
    if args.iter().any(|arg| arg == "--inspect") {
        let (temperature, power) = camera.temperature_power()?;
        println!("{}", json!({"temperature":temperature,"coolerPower":power}));
    }
    if let Some(output) = output {
        std::fs::create_dir(&output)?;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(
                60 + frames as u64 * (u64::from(micros) / 1_000_000 + 40),
            ));
            std::process::exit(124)
        });
        camera.initialize()?;
        for index in 0..frames {
            let start = Instant::now();
            let pixels = camera.capture(micros)?;
            std::fs::write(output.join(format!("frame-{index}.raw")), &pixels)?;
            println!(
                "{}",
                json!({"frame":index,"bytes":pixels.len(),"elapsedMs":start.elapsed().as_millis()})
            );
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_identity_compatibility() {
        assert_eq!(
            sdk_serial(&[
                0x98, 0x6a, 0x7d, 0x63, 3, 0xb8, 0x43, 0xaa, 0xbb, 0x6d, 0xa4, 0x78, 0x9e, 0xe5,
                0x11, 0x17
            ]),
            "QHY600M-96763b4ab6a79e117"
        );
    }
}
