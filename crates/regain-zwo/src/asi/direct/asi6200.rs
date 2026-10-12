//! ASI6200MM Pro original and P25 acquisition over the platform USB transport.
//! Register roles below are observed SDK 1.41 behavior. See
//! docs/camera-usb-registers.md for transport notation, evidence and unknowns;
//! the initialization table is ordered volatile setup, not a public register map.
use crate::asi::direct::{asi6200_tables, processing, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};

// SDK exposure range. Integrations >= 1 s use host timing, so their duration
// does not increase the sensor frame/shutter register values.
pub const MAX_EXPOSURE_US: u32 = 2_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Revision {
    Original,
    P25,
}
impl Revision {
    pub fn from_register(value: u8) -> Result<Self> {
        // SDK 1.41 selects the faster sensor timing with BC:1c == 5.
        // Only these two register values have been tested on hardware.
        match value {
            3 => Ok(Self::Original),
            5 => Ok(Self::P25),
            _ => anyhow::bail!("unverified ASI6200 hardware revision {value:#x}; use SDK mode"),
        }
    }
    pub fn detect(camera: &Camera) -> Result<Self> {
        Self::from_register(camera.vendor(0xbc, 0x1c, 0, 1)?[0])
    }
    fn hmax(self) -> u32 {
        match self {
            Self::Original => 1515,
            Self::P25 => 880,
        }
    }
}

/// SDK open reads ASID before initializing the sensor, then reuses both for
/// subsequent captures. Keep only the immutable factory bytes and the most
/// recently decoded physical ROI; never cache frame pixels or capture controls.
#[derive(Default)]
pub(super) struct ConnectionState {
    generation: u64,
    revision: Option<Revision>,
    calibration: Option<Vec<u8>>,
    defects: Option<([u32; 4], Arc<processing::Defects>)>,
    initialized: bool,
    register_snapshot: Option<RegisterSnapshot>,
}
impl ConnectionState {
    fn clear(&mut self) {
        self.calibration = None;
        self.defects = None;
        self.initialized = false;
        self.register_snapshot = None;
    }

    fn prepare(
        &mut self,
        generation: u64,
        revision: Revision,
        roi: [u32; 4],
        load: impl FnOnce() -> Result<Vec<u8>>,
        initialize: impl FnOnce() -> Result<()>,
    ) -> Result<Arc<processing::Defects>> {
        if self.generation != generation || self.revision != Some(revision) {
            self.clear();
            self.generation = generation;
            self.revision = Some(revision);
        }
        let result = (|| {
            if self.calibration.is_none() {
                self.calibration = Some(load()?);
            }
            if self.defects.as_ref().is_none_or(|(prior, _)| *prior != roi) {
                let [width, height, x, y] = roi;
                let defects = processing::Defects::decode_6200(
                    self.calibration.as_ref().unwrap(),
                    width as usize,
                    height as usize,
                    x as usize,
                    y as usize,
                )?;
                self.defects = Some((roi, Arc::new(defects)));
            }
            // Validate calibration before any initialization writes. Only a
            // complete setup and thermal restoration establish reusable state.
            if !self.initialized {
                initialize()?;
                self.initialized = true;
            }
            Ok(Arc::clone(&self.defects.as_ref().unwrap().1))
        })();
        if result.is_err() {
            self.clear();
        }
        result
    }
}

fn writes(camera: &Camera, commands: &[(u8, u16, u16)]) -> Result<()> {
    for &(request, register, value) in commands {
        camera.vendor(request, register, value, 0)?;
    }
    Ok(())
}
fn word(camera: &Camera, request: u8, register: u16, value: u32, bytes: u16) -> Result<()> {
    // Multi-byte fields occupy ascending byte addresses, least significant
    // byte first. SDK FPGA writes bracket the complete field with BD01=1/0;
    // sensor B6 writes do not. The bracket's hardware implementation is unknown.
    if request == 0xbd {
        camera.vendor(request, 1, 1, 0)?;
    }
    for byte in 0..bytes {
        camera.vendor(
            request,
            register + byte,
            ((value >> (8 * byte)) & 255) as u16,
            0,
        )?;
    }
    if request == 0xbd {
        camera.vendor(request, 1, 0, 0)?;
    }
    Ok(())
}
fn stop(camera: &Camera) -> Result<()> {
    super::completion::finish_stop(
        "ASI6200",
        super::host_timed::restore_if_gated(camera, super::host_timed::ASI6200),
        || {
            // Destructive stop: assert FPGA mask 0x10, put the sensor in
            // standby, discard DDR with AA, then drain/reset the host pipe.
            // Gate repair above is required even when BC23 already reads 1.
            let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
            camera.vendor(0xbd, 0, u16::from(flags | 0x10), 0)?;
            freeze(camera)?;
            camera.vendor(0xaa, 0, 0, 0)?;
            camera.reset_pipe()
        },
    )
}
fn freeze(camera: &Camera) -> Result<()> {
    // Sensor-only standby preserves retained DDR. Do not add BD00 mask 0x10
    // (bit 4) or AA here: both are destructive in the P25 retention tests.
    camera.vendor(0xb6, 0x19e, 5, 0)?;
    // SDK 1.41 helper 1e8320 changes only sensor standby bit 0. Preserve
    // additional sensor flags instead of assuming every firmware reads 4/5.
    let sensor = camera.vendor(0xb7, 0, 0, 1)?[0];
    camera.vendor(0xb6, 0, u16::from(sensor | 1), 0)?;
    Ok(())
}
fn arm(
    mut vendor: impl FnMut(u8, u16, u16, u16) -> Result<Vec<u8>>,
    sleep: impl FnOnce(Duration),
) -> Result<()> {
    // SDK 1.41 helper 18d800 asserts FPGA stop again after A9, then enables
    // sensor bit 2 and clears standby bit 0 with separate B7 read/modify/writes.
    // Preserve other flags on both devices, including host-timed mode bits.
    vendor(0xa9, 0, 0, 0)?;
    let flags = vendor(0xbc, 0, 0, 1)?[0];
    vendor(0xbd, 0, u16::from(flags | 0x10), 0)?;
    vendor(0xb6, 0x19e, 1, 0)?;
    let sensor = vendor(0xb7, 0, 0, 1)?[0];
    vendor(0xb6, 0, u16::from(sensor | 4), 0)?;
    sleep(Duration::from_millis(10));
    let sensor = vendor(0xb7, 0, 0, 1)?[0];
    vendor(0xb6, 0, u16::from(sensor & !1), 0)?;
    let flags = vendor(0xbc, 0, 0, 1)?[0];
    vendor(0xbd, 0, u16::from(flags & !0x10), 0)?;
    Ok(())
}

#[derive(Clone, Default)]
struct RegisterSnapshot {
    registers: serde_json::Map<String, Value>,
    observed_at_unix_us: u128,
    error: Option<String>,
}

fn snapshot_registers(
    cached: &mut Option<RegisterSnapshot>,
    failure: bool,
    mut read: impl FnMut(u16) -> Result<u8>,
) -> RegisterSnapshot {
    if failure {
        // The acquisition's original transfer diagnostics retain the failing
        // command and last acknowledgements. Optional failure probes must not
        // delay restoration or pound a camera that has stopped responding.
        return cached.clone().unwrap_or_default();
    }
    let mut snapshot = RegisterSnapshot {
        observed_at_unix_us: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros(),
        ..RegisterSnapshot::default()
    };
    // These BC mirrors are not a validated public register map. Never use
    // their diagnostic values as readiness or acceptance gates.
    for register in [
        0x40, 0x41, 0x42, 0x43, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x00, 0x0b, 0x19, 0x23, 0x1c,
    ] {
        match read(register) {
            Ok(value) => {
                snapshot
                    .registers
                    .insert(format!("{register:02x}"), json!(value));
            }
            Err(error) => {
                snapshot.error = Some(error.to_string());
                break;
            }
        }
    }
    *cached = Some(snapshot.clone());
    snapshot
}

fn register_snapshot(camera: &Camera, s: &Settings, stage: &str) {
    let started = Instant::now();
    // Capture the programmed counts/gates on healthy frames too, so a remote
    // second-frame failure can be compared with the preceding successful one.
    // This is read-only diagnostic evidence, not a readiness/acceptance gate.
    // Explicit opt-out only affects healthy snapshots; failures remain visible.
    let failure = matches!(stage, "capture_failure" | "read_failure");
    let snapshots = std::env::var("REGAIN_CAMERA_REGISTER_SNAPSHOTS");
    if !failure
        && (snapshots.as_deref() == Ok("0")
            || (!crate::asi::diagnostics::command_logging_enabled()
                && snapshots.as_deref() != Ok("1")))
    {
        return;
    }
    let mut cached = camera.asi6200.borrow_mut();
    // Any preceding failed operation invalidates this setup generation. Even
    // if the caller can finish/replay the image, avoid optional live probes
    // until the next validated setup reestablishes the connection state.
    let cached_only = failure || cached.generation != camera.transport_generation();
    let snapshot = snapshot_registers(&mut cached.register_snapshot, cached_only, |register| {
        Ok(camera.vendor(0xbc, register, 0, 1)?[0])
    });
    let RegisterSnapshot {
        registers,
        observed_at_unix_us,
        error,
    } = snapshot;
    let source = if cached_only {
        "cached_may_be_stale"
    } else {
        "live"
    };
    super::diagnostics::details(
        "info",
        "asi6200.register_snapshot",
        format_args!(
            "ASI6200 {stage} FPGA register snapshot ({source}): {registers:?}; read error {error:?}"
        ),
        json!({"stage":stage,"model":"ASI6200MM Pro","width":s.width,"height":s.height,
            "x":s.x,"y":s.y,"microseconds":s.microseconds,"offset":s.offset,
            "expectedTransferWords":s.width * s.height / 2,"expectedBytes":s.width * s.height * 2,
            "registerReadElapsedUs":started.elapsed().as_micros(),
            "registerSource":source,"liveDiagnosticReads":!cached_only,
            "registersObservedAtUnixUs":observed_at_unix_us,
            "timestampUnixUs":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_micros(),
            "transportGeneration":camera.transport_generation(),"setupGeneration":cached.generation,
            "initialized":cached.initialized,"registers":registers,"error":error}),
    );
}

fn remaining_readout_guard(guard: Duration, since_ungated: Option<Duration>) -> Duration {
    // Long mode has a verified ungated sensor after host_timed::expose. Its
    // ready polling counts toward the full sensor interval. Short mode has no
    // such anchor, so retain the complete guard after the ready indication.
    guard.saturating_sub(since_ungated.unwrap_or_default())
}
fn freeze_readout(
    camera: &Camera,
    s: &Settings,
    armed: Instant,
    ungated: Option<Instant>,
    readout: Duration,
) -> Result<()> {
    camera.phase("sensor_readout");
    let deadline = Duration::from_micros(u64::from(s.microseconds)) + Duration::from_secs(10);
    loop {
        let status = camera.vendor(0xbc, 0x23, 0, 1)?[0];
        if status & !0x10 == 5 {
            break;
        }
        ensure!(
            armed.elapsed() < deadline,
            "ASI6200 retained-frame timeout: {status}"
        );
        camera.service_environment()?;
        std::thread::sleep(Duration::from_millis(5));
    }
    // Only recovery/research needs this conservative DDR-completion guard.
    // The SDK-style normal path proves completion by consuming the full frame.
    let remaining = remaining_readout_guard(readout, ungated.map(|anchor| anchor.elapsed()));
    let settling = Instant::now();
    while settling.elapsed() < remaining {
        camera.service_environment()?;
        std::thread::sleep(Duration::from_millis(10));
    }
    freeze(camera)?;
    camera.phase("retained");
    register_snapshot(camera, s, "frozen_readout");
    Ok(())
}
fn frame_sequence(data: &[u8]) -> Result<u16> {
    ensure!(
        data.len() >= 16 && data[..2] == [0x7e, 0x5a] && data[data.len() - 2..] == [0xf0, 0x3c],
        "invalid ASI6200 frame envelope"
    );
    let first = u16::from_le_bytes(data[2..4].try_into().unwrap());
    let last = u16::from_le_bytes(data[data.len() - 4..data.len() - 2].try_into().unwrap());
    ensure!(first == last, "ASI6200 frame envelope counters differ");
    Ok(first)
}
pub fn validate(s: &Settings, gain: i32) -> Result<()> {
    ensure!(
        s.width >= 64
            && s.width <= 9576
            && s.width.is_multiple_of(8)
            && s.height >= 64
            && s.height <= 6388
            && s.height.is_multiple_of(2)
            && s.x.is_multiple_of(16)
            && s.y.is_multiple_of(2)
            && s.x <= 9576 - s.width
            && s.y <= 6388 - s.height,
        "invalid ASI6200 bin-1 ROI (x multiple of 16, y even)"
    );
    ensure!(
        (32..=MAX_EXPOSURE_US).contains(&s.microseconds)
            && (0..=700).contains(&gain)
            && s.offset <= 200
            && s.read_retries <= 5,
        "unsupported ASI6200 exposure/controls"
    );
    let bytes = s.width * s.height * 2;
    ensure!(
        s.interrupt_read_after_bytes == 0 || s.timeout_read_after_bytes == 0,
        "choose one read fault injection"
    );
    for prefix in [
        s.interrupt_read_after_bytes,
        s.replay_prefix_bytes,
        s.timeout_read_after_bytes,
    ] {
        ensure!(
            prefix == 0 || (prefix >= 1024 && prefix < bytes && prefix.is_multiple_of(1024)),
            "ASI6200 interruption prefix must be packet aligned and shorter than the frame"
        );
    }
    Ok(())
}
fn calibration(camera: &Camera) -> Result<Vec<u8>> {
    // SDK brackets ASID reads with BE=0/1 (exact internal mode unknown).
    // C3's index is a 256-byte address unit: 0x400 starts at 0x40000.
    // Data is read from this camera and decoded, never copied from a fixture.
    camera.vendor(0xbe, 0, 0, 0)?;
    let result: Result<Vec<u8>> = (|| {
        let mut data = camera.vendor(0xc3, 0, 0x400, 2048)?;
        let length = processing::declared_length(&data)?;
        while data.len() < length {
            let offset = data.len();
            let amount = (length - offset).min(2048).next_multiple_of(256);
            data.extend(camera.vendor(0xc3, 0, ((0x40000 + offset) >> 8) as u16, amount as u16)?);
        }
        data.truncate(length);
        Ok(data)
    })();
    let restore = camera.vendor(0xbe, 1, 0, 0);
    let data = result?;
    restore?;
    Ok(data)
}

pub fn gain_registers(gain: i32) -> (u32, u16, u16, u16) {
    let digital = if gain > 460 {
        (gain - 460 + 59) / 60
    } else {
        0
    };
    let adjusted = if gain < 100 {
        gain
    } else {
        gain - 100 - digital * 60
    };
    // Preserve the SDK's division and subtraction order at integer boundaries.
    let analog = (4095.0 - 4095.0 * 10.0_f64.powf(-(adjusted as f64 / 10.0 / 20.0))) as u32;
    let (mode, readout) = match gain {
        ..61 => (0, 8),
        61..100 => (4, 10),
        100..160 => (1, 8),
        160..280 => (5, 10),
        _ => (5, 12),
    };
    (analog, mode, readout, (digital * 16) as u16)
}
pub fn timing(s: &Settings, revision: Revision) -> (u32, u32) {
    // SDK arithmetic: HMAX / 20 MHz gives microseconds per line; 52 extra
    // rows, +10000 us for host-timed setup, +20/3 line adjustments and shutter
    // /2 encoding are model-specific observed constants. They are not added
    // to the user's exposure. >=1 s uses a fixed sensor interval while
    // host_timed controls integration duration. Preserve f32/truncation order.
    let line = revision.hmax() as f32 * 1000.0 / 20000.0;
    let minimum = ((s.height + 52) as f32 * line) as u32;
    let exposure = if s.microseconds >= 1_000_000 {
        minimum + 10000
    } else {
        s.microseconds
    };
    let (frame, shutter) = if exposure > minimum {
        ((exposure as f32 / line) as u32 + 20, 20)
    } else {
        let frame = s.height + 52;
        (
            frame,
            frame
                .saturating_sub((exposure as f32 / line) as u32 + 3)
                .clamp(3, frame - 3),
        )
    };
    (frame.min(0xffffff), (shutter.min(0x1fffe) / 2).max(3))
}
fn begin_retained_read(camera: &Camera) -> Result<()> {
    camera.phase("restarting_retained");
    // BD18=0/1 stops/restarts the DDR sender without A9 (new exposure) or
    // AA (discard frame). Retained status 0x05/0x15 differs only by sync bit 4.
    // The 100 ms sender settling wait is independent of exposure duration.
    camera.vendor(0xbd, 0x18, 0, 0)?;
    camera.service_environment()?;
    std::thread::sleep(Duration::from_millis(100));
    // Stop the sender before clearing queued USB data. Resetting while the
    // P25 sender is still active can leave a stale partial pass in the pipe.
    camera.reset_pipe()?;
    ensure!(
        camera.vendor(0xbc, 0x18, 0, 1)?[0] == 0,
        "ASI6200 replay engine did not return idle"
    );
    let retained = camera.vendor(0xbc, 0x23, 0, 1)?[0];
    ensure!(
        retained & !0x10 == 5,
        "ASI6200 retained frame was lost: {retained:#x}"
    );
    camera.vendor(0xbd, 0x18, 1, 0)?;
    camera.phase("retained");
    Ok(())
}

pub fn raw_settings(s: &Settings, gain: i32, bin: u32) -> Result<Settings> {
    ensure!((1..=4).contains(&bin), "ASI6200 bin must be 1..4");
    ensure!(
        s.width.is_multiple_of(8) && s.height.is_multiple_of(2),
        "ASI6200 output dimensions require width multiple of 8 and even height"
    );
    let mut raw = s.clone();
    raw.width = s
        .width
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("ASI6200 width overflow"))?;
    raw.height = s
        .height
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("ASI6200 height overflow"))?;
    raw.x =
        s.x.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("ASI6200 x overflow"))?;
    raw.y =
        s.y.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("ASI6200 y overflow"))?;
    validate(&raw, gain)?;
    Ok(raw)
}
pub fn capture(
    camera: &Camera,
    info: &Value,
    s: &Settings,
    gain: i32,
    bin: u32,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    let raw = raw_settings(s, gain, bin)?;
    let sensor = sensor_settings(&raw);
    let (meta, data) = capture_native(camera, info, &sensor, gain, replay)?;
    finish_output(s, &raw, &sensor, bin, meta, data)
}
fn finish_output(
    s: &Settings,
    raw: &Settings,
    sensor: &Settings,
    bin: u32,
    mut meta: Value,
    mut data: Vec<u8>,
) -> Result<(Value, Vec<u8>)> {
    let crop = sensor.width != raw.width || sensor.height != raw.height;
    if crop {
        let mut cropped = Vec::with_capacity(raw.width as usize * raw.height as usize * 2);
        for row in 0..raw.height {
            let start = ((row + raw.y - sensor.y) * sensor.width + raw.x - sensor.x) as usize * 2;
            cropped.extend_from_slice(&data[start..start + raw.width as usize * 2]);
        }
        data = cropped;
    }
    if bin > 1 {
        data =
            processing::bin_average(&data, raw.width as usize, raw.height as usize, bin as usize)?;
    }
    meta["wireBytes"] = json!(sensor.width * sensor.height * 2);
    meta["rawWidth"] = json!(sensor.width);
    meta["rawHeight"] = json!(sensor.height);
    meta["rawX"] = json!(sensor.x);
    meta["rawY"] = json!(sensor.y);
    meta["width"] = json!(s.width);
    meta["height"] = json!(s.height);
    meta["bin"] = json!(bin);
    meta["x"] = json!(s.x);
    meta["y"] = json!(s.y);
    meta["bytes"] = json!(data.len());
    if crop || bin > 1 {
        meta["sha256"] = json!(format!("{:x}", Sha256::digest(&data)));
    }
    Ok((meta, data))
}
fn sensor_settings(raw: &Settings) -> Settings {
    let mut sensor = raw.clone();
    // This P25 unit stalls DDR replay for 8/16/32/64 KiB frames. Both narrow
    // and wide 128 KiB frames replay correctly. Read at least that amount,
    // correct defects in sensor coordinates, then crop before software binning.
    if sensor.width * sensor.height < 65536 {
        sensor.width = sensor.width.max(512);
        sensor.height = sensor.height.max(128);
        sensor.x = sensor.x.min((9576 - sensor.width) / 16 * 16);
        sensor.y = sensor.y.min(6388 - sensor.height);
    }
    sensor
}
/// Sensor initialization can reset thermal hardware even when its explicit
/// BD19/26 defaults are skipped. Restore after setup, including failure paths,
/// before cancellation can return or another exposure can be armed.
fn initialize<T>(
    revision: Revision,
    has_environment: bool,
    mut write: impl FnMut(u8, u16, u16) -> Result<()>,
    configure: impl FnOnce() -> Result<T>,
    restore_environment: impl FnOnce() -> Result<()>,
) -> Result<T> {
    super::initialization::preserve_environment(
        "ASI6200",
        || {
            for &(request, register, value) in asi6200_tables::INITIALIZE {
                if has_environment && request == 0xbd && [0x19, 0x26].contains(&register) {
                    // Omit traced thermal defaults: BD19 contains cooler/dew
                    // flags; BD26 is cooler output. Still restore afterward:
                    // other initialization commands can reset these indirectly.
                    continue;
                }
                let value = match (request, register) {
                    (0xbd, 0x13) => (revision.hmax() & 255) as u16,
                    (0xbd, 0x14) => (revision.hmax() >> 8) as u16,
                    _ => value,
                };
                write(request, register, value)?;
            }
            configure()
        },
        restore_environment,
    )
}
fn prepare(camera: &Camera, s: &Settings, revision: Revision) -> Result<Arc<processing::Defects>> {
    camera.asi6200.borrow_mut().prepare(
        camera.transport_generation(),
        revision,
        [s.width, s.height, s.x, s.y],
        || {
            // SDK flash reads precede AF/sensor setup. They need no actuator
            // writes on success; an uncertain read still restores cached state.
            match calibration(camera) {
                Ok(data) => Ok(data),
                Err(error) => super::initialization::preserve_environment(
                    "ASI6200",
                    || Err(error),
                    || camera.restore_environment(),
                ),
            }
        },
        || {
            // SDK restores thermal state during init, BEFORE RAW16. The 0.6.0.4
            // change added unconditional post-RAW16 actuator rewrites on
            // every frame. Reuse setup and only repair an observed reset here.
            initialize(
                revision,
                camera.has_environment(),
                |request, register, value| {
                    camera.vendor(request, register, value, 0)?;
                    Ok(())
                },
                || Ok(()),
                || camera.restore_environment(),
            )?;
            finish_raw16(
                || writes(camera, asi6200_tables::RAW16),
                || camera.verify_environment(),
                || camera.restore_environment(),
            )
        },
    )
}
fn finish_raw16(
    configure: impl FnOnce() -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
    restore: impl FnOnce() -> Result<()>,
) -> Result<()> {
    match configure() {
        Ok(()) if verify().is_ok() => Ok(()),
        Ok(()) => restore(),
        Err(error) => {
            super::initialization::preserve_environment("ASI6200", || Err(error), restore)
        }
    }
}
/// Like SDK open/init, establish factory correction and sensor setup before
/// acknowledging the selected connection. Discovery must never call this.
pub(super) fn initialize_connection(camera: &Camera, info: &Value) -> Result<()> {
    super::link::validate(info, 0x620b)?;
    let revision = Revision::detect(camera)?;
    let settings = Settings {
        width: 9576,
        height: 6388,
        ..Settings::default()
    };
    prepare(camera, &settings, revision)?;
    Ok(())
}
fn retained_read_allowed(error: &anyhow::Error, exposure_complete: bool) -> bool {
    exposure_complete
        && !error.is::<super::completion::CaptureCancelled>()
        && !super::transfer::Failure::details(error).is_null()
}
fn capture_native(
    camera: &Camera,
    info: &Value,
    s: &Settings,
    gain: i32,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    validate(s, gain)?;
    super::link::validate(info, 0x620b)?;
    let started = Instant::now();
    let revision = Revision::detect(camera)?;
    camera.phase("initializing");
    let result = (|| {
        let defects = prepare(camera, s, revision)?;
        // Fixed SDK FPGA words BD06=49 (0x31), BD02=24 (0x18). Their individual
        // meanings are not decoded; keep them distinct from the user's ROI.
        word(camera, 0xbd, 6, 49, 2)?;
        word(camera, 0xbd, 2, 24, 2)?;
        // SDK window selectors (a5/05=1; internal field definitions unknown),
        // then sensor origin: x in 16-pixel units, y with a 25-row base offset.
        writes(camera, &[(0xb6, 0xa5, 1), (0xb6, 5, 1)])?;
        word(camera, 0xb6, 0xa6, s.x / 16, 2)?;
        word(camera, 0xb6, 6, s.y + 25, 2)?;
        // FPGA transfer count is 32-bit words: RAW16 bytes/4 = width*height/2.
        // Sensor 0187=4 is fixed SDK window setup (bits not independently named).
        // Program physical sensor dimensions, including its +24 width encoding,
        // and matching FPGA output dimensions before starting the acquisition.
        word(camera, 0xbd, 0x40, s.width * s.height / 2, 4)?;
        camera.vendor(0xb6, 0x187, 4, 0)?;
        word(camera, 0xb6, 8, s.height, 2)?;
        word(camera, 0xb6, 0x18c, s.width + 24, 2)?;
        word(camera, 0xbd, 8, s.height, 2)?;
        word(camera, 0xbd, 4, s.width, 2)?;
        // Both revisions use divisor 400 at USB bandwidth 40.
        word(camera, 0xbd, 0x13, revision.hmax(), 2)?;
        word(camera, 0xbd, 0x24, 400, 2)?;
        let (analog, mode, readout, digital) = gain_registers(gain);
        // SDK conversion/readout modes at gains 61/100/160/280, plus its
        // associated 03a2..03a6 tuning values. Individual tuning field meanings
        // are unknown; keep the observed tuple rather than simplifying bits.
        writes(
            camera,
            &[
                (0xb6, 0x2d, mode),
                (0xb6, 0x4d, readout),
                (0xb6, 0x3a2, 7),
                (0xb6, 0x3a3, 17),
                (0xb6, 0x3a4, if gain >= 280 { 35 } else { 17 }),
                (0xb6, 0x3a5, if gain >= 280 { 45 } else { 17 }),
                (0xb6, 0x3a6, if gain >= 280 { 45 } else { 17 }),
            ],
        )?;
        // Paired analog gain fields 2e/30, digital stages 3e above gain 460,
        // and paired offset fields 40/42 scaled by ten, per SDK control traces.
        word(camera, 0xb6, 0x2e, analog, 2)?;
        word(camera, 0xb6, 0x30, analog, 2)?;
        camera.vendor(0xb6, 0x3e, digital, 0)?;
        word(camera, 0xb6, 0x40, s.offset * 10, 2)?;
        word(camera, 0xb6, 0x42, s.offset * 10, 2)?;
        let (frame, shutter) = timing(s, revision);
        word(camera, 0xbd, 0x10, frame, 3)?;
        word(camera, 0xb6, 0x16, shutter, 2)?;
        // BD00 masks 0xc0 select the traced host-timed (>=1 s) path together.
        // Their individual firmware meanings are not established; preserve
        // all other bits, including the separate synchronization/stop bit 4.
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        camera.vendor(
            0xbd,
            0,
            u16::from(if s.microseconds >= 1_000_000 {
                flags | 0xc0
            } else {
                flags & !0xc0
            }),
            0,
        )?;
        register_snapshot(camera, s, "programmed");
        stop(camera)?;
        let old = camera.vendor(0xbc, 0x23, 0, 1)?[0];
        ensure!(old == 1, "ASI6200 old frame did not clear: {old}");
        register_snapshot(camera, s, "pre_arm");
        // Do not start a new acquisition if cancellation arrived during setup.
        camera.service_environment()?;
        let armed = Instant::now();
        camera.phase("exposing");
        arm(
            |request, register, value, length| camera.vendor(request, register, value, length),
            std::thread::sleep,
        )?;
        camera.reset_pipe()?;
        let readout = Duration::from_micros(
            (u64::from(frame) * u64::from(revision.hmax())).div_ceil(20) + 100000,
        );
        let frame_bytes = s.width as usize * s.height as usize * 2;
        let mut continuity = super::transfer::Continuity::default();
        let mut ungated = None;
        let mut exposure_complete = false;
        let mut expose = || -> Result<()> {
            if s.microseconds >= 1_000_000 {
                super::host_timed::expose(camera, super::host_timed::ASI6200, s.microseconds)?;
                ungated = Some(Instant::now());
            }
            exposure_complete = true;
            Ok(())
        };
        // Explicit retained-sender research keeps its established injection
        // point. Production first-pass reads follow SDK timing instead: queue
        // before synchronization, overlap readout, freeze only after completion.
        let research_read = s.timeout_read_after_bytes > 0 || s.interrupt_read_after_bytes > 0;
        let mut first_pass = if research_read {
            expose()?;
            freeze_readout(camera, s, armed, ungated, readout)?;
            if s.microseconds < 1_000_000 {
                begin_retained_read(camera)?;
            }
            None
        } else {
            let first = camera.read_frame_during(frame_bytes, &mut continuity, expose);
            if let Err(error) = &first {
                // Queue submission can fail before expose() runs, including
                // with structured transfer evidence. Never replay a frame
                // whose exposure handshake did not complete. All queued I/O
                // has drained before this exit.
                if !retained_read_allowed(error, exposure_complete) {
                    return Err(first.unwrap_err());
                }
                freeze_readout(camera, s, armed, ungated, readout)?;
            } else {
                freeze(camera)?;
                camera.phase("retained");
                register_snapshot(camera, s, "completed_readout");
            }
            Some(first)
        };
        let mut prefix = Vec::new();
        let mut read_errors = Vec::new();
        let mut data = loop {
            let attempt = (|| {
                if let Some(first) = first_pass.take() {
                    let data = first?;
                    frame_sequence(&data)?;
                    return Ok(data);
                }
                if read_errors.is_empty() && s.timeout_read_after_bytes > 0 {
                    prefix = camera.read_frame(s.timeout_read_after_bytes as usize)?;
                    // Research CLI only: stop the retained-frame sender and drain
                    // the pipe. The following real bulk read has no producer and
                    // must expire through the normal request/cancel/drain path.
                    camera.vendor(0xbd, 0x18, 0, 0)?;
                    camera.reset_pipe()?;
                    camera.read_frame(1024 * 1024)?;
                    anyhow::bail!("stalled sender unexpectedly delivered a bulk read");
                }
                if read_errors.is_empty() && s.interrupt_read_after_bytes > 0 {
                    prefix = camera.read_frame(s.interrupt_read_after_bytes as usize)?;
                    anyhow::bail!("injected host interruption after {} bytes", prefix.len());
                }
                let data = camera.read_frame_checked(frame_bytes, 5000, &mut continuity)?;
                frame_sequence(&data)?;
                Ok(data)
            })();
            match attempt {
                Ok(data) => break data,
                Err(error) => {
                    if error.is::<super::completion::CaptureCancelled>() {
                        return Err(error);
                    }
                    register_snapshot(camera, s, "read_failure");
                    crate::asi::direct::diagnostics::read_failure(
                        "ASI6200MM Pro",
                        &error,
                        read_errors.len(),
                        s.read_retries,
                        true,
                    );
                    if read_errors.len() >= s.read_retries as usize {
                        return Err(error);
                    }
                    read_errors.push(error.to_string());
                    begin_retained_read(camera)?;
                }
            }
        };
        ensure!(
            prefix.is_empty() || data[4..prefix.len()] == prefix[4..],
            "ASI6200 recovered pixels do not match interrupted prefix"
        );
        let sequence = frame_sequence(&data)?;
        camera.phase("validating");
        let wire_hash = format!("{:x}", Sha256::digest(&data));
        let replay_result = if replay {
            begin_retained_read(camera)?;
            if s.replay_prefix_bytes > 0 {
                let prefix = camera.read_frame(s.replay_prefix_bytes as usize)?;
                ensure!(
                    prefix[4..] == data[4..prefix.len()],
                    "ASI6200 replay prefix changed"
                );
                begin_retained_read(camera)?;
            }
            let mut errors = Vec::new();
            let second = loop {
                match camera.read_frame(data.len()).and_then(|frame| {
                    frame_sequence(&frame)?;
                    Ok(frame)
                }) {
                    Ok(frame) => break frame,
                    Err(e) if errors.len() < s.read_retries as usize => {
                        errors.push(e.to_string());
                        begin_retained_read(camera)?;
                    }
                    Err(e) => return Err(e),
                }
            };
            let replay_sequence = frame_sequence(&second)?;
            ensure!(
                second[4..second.len() - 4] == data[4..data.len() - 4],
                "ASI6200 retained pixels differ from original"
            );
            json!({"pixelBytesIdentical":true,"wireBytesIdentical":second==data,"bytes":data.len(),
                "firstSequence":sequence,"replaySequence":replay_sequence,"readErrors":errors})
        } else {
            Value::Null
        };
        let last = data.len() - 4;
        let row = s.width as usize * 2;
        data.copy_within(row..row + 4, 0);
        data.copy_within(last - row..last - row + 4, last);
        defects.correct(&mut data)?;
        let meta = json!({"model":"ASI6200MM Pro","width":s.width,"height":s.height,"x":s.x,"y":s.y,
            "bin":1,"gain":gain,"offset":s.offset,"microseconds":s.microseconds,"bytes":data.len(),
            "wireSha256":wire_hash,"sha256":format!("{:x}",Sha256::digest(&data)),"sequence":sequence,
            "factoryDefects":defects.indices.len(),"acquisitionMs":armed.elapsed().as_millis(),"elapsedMs":started.elapsed().as_millis(),
            "readoutGuardUs":readout.as_micros(),"sensorHmax":revision.hmax(),
            "downloadPipeline":if research_read {"retained-research"} else {"sdk-queued"},
            "retainedPixelsVerified":continuity.verified_bytes() > 0,
            "hardwareRevision":if revision == Revision::P25 {5} else {3},
            "replay":replay_result,"sdkLoaded":false,"readRecoveries":read_errors.len(),"readErrors":read_errors,
            "interruptedPrefixBytes":prefix.len(),"interruptedPrefixPixelsMatch":!prefix.is_empty(),
            "timeoutInjectionBytes":s.timeout_read_after_bytes});
        Ok((meta, data))
    })();
    if result
        .as_ref()
        .is_err_and(|error| !error.is::<super::completion::CaptureCancelled>())
    {
        register_snapshot(camera, s, "capture_failure");
        camera.asi6200.borrow_mut().clear();
    }
    let cleanup = stop(camera);
    if cleanup.is_err() {
        camera.asi6200.borrow_mut().clear();
    }
    crate::asi::direct::completion::finish(result, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn empty_calibration() -> Vec<u8> {
        [b"ASID".as_slice(), &8_u32.to_be_bytes()].concat()
    }
    #[test]
    fn failure_snapshots_never_probe_the_unavailable_camera() {
        let mut cached = None;
        let empty = snapshot_registers(&mut cached, true, |_| panic!("failure must not probe USB"));
        assert!(empty.registers.is_empty());
        assert_eq!(empty.observed_at_unix_us, 0);
        assert!(cached.is_none());

        let reads = Cell::new(0);
        let healthy = snapshot_registers(&mut cached, false, |register| {
            reads.set(reads.get() + 1);
            Ok(register as u8)
        });
        assert_eq!(reads.get(), 15);
        assert_eq!(healthy.registers["23"], 0x23);
        let failure = snapshot_registers(&mut cached, true, |_| panic!("failure must use cache"));
        assert_eq!(failure.registers, healthy.registers);
        assert_eq!(failure.observed_at_unix_us, healthy.observed_at_unix_us);
        assert!(failure.error.is_none());

        let partial = snapshot_registers(&mut cached, false, |register| {
            if register == 0x42 {
                anyhow::bail!("USB unavailable")
            }
            Ok(register as u8)
        });
        assert_eq!(partial.registers.len(), 2);
        assert_eq!(partial.error.as_deref(), Some("USB unavailable"));
        let failure =
            snapshot_registers(&mut cached, true, |_| panic!("do not repeat failed probes"));
        assert_eq!(failure.registers, partial.registers);
        assert_eq!(failure.error, partial.error);
    }
    #[test]
    fn queued_failures_cannot_replay_before_a_completed_exposure() {
        for category in ["submission", "timeout", "short_read"] {
            let error = anyhow::Error::new(super::super::transfer::Failure::new(
                category,
                122_342_976,
                0,
                false,
            ))
            .context("first queued USB batch");
            assert!(!retained_read_allowed(&error, false), "{category}");
            assert!(retained_read_allowed(&error, true), "{category}");
        }
        for exposure_complete in [false, true] {
            assert!(!retained_read_allowed(
                &super::super::completion::CaptureCancelled.into(),
                exposure_complete
            ));
            assert!(!retained_read_allowed(
                &anyhow::anyhow!("sensor handshake failed"),
                exposure_complete
            ));
        }
    }
    #[test]
    fn raw16_does_not_rewrite_preserved_thermal_state_but_repairs_resets_and_errors() {
        for (resets, fails) in [(false, false), (true, false), (true, true)] {
            let heater = Cell::new(true);
            let restores = Cell::new(0);
            let verified = Cell::new(false);
            let result = finish_raw16(
                || {
                    if resets {
                        heater.set(false);
                    }
                    if fails {
                        anyhow::bail!("sensor setup USB failure");
                    }
                    Ok(())
                },
                || {
                    verified.set(true);
                    ensure!(heater.get(), "heater reset by setup");
                    Ok(())
                },
                || {
                    restores.set(restores.get() + 1);
                    heater.set(true);
                    Ok(())
                },
            );
            assert!(heater.get());
            assert_eq!(restores.get(), usize::from(resets));
            assert_eq!(verified.get(), !fails);
            assert_eq!(result.is_err(), fails);
        }
        assert!(
            finish_raw16(
                || Ok(()),
                || anyhow::bail!("unreadable state"),
                || anyhow::bail!("restore failed")
            )
            .is_err()
        );
    }

    #[test]
    fn connection_setup_validates_calibration_before_initializing_once() {
        let mut state = ConnectionState::default();
        let events = RefCell::new(Vec::new());
        let first = state
            .prepare(
                0,
                Revision::Original,
                [64, 64, 0, 0],
                || {
                    events.borrow_mut().push("calibration");
                    Ok(empty_calibration())
                },
                || {
                    events.borrow_mut().push("initialize_raw16");
                    Ok(())
                },
            )
            .unwrap();
        let second = state
            .prepare(
                0,
                Revision::Original,
                [64, 64, 0, 0],
                || panic!("factory calibration must be reused"),
                || panic!("sensor initialization must be reused"),
            )
            .unwrap();
        assert_eq!(*events.borrow(), ["calibration", "initialize_raw16"]);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(state.initialized);

        // A changed physical ROI gets its own correction map without resetting
        // the sensor. Per-capture ROI/controls are programmed after prepare.
        let reconfigured = state
            .prepare(
                0,
                Revision::Original,
                [128, 64, 16, 2],
                || panic!("ROI changes must not reread immutable calibration"),
                || panic!("ROI changes must not reset initialized hardware"),
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &reconfigured));
        assert_eq!(state.defects.as_ref().unwrap().0, [128, 64, 16, 2]);
        assert!(reconfigured.correct(&mut vec![0; 128 * 64 * 2]).is_ok());
        assert_eq!(Arc::strong_count(&first), 2);
    }

    #[test]
    fn failed_or_invalid_setup_is_not_cached_and_transport_changes_reload() {
        let mut state = ConnectionState::default();
        let initializations = Cell::new(0);
        let invalid = state.prepare(
            0,
            Revision::Original,
            [64, 64, 0, 0],
            || Ok(b"invalid ASID".to_vec()),
            || {
                initializations.set(initializations.get() + 1);
                Ok(())
            },
        );
        assert!(invalid.is_err());
        assert_eq!(initializations.get(), 0);
        assert!(state.calibration.is_none() && state.defects.is_none() && !state.initialized);
        let failed = state.prepare(
            0,
            Revision::Original,
            [64, 64, 0, 0],
            || Ok(empty_calibration()),
            || anyhow::bail!("initialization or thermal restore failed"),
        );
        assert!(failed.is_err());
        assert!(state.calibration.is_none() && state.defects.is_none() && !state.initialized);

        let loads = Cell::new(0);
        // A new transport generation models a USB failure or reopening the
        // handle. Revision changes also require a complete fresh setup.
        for (generation, revision) in [
            (0, Revision::Original),
            (1, Revision::Original),
            (1, Revision::P25),
        ] {
            state
                .prepare(
                    generation,
                    revision,
                    [64, 64, 0, 0],
                    || {
                        loads.set(loads.get() + 1);
                        Ok(empty_calibration())
                    },
                    || {
                        initializations.set(initializations.get() + 1);
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert_eq!(loads.get(), 3);
        assert_eq!(initializations.get(), 3);
    }

    #[test]
    fn sdk_arm_preserves_sensor_and_fpga_bits_with_ten_ms_delay() {
        let operations = RefCell::new(Vec::new());
        let flags = Cell::new(0xc1_u8);
        let sensor = Cell::new(0x21_u8);
        arm(
            |request, register, value, length| {
                operations
                    .borrow_mut()
                    .push((request, register, value, length));
                match (request, register) {
                    (0xbc, 0) => return Ok(vec![flags.get()]),
                    (0xb7, 0) => return Ok(vec![sensor.get()]),
                    (0xbd, 0) => flags.set(value as u8),
                    (0xb6, 0) => sensor.set(value as u8),
                    _ => {}
                }
                Ok(Vec::new())
            },
            |delay| {
                assert_eq!(delay, Duration::from_millis(10));
                operations.borrow_mut().push((0xff, 10, 0, 0));
            },
        )
        .unwrap();
        assert_eq!(
            *operations.borrow(),
            [
                (0xa9, 0, 0, 0),
                (0xbc, 0, 0, 1),
                (0xbd, 0, 0xd1, 0),
                (0xb6, 0x19e, 1, 0),
                (0xb7, 0, 0, 1),
                (0xb6, 0, 0x25, 0),
                (0xff, 10, 0, 0),
                (0xb7, 0, 0, 1),
                (0xb6, 0, 0x24, 0),
                (0xbc, 0, 0, 1),
                (0xbd, 0, 0xc1, 0),
            ]
        );
    }

    #[test]
    fn full_readout_guard_counts_only_known_ungated_time() {
        let guard = Duration::from_millis(600);
        for (ungated_ms, expected_ms) in [
            (None, 600),    // Short mode retains the complete post-ready guard.
            (Some(0), 600), // An early P25 ready indication cannot shorten it.
            (Some(487), 113),
            (Some(600), 0),
            (Some(900), 0),
        ] {
            assert_eq!(
                remaining_readout_guard(guard, ungated_ms.map(Duration::from_millis)),
                Duration::from_millis(expected_ms)
            );
        }
    }

    fn pixels(values: impl IntoIterator<Item = u16>) -> Vec<u8> {
        values.into_iter().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn output_reuses_native_hash_until_crop_or_binning_changes_pixels() {
        let sensor = Settings {
            width: 4,
            height: 4,
            ..Settings::default()
        };
        let data = pixels(0..16);
        let (meta, unchanged) = finish_output(
            &sensor,
            &sensor,
            &sensor,
            1,
            json!({"sha256":"native_hash"}),
            data.clone(),
        )
        .unwrap();
        assert_eq!(unchanged, data);
        assert_eq!(meta["sha256"], "native_hash");

        let cropped = Settings {
            width: 2,
            height: 2,
            x: 1,
            y: 1,
            ..sensor.clone()
        };
        let (meta, result) = finish_output(
            &cropped,
            &cropped,
            &sensor,
            1,
            json!({"sha256":"native_hash"}),
            data.clone(),
        )
        .unwrap();
        assert_eq!(result, pixels([5, 6, 9, 10]));
        assert_eq!(meta["sha256"], format!("{:x}", Sha256::digest(&result)));
        assert_eq!(meta["wireBytes"], 32);

        let binned = Settings {
            width: 2,
            height: 2,
            ..sensor.clone()
        };
        let (meta, result) = finish_output(
            &binned,
            &sensor,
            &sensor,
            2,
            json!({"sha256":"native_hash"}),
            data,
        )
        .unwrap();
        assert_eq!(result, pixels([2, 4, 10, 12]));
        assert_eq!(meta["sha256"], format!("{:x}", Sha256::digest(&result)));
        assert_eq!(meta["bin"], 2);
    }

    #[test]
    fn initialization_restores_thermal_hardware_after_configuration_and_errors() {
        // The original ASI6200 acceptance trace showed the requested heater bit
        // lost during initialization despite skipping explicit thermal defaults.
        // Model that side effect, including an uncertain USB write completion.
        for fail_stage in ["none", "usb", "calibration"] {
            let flags = Cell::new(0x40_u8); // Cooler and heater both enabled.
            let power = Cell::new(60_u16);
            let restores = Cell::new(0);
            let configurations = Cell::new(0);
            let result = initialize(
                Revision::Original,
                true,
                |request, register, _| {
                    assert!(request != 0xbd || ![0x19, 0x26].contains(&register));
                    if request == 0xaf {
                        flags.set(0x80);
                        power.set(14);
                        if fail_stage == "usb" {
                            anyhow::bail!("initialization USB completion failed");
                        }
                    }
                    Ok(())
                },
                || {
                    configurations.set(configurations.get() + 1);
                    // Restoration must also follow the calibration/setup work.
                    flags.set(0x80);
                    power.set(14);
                    if fail_stage == "calibration" {
                        anyhow::bail!("calibration read failed");
                    }
                    Ok(42)
                },
                || {
                    restores.set(restores.get() + 1);
                    flags.set(0x40);
                    power.set(60);
                    Ok(())
                },
            );
            assert_eq!(restores.get(), 1, "{fail_stage}");
            assert_eq!(flags.get(), 0x40, "{fail_stage}");
            assert_eq!(power.get(), 60, "{fail_stage}");
            assert_eq!(configurations.get(), i32::from(fail_stage != "usb"));
            match fail_stage {
                "none" => assert_eq!(result.unwrap(), 42),
                "usb" => assert_eq!(
                    result.unwrap_err().to_string(),
                    "initialization USB completion failed"
                ),
                _ => assert_eq!(result.unwrap_err().to_string(), "calibration read failed"),
            }
        }
    }

    #[test]
    fn failed_thermal_restore_cannot_acknowledge_clean_initialization_cancellation() {
        let error = initialize(
            Revision::Original,
            true,
            |_, _, _| Ok(()),
            || Err::<(), _>(super::super::completion::CaptureCancelled.into()),
            || anyhow::bail!("heater readback failed"),
        )
        .unwrap_err();
        assert!(!error.is::<super::super::completion::CaptureCancelled>());
        assert!(error.to_string().contains("heater readback failed"));
        assert!(error.to_string().contains("Capture cancelled"));
    }

    #[test]
    fn original_timing_matches_independent_sdk_trace() {
        // Non-P25 Camera Kit 20260917T205753Z: BC1c=3, HMAX=1515.
        for (height, microseconds, expected) in [
            (6388, 100000, (6440, 2558)),
            (256, 32, (308, 152)),
            (256, 100, (308, 152)),
            (256, 1000, (308, 146)),
            (256, 10000, (308, 86)),
            (256, 490000, (6488, 10)),
            (256, 999000, (13208, 10)),
            (256, 1000000, (460, 10)),
            (256, 60000000, (460, 10)),
        ] {
            let settings = Settings {
                height,
                microseconds,
                ..Settings::default()
            };
            assert_eq!(timing(&settings, Revision::Original), expected);
        }
        assert_eq!(Revision::from_register(3).unwrap(), Revision::Original);
        assert_eq!(Revision::from_register(5).unwrap(), Revision::P25);
        for unknown in [0, 1, 2, 4, 6, 255] {
            assert!(Revision::from_register(unknown).is_err());
        }
    }

    #[test]
    fn small_edge_rois_remain_inside_padded_sensor_readout() {
        for bin in 1..=4 {
            for (width, height) in [(64, 64), (64, 128), (128, 64), (512, 64)] {
                let settings = Settings {
                    width,
                    height,
                    x: ((9576 / bin - width) / 16) * 16,
                    y: ((6388 / bin - height) / 2) * 2,
                    ..Settings::default()
                };
                let raw = raw_settings(&settings, 100, bin).unwrap();
                let padded = sensor_settings(&raw);
                validate(&padded, 100).unwrap();
                assert!(padded.width * padded.height >= 65536);
                assert!(padded.x <= raw.x && padded.y <= raw.y);
                assert!(padded.x + padded.width >= raw.x + raw.width);
                assert!(padded.y + padded.height >= raw.y + raw.height);
            }
        }
    }

    #[test]
    fn timing_matches_sdk_141_observations() {
        let settings = Settings {
            width: 512,
            height: 256,
            microseconds: 100000,
            ..Settings::default()
        };
        assert_eq!(timing(&settings, Revision::P25), (0x8f4, 10));
        assert_eq!(
            timing(
                &Settings {
                    microseconds: 2_000_000,
                    ..settings
                },
                Revision::P25
            ),
            (555, 10)
        );
    }
    #[test]
    fn gain_boundaries_match_independent_sdk_141_trace() {
        // Observed register values from asi6200-sdk-controls, not generated
        // through this implementation. Covers every conversion/digital boundary.
        for (gain, analog, mode, readout, digital) in [
            (0, 0, 0, 8, 0),
            (60, 2042, 0, 8, 0),
            (61, 2066, 4, 10, 0),
            (62, 2089, 4, 10, 0),
            (99, 2785, 4, 10, 0),
            (100, 0, 1, 8, 0),
            (101, 46, 1, 8, 0),
            (159, 2018, 1, 8, 0),
            (160, 2042, 5, 10, 0),
            (161, 2066, 5, 10, 0),
            (279, 3573, 5, 10, 0),
            (280, 3579, 5, 12, 0),
            (281, 3585, 5, 12, 0),
            (460, 4030, 5, 12, 0),
            (461, 3966, 5, 12, 16),
            (520, 4030, 5, 12, 16),
            (521, 3966, 5, 12, 32),
            (700, 4030, 5, 12, 64),
        ] {
            assert_eq!(
                gain_registers(gain),
                (analog, mode, readout, digital),
                "gain {gain}"
            );
        }
    }

    #[test]
    fn long_exposures_keep_host_timed_sensor_registers_and_sdk_range() {
        let mut settings = Settings {
            width: 9576,
            height: 6388,
            microseconds: 1_000_000,
            ..Settings::default()
        };
        let registers = timing(&settings, Revision::P25);
        for duration in [
            30_000_001,
            60_000_000,
            120_000_000,
            1_200_000_000,
            MAX_EXPOSURE_US,
        ] {
            settings.microseconds = duration;
            assert!(validate(&settings, 100).is_ok());
            assert_eq!(timing(&settings, Revision::P25), registers);
        }
        settings.microseconds = MAX_EXPOSURE_US + 1;
        assert!(validate(&settings, 100).is_err());
    }
}
