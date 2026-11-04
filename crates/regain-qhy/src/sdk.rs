//! Dynamic QHY SDK reference. No linked SDK or SDK dependency in other vendors.
//!
//! ABI and sequencing are from the installed QHY qhyccd.h and SDK API manual:
//! open -> read mode -> stream mode -> initialize -> bits -> bin/ROI -> expose.
//! Read mode changes require a fresh open, not reinitialization between frames.
use anyhow::{Context, Result, ensure};
use libloading::Library;
use serde::Serialize;
use std::{
    ffi::{CString, c_char, c_void},
    path::Path,
    time::Instant,
};

type Handle = *mut c_void;
const ERROR: u32 = u32::MAX;
const MAX_FRAME: usize = 512 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct Identity {
    pub id: String,
    pub model: String,
}
#[derive(Debug, Serialize)]
pub struct ReadMode {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Serialize)]
pub struct Chip {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub pixel_width: f64,
    pub pixel_height: f64,
    pub chip_width: f64,
    pub chip_height: f64,
}

/// One SDK instance per isolated worker; SDK global resource teardown belongs to it.
/// This type is deliberately neither Send nor Sync: native calls share one owner.
pub struct Sdk {
    lib: Library,
    handle: Handle,
    initialized: bool,
    exposure: Option<(u32, u32)>,
    download_attempted: bool,
    integration: Option<(Instant, u32)>,
}

macro_rules! invoke {
    ($self:expr, $name:literal, ($($ty:ty),*) -> $ret:ty $(, $arg:expr)* $(,)?) => {{
        let start = Instant::now();
        // SAFETY: each declaration below matches the vendor's STDCALL header.
        // Pointer storage lives across this synchronous call; library outlives symbols.
        let result = unsafe {
            let function = $self.lib.get::<unsafe extern "system" fn($($ty),*) -> $ret>(concat!($name, "\0").as_bytes())
                .with_context(|| concat!("Missing QHY SDK export ", $name))?;
            function($($arg),*)
        };
        crate::diagnostics::sdk(serde_json::json!({"operation":$name,"elapsedUs":start.elapsed().as_micros(),"result":format!("{result:?}")}));
        result
    }};
}

fn checked(name: &str, result: u32) -> Result<()> {
    ensure!(result == 0, "{name}: QHY error 0x{result:08x}");
    Ok(())
}
fn text(bytes: &[c_char]) -> Result<String> {
    let end = bytes
        .iter()
        .position(|&v| v == 0)
        .context("Unterminated QHY string")?;
    Ok(String::from_utf8(
        // C char is signed on Windows/x86 and unsigned on some ARM targets.
        bytes[..end].iter().map(|v| v.to_ne_bytes()[0]).collect(),
    )?)
}
pub fn frame_bytes(width: u32, height: u32) -> Result<usize> {
    let size = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|v| v.checked_mul(2))
        .context("Frame size overflow")?;
    ensure!(
        size > 0 && size <= MAX_FRAME as u64,
        "Invalid QHY frame size"
    );
    Ok(size as usize)
}

impl Sdk {
    pub fn wheel_slots(&self) -> Result<u8> {
        let h = self.handle()?;
        checked(
            "IsQHYCCDCFWPlugged",
            invoke!(self,"IsQHYCCDCFWPlugged",(Handle)->u32,h),
        )?;
        let slots = self.get(44)?;
        ensure!(
            (2.0..=16.0).contains(&slots) && slots.fract() == 0.,
            "Invalid QHY wheel geometry"
        );
        Ok(slots as u8)
    }
    pub fn wheel_position(&self) -> Result<Option<u8>> {
        let h = self.handle()?;
        let mut response = [0u8; 64];
        checked(
            "GetQHYCCDCFWStatus",
            invoke!(self,"GetQHYCCDCFWStatus",(Handle,*mut u8)->u32,h,response.as_mut_ptr()),
        )?;
        if response[0] == b'N' {
            return Ok(None);
        }
        Ok(Some(crate::cfw::decode_position(response[0])?))
    }
    pub fn wheel_move(&self, position: u8) -> Result<()> {
        ensure!(position < 16, "Invalid wheel position");
        let h = self.handle()?;
        let command = [b"0123456789ABCDEF"[position as usize], 0];
        checked(
            "SendOrder2QHYCCDCFW",
            invoke!(self,"SendOrder2QHYCCDCFW",(Handle,*const u8,u32)->u32,h,command.as_ptr(),1),
        )
    }
    pub fn default_path() -> std::path::PathBuf {
        if let Some(path) = std::env::var_os("REGAIN_QHY_SDK") {
            return path.into();
        }
        #[cfg(windows)]
        {
            std::path::PathBuf::from(
                std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
            )
            .join("QHYCCD/AllInOne/sdk/x64/qhyccd.dll")
        }
        #[cfg(not(windows))]
        {
            std::path::PathBuf::from("libqhyccd.so")
        }
    }
    pub fn ready(&self) -> Result<bool> {
        ensure!(self.exposure.is_some(), "No QHY exposure");
        // This SDK reports <=100 even immediately after ExpSingleFrame on the
        // QHY600. Do not enter its blocking download before integration ends:
        // the owner must remain available for thermal control and cancellation.
        if self
            .integration
            .is_some_and(|(at, us)| at.elapsed().as_micros() < u128::from(us))
        {
            return Ok(false);
        }
        let result = invoke!(self,"GetQHYCCDExposureRemaining",(Handle)->u32,self.handle()?);
        ensure!(result != ERROR, "QHY exposure readiness failed");
        Ok(result <= 100)
    }
    pub fn load(path: &Path) -> Result<Self> {
        // SAFETY: caller explicitly supplies a trusted vendor library. On Windows
        // libloading's DLL-load-directory flag resolves QHY's sibling dependencies.
        #[cfg(windows)]
        let lib: Library =
            unsafe { libloading::os::windows::Library::load_with_flags(path, 0x100 | 0x1000)? }
                .into();
        #[cfg(not(windows))]
        let lib = unsafe { Library::new(path)? };
        let mut sdk = Self {
            lib,
            handle: std::ptr::null_mut(),
            initialized: false,
            exposure: None,
            download_attempted: false,
            integration: None,
        };
        // Keep SDK console messages out of the camera's framed stdout stream.
        invoke!(sdk,"EnableQHYCCDMessage",(bool)->(),false);
        invoke!(sdk,"EnableQHYCCDLogFile",(bool)->(),false);
        checked(
            "InitQHYCCDResource",
            invoke!(sdk,"InitQHYCCDResource",()->u32),
        )?;
        sdk.initialized = true;
        Ok(sdk)
    }
    pub fn version(&self) -> Result<[u32; 4]> {
        let mut v = [0; 4];
        checked(
            "GetQHYCCDSDKVersion",
            invoke!(self,"GetQHYCCDSDKVersion",(*mut u32,*mut u32,*mut u32,*mut u32)->u32,
            &mut v[0],&mut v[1],&mut v[2],&mut v[3]),
        )?;
        Ok(v)
    }
    pub fn list(&self) -> Result<Vec<Identity>> {
        ensure!(
            self.handle.is_null(),
            "Discovery is unavailable while a camera is open"
        );
        let count = invoke!(self,"ScanQHYCCD",()->u32);
        ensure!(count < 128, "Invalid QHY camera count: {count}");
        (0..count).map(|i| {
            let (mut id,mut model)=([0;256],[0;256]);
            checked("GetQHYCCDId",invoke!(self,"GetQHYCCDId",(u32,*mut c_char)->u32,i,id.as_mut_ptr()))?;
            checked("GetQHYCCDModel",invoke!(self,"GetQHYCCDModel",(*mut c_char,*mut c_char)->u32,id.as_mut_ptr(),model.as_mut_ptr()))?;
            Ok(Identity{id:text(&id)?,model:text(&model)?})
        }).collect()
    }
    pub fn open(&mut self, id: &str, mode: u32) -> Result<()> {
        ensure!(self.handle.is_null(), "Camera already open");
        let id = CString::new(id)?;
        self.handle = invoke!(self,"OpenQHYCCD",(*const c_char)->Handle,id.as_ptr());
        ensure!(!self.handle.is_null(), "OpenQHYCCD returned null");
        let result = (|| {
            ensure!(
                self.read_modes()?.iter().any(|m| m.id == mode),
                "Unsupported read mode {mode}"
            );
            checked(
                "SetQHYCCDReadMode",
                invoke!(self,"SetQHYCCDReadMode",(Handle,u32)->u32,self.handle,mode),
            )?;
            checked(
                "SetQHYCCDStreamMode",
                invoke!(self,"SetQHYCCDStreamMode",(Handle,u8)->u32,self.handle,0),
            )?;
            checked(
                "InitQHYCCD",
                invoke!(self,"InitQHYCCD",(Handle)->u32,self.handle),
            )?;
            checked(
                "SetQHYCCDBitsMode",
                invoke!(self,"SetQHYCCDBitsMode",(Handle,u32)->u32,self.handle,16),
            )
        })();
        if result.is_err() {
            let _ = self.close();
        }
        result
    }
    fn handle(&self) -> Result<Handle> {
        ensure!(!self.handle.is_null(), "QHY camera not open");
        Ok(self.handle)
    }
    pub fn read_modes(&self) -> Result<Vec<ReadMode>> {
        let h = self.handle()?;
        let mut n = 0;
        checked(
            "GetQHYCCDNumberOfReadModes",
            invoke!(self,"GetQHYCCDNumberOfReadModes",(Handle,*mut u32)->u32,h,&mut n),
        )?;
        ensure!((1..=64).contains(&n), "Invalid QHY read mode count");
        (0..n).map(|id| {
            let mut name=[0;256];let(mut w,mut height)=(0,0);
            checked("GetQHYCCDReadModeName",invoke!(self,"GetQHYCCDReadModeName",(Handle,u32,*mut c_char)->u32,h,id,name.as_mut_ptr()))?;
            checked("GetQHYCCDReadModeResolution",invoke!(self,"GetQHYCCDReadModeResolution",(Handle,u32,*mut u32,*mut u32)->u32,h,id,&mut w,&mut height))?;
            Ok(ReadMode{id,name:text(&name)?,width:w,height})
        }).collect()
    }
    pub fn chip(&self) -> Result<Chip> {
        let h = self.handle()?;
        let mut chip = Chip {
            width: 0,
            height: 0,
            bits: 0,
            pixel_width: 0.,
            pixel_height: 0.,
            chip_width: 0.,
            chip_height: 0.,
        };
        checked(
            "GetQHYCCDChipInfo",
            invoke!(self,"GetQHYCCDChipInfo",(Handle,*mut f64,*mut f64,*mut u32,*mut u32,*mut f64,*mut f64,*mut u32)->u32,h,
            &mut chip.chip_width,&mut chip.chip_height,&mut chip.width,&mut chip.height,&mut chip.pixel_width,&mut chip.pixel_height,&mut chip.bits),
        )?;
        frame_bytes(chip.width, chip.height)?;
        ensure!(
            chip.pixel_width.is_finite()
                && chip.pixel_width > 0.
                && chip.pixel_height.is_finite()
                && chip.pixel_height > 0.,
            "Invalid pixel geometry"
        );
        Ok(chip)
    }
    pub fn available(&self, control: u32) -> Result<u32> {
        let h = self.handle()?;
        Ok(invoke!(self,"IsQHYCCDControlAvailable",(Handle,u32)->u32,h,control))
    }
    pub fn range(&self, control: u32) -> Result<[f64; 3]> {
        let h = self.handle()?;
        let mut r = [0.; 3];
        checked(
            "GetQHYCCDParamMinMaxStep",
            invoke!(self,"GetQHYCCDParamMinMaxStep",(Handle,u32,*mut f64,*mut f64,*mut f64)->u32,h,control,&mut r[0],&mut r[1],&mut r[2]),
        )?;
        ensure!(
            r.iter().all(|v| v.is_finite()) && r[0] <= r[1] && r[2] > 0.,
            "Invalid QHY control range"
        );
        Ok(r)
    }
    pub fn get(&self, control: u32) -> Result<f64> {
        let h = self.handle()?;
        let v = invoke!(self,"GetQHYCCDParam",(Handle,u32)->f64,h,control);
        ensure!(
            v.is_finite() && v != f64::from(ERROR),
            "Invalid QHY telemetry/control {control}"
        );
        Ok(v)
    }
    pub fn set(&self, control: u32, value: f64) -> Result<()> {
        ensure!(value.is_finite(), "Nonfinite control value");
        let h = self.handle()?;
        crate::diagnostics::sdk(serde_json::json!({
            "operation":"SetQHYCCDParam.request","control":control,"value":value
        }));
        checked(
            "SetQHYCCDParam",
            invoke!(self,"SetQHYCCDParam",(Handle,u32,f64)->u32,h,control,value),
        )
    }
    pub fn start_full_frame(&mut self, microseconds: u32) -> Result<()> {
        ensure!(
            self.exposure.is_none(),
            "Prior exposure not consumed or stopped"
        );
        let h = self.handle()?;
        let chip = self.chip()?;
        let range = self.range(8)?;
        ensure!(
            microseconds > 0
                && f64::from(microseconds) >= range[0]
                && f64::from(microseconds) <= range[1],
            "Exposure outside camera range"
        );
        checked(
            "SetQHYCCDBinMode",
            invoke!(self,"SetQHYCCDBinMode",(Handle,u32,u32)->u32,h,1,1),
        )?;
        checked(
            "SetQHYCCDResolution",
            invoke!(self,"SetQHYCCDResolution",(Handle,u32,u32,u32,u32)->u32,h,0,0,chip.width,chip.height),
        )?;
        self.set(8, f64::from(microseconds))?;
        checked(
            "SetQHYCCDSingleFrameTimeOut",
            invoke!(self,"SetQHYCCDSingleFrameTimeOut",(Handle,u32)->u32,h,microseconds.div_ceil(1000).saturating_add(60000)),
        )?;
        // Once dispatched, even an error may mean hardware started. Require
        // explicit cleanup before another start; never silently replay a write.
        self.exposure = Some((chip.width, chip.height));
        self.integration = Some((Instant::now(), microseconds));
        self.download_attempted = false;
        let rc = invoke!(self,"ExpQHYCCDSingleFrame",(Handle)->u32,h);
        ensure!(
            matches!(rc, 0 | 0x2000),
            "ExpQHYCCDSingleFrame: QHY error {rc:#x}"
        );
        Ok(())
    }
    pub fn download(&mut self) -> Result<Vec<u8>> {
        let h = self.handle()?;
        ensure!(
            !self.download_attempted,
            "QHY download already attempted; stop/reopen before replacing the exposure"
        );
        let (width, height) = self.exposure.context("No QHY exposure")?;
        let expected = frame_bytes(width, height)?;
        let capacity = invoke!(self,"GetQHYCCDMemLength",(Handle)->u32,h) as usize;
        ensure!(
            (expected..=MAX_FRAME).contains(&capacity),
            "Invalid SDK image allocation {capacity}"
        );
        let mut pixels = crate::image::zeroed(capacity)?;
        let (mut w, mut height_out, mut bits, mut channels) = (0, 0, 0, 0);
        // The documented SDK call is consumptive. Do not claim same-frame
        // retries or keep a ready state after it fails.
        self.download_attempted = true;
        checked(
            "GetQHYCCDSingleFrame",
            invoke!(self,"GetQHYCCDSingleFrame",(Handle,*mut u32,*mut u32,*mut u32,*mut u32,*mut u8)->u32,h,&mut w,&mut height_out,&mut bits,&mut channels,pixels.as_mut_ptr()),
        )?;
        ensure!(
            (w, height_out, bits, channels) == (width, height, 16, 1),
            "QHY returned unexpected image geometry/format"
        );
        self.exposure = None;
        self.download_attempted = false;
        pixels.truncate(expected);
        Ok(pixels)
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.exposure.is_some() {
            let h = self.handle()?;
            checked(
                "CancelQHYCCDExposingAndReadout",
                invoke!(self,"CancelQHYCCDExposingAndReadout",(Handle)->u32,h),
            )?;
            self.exposure = None;
            self.download_attempted = false;
        }
        Ok(())
    }
    pub fn close(&mut self) -> Result<()> {
        if self.handle.is_null() {
            return Ok(());
        }
        let stopped = self.stop();
        let h = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        self.exposure = None;
        self.download_attempted = false;
        let closed = checked("CloseQHYCCD", invoke!(self,"CloseQHYCCD",(Handle)->u32,h));
        stopped.and(closed)
    }
}
impl Drop for Sdk {
    fn drop(&mut self) {
        let _ = self.close();
        if self.initialized {
            // SAFETY: release after closing the sole camera and before unloading.
            unsafe {
                if let Ok(f) = self
                    .lib
                    .get::<unsafe extern "system" fn() -> u32>(b"ReleaseQHYCCDResource\0")
                {
                    f();
                }
            }
        }
    }
}

/// Reference CLI exposes bounded captures and inspection alongside the worker.
pub fn run(args: Vec<String>) -> Result<()> {
    if !args
        .iter()
        .any(|a| matches!(a.as_str(), "--capture" | "--inspect" | "--list" | "--help"))
    {
        return crate::worker::run(false, args);
    }
    if args.iter().any(|a| a == "--help") {
        println!(
            "QHY SDK reference: --list | --inspect | --capture --output NEW_DIRECTORY\n  --sdk LIBRARY --serial EXACT_QHY_ID --read-mode N --frames N --microseconds N\nCaptures are full-sensor RAW16. No same-frame reread is assumed."
        );
        return Ok(());
    }
    let mut sdk_path = None;
    let mut serial = None;
    let mut mode = 0;
    let mut frames = 3u32;
    let mut microseconds = 5_000_000u32;
    let mut output = None;
    let mut command = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--list" | "--inspect" | "--capture" => {
                ensure!(command.is_none(), "Choose one command");
                command = Some(arg.as_str());
            }
            "--sdk" => {
                sdk_path = Some(std::path::PathBuf::from(
                    iter.next().context("--sdk requires a path")?,
                ))
            }
            "--serial" => {
                serial = Some(iter.next().context("--serial requires identity")?.as_str())
            }
            "--read-mode" => {
                mode = iter
                    .next()
                    .context("--read-mode requires number")?
                    .parse()?
            }
            "--frames" => frames = iter.next().context("--frames requires number")?.parse()?,
            "--microseconds" => {
                microseconds = iter
                    .next()
                    .context("--microseconds requires number")?
                    .parse()?
            }
            "--output" => {
                output = Some(std::path::PathBuf::from(
                    iter.next().context("--output requires new directory")?,
                ))
            }
            _ => anyhow::bail!("Unknown QHY option {arg}"),
        }
    }
    let command = command.context("Choose --list, --inspect or --capture")?;
    ensure!(
        (1..=20).contains(&frames) && (1..=30_000_000).contains(&microseconds),
        "Invalid bounded reference capture"
    );
    let sdk_path = sdk_path.context("Specify --sdk with the trusted QHY SDK library path")?;
    if command == "--capture" {
        std::fs::create_dir(
            output
                .as_ref()
                .context("Capture requires --output NEW_DIRECTORY")?,
        )?;
    }
    // A native DLL can hang despite its own timeout. This reference process is
    // disposable; no caller must wait indefinitely. Production uses its supervisor.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(
            60 + u64::from(frames) * (u64::from(microseconds).div_ceil(1_000_000) + 65),
        ));
        eprintln!("QHY reference deadline exceeded");
        std::process::exit(124);
    });
    let mut sdk = Sdk::load(&sdk_path)?;
    let cameras = sdk.list()?;
    if command == "--list" {
        println!("{}", serde_json::to_string_pretty(&cameras)?);
        return Ok(());
    }
    let matches: Vec<_> = cameras
        .iter()
        .filter(|c| serial.is_none_or(|id| id == c.id))
        .collect();
    ensure!(
        matches.len() == 1,
        "Select exactly one QHY camera with --serial"
    );
    let identity = matches[0];
    sdk.open(&identity.id, mode)?;
    let chip = sdk.chip()?;
    let metadata = serde_json::json!({"identity":identity,"sdkVersion":sdk.version()?,"chip":chip,"readModes":sdk.read_modes()?,"selectedReadMode":mode});
    println!("{}", serde_json::to_string_pretty(&metadata)?);
    if command == "--capture" {
        let output = output.unwrap();
        std::fs::write(
            output.join("camera.json"),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        for frame in 0..frames {
            let start = Instant::now();
            sdk.start_full_frame(microseconds)?;
            let pixels = sdk.download()?;
            std::fs::write(output.join(format!("frame-{frame}.raw")), &pixels)?;
            let record = serde_json::json!({"frame":frame,"bytes":pixels.len(),"microseconds":microseconds,"elapsedMs":start.elapsed().as_millis()});
            std::fs::write(
                output.join(format!("frame-{frame}.json")),
                serde_json::to_vec_pretty(&record)?,
            )?;
            println!("{record}");
        }
    }
    sdk.close()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_frame_and_untrusted_geometry_bounds() {
        assert_eq!(frame_bytes(9600, 6422).unwrap(), 123302400);
        assert!(frame_bytes(0, 6422).is_err());
        assert!(frame_bytes(u32::MAX, u32::MAX).is_err());
        assert!(text(&[65, 66]).is_err());
        assert_eq!(text(&[65, 66, 0, 67]).unwrap(), "AB");
    }
}
