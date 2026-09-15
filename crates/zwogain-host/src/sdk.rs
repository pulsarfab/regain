//! All SDK calls stay on the host's command thread; the parent owns watchdogs.
use crate::raw;
use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use serde_json::{Value, json};
use std::{
    ffi::{CStr, c_int, c_long},
    path::Path,
};

#[cfg(windows)]
pub const LIBRARY_NAME: &str = "ASICamera2.dll";
#[cfg(target_os = "linux")]
pub const LIBRARY_NAME: &str = "libASICamera2.so";
#[cfg(target_os = "macos")]
pub const LIBRARY_NAME: &str = "libASICamera2.dylib";

pub struct Sdk {
    lib: Library,
    id: Option<i32>,
}
#[derive(Debug)]
pub struct SdkError {
    pub code: i32,
    pub operation: String,
}
impl std::fmt::Display for SdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: ASI error {}", self.operation, self.code)
    }
}
impl std::error::Error for SdkError {}
impl Sdk {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "SDK path must be absolute");
        // SAFETY: application-controlled library, ABI declarations from bundled header.
        let lib = unsafe { Library::new(path) }.context("load ASICamera2")?;
        Ok(Self { lib, id: None })
    }
    unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Result<T> {
        Ok(*unsafe { self.lib.get::<T>(name)? })
    }
    fn check(code: i32, op: &str) -> Result<()> {
        if code != 0 {
            return Err(SdkError {
                code,
                operation: op.into(),
            }
            .into());
        }
        Ok(())
    }
    fn id(&self) -> Result<i32> {
        self.id.context("camera is not open")
    }
    pub fn list(&self) -> Result<Value> {
        let mut cameras = Vec::new();
        unsafe {
            let count =
                self.symbol::<raw::GetNumOfConnectedCameras>(b"ASIGetNumOfConnectedCameras\0")?();
            ensure!((0..=128).contains(&count), "invalid camera count");
            for index in 0..count {
                let mut info: raw::CameraInfo = std::mem::zeroed();
                Self::check(
                    self.symbol::<raw::GetCameraProperty>(b"ASIGetCameraProperty\0")?(
                        &mut info, index,
                    ),
                    "property",
                )?;
                // Enumeration deliberately does not open cameras owned by another driver.
                let name = String::from_utf8_lossy(
                    &info
                        .name
                        .iter()
                        .take_while(|&&v| v != 0)
                        .map(|&v| v as u8)
                        .collect::<Vec<_>>(),
                )
                .into_owned();
                cameras.push(json!({"id":info.camera_id,"name":name,"width":info.max_width,"height":info.max_height,
                    "color":info.is_color_camera != 0,"bayer":info.bayer_pattern,"pixelSize":info.pixel_size,
                    "bitDepth":info.bit_depth,"cooled":info.is_cooled_camera != 0,"shutter":info.has_mechanical_shutter != 0,
                    "usb3Host":info.is_usb3_host!=0,"usb3Camera":info.is_usb3_camera!=0,
                    "st4":info.has_st4_port!=0,"triggerCamera":info.is_trigger_camera!=0,
                    "bins":info.supported_bins.into_iter().take_while(|&v| v>0).collect::<Vec<_>>(),
                    "formats":info.supported_video_formats.into_iter().take_while(|&v| v>=0).collect::<Vec<_>>() }));
            }
        }
        Ok(json!(cameras))
    }
    pub fn open(&mut self, name: &str, serial: Option<&str>) -> Result<Value> {
        ensure!(self.id.is_none(), "already open");
        let list = self.list()?;
        let candidates: Vec<_> = list
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["name"] == name)
            .collect();
        ensure!(
            serial.is_some() || candidates.len() == 1,
            "camera name missing or ambiguous; serial required"
        );
        let mut last_error = None;
        for info in candidates {
            let id = info["id"].as_i64().unwrap() as i32;
            let opened = unsafe {
                Self::check(
                    self.symbol::<raw::OpenCamera>(b"ASIOpenCamera\0")?(id),
                    "open",
                )
            };
            if let Err(error) = opened {
                if serial.is_none() {
                    return Err(error);
                }
                eprintln!("Skipping unavailable camera {id}: {error:#}");
                last_error = Some(error);
                continue;
            }
            self.id = Some(id);
            let result = self.initialize();
            match result {
                Ok(mut details) if serial.is_none() || details["serial"].as_str() == serial => {
                    details["info"] = info.clone();
                    return Ok(details);
                }
                Ok(_) => self.close()?,
                Err(e) => {
                    let _ = self.close();
                    if serial.is_none() {
                        return Err(e);
                    }
                    eprintln!("Skipping unidentified camera {id}: {e:#}");
                    last_error = Some(e);
                }
            }
        }
        if let Some(error) = last_error {
            return Err(
                error.context("selected serial could not be found among accessible cameras")
            );
        }
        bail!("selected camera serial is not present")
    }
    fn initialize(&self) -> Result<Value> {
        unsafe {
            let id = self.id()?;
            Self::check(
                self.symbol::<raw::InitCamera>(b"ASIInitCamera\0")?(id),
                "init",
            )?;
            let mut serial = [0u8; 8];
            let code =
                self.symbol::<unsafe extern "C" fn(c_int, *mut u8) -> c_int>(
                    b"ASIGetSerialNumber\0",
                )?(id, serial.as_mut_ptr());
            let serial = if code == 0 && serial.iter().any(|&v| v != 0) {
                Some(
                    serial
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                )
            } else {
                None
            };
            let mut count = 0;
            Self::check(
                self.symbol::<raw::GetNumOfControls>(b"ASIGetNumOfControls\0")?(id, &mut count),
                "controls",
            )?;
            ensure!((0..=256).contains(&count), "invalid control count");
            let mut controls = Vec::new();
            for i in 0..count {
                let mut caps: raw::ControlCaps = std::mem::zeroed();
                Self::check(
                    self.symbol::<raw::GetControlCaps>(b"ASIGetControlCaps\0")?(id, i, &mut caps),
                    "caps",
                )?;
                match self.get(caps.control_type) {
                    Ok(value) => controls.push(
                        json!({"type":caps.control_type,"min":caps.min_value,"max":caps.max_value,
                        "default":caps.default_value,"writable":caps.is_writable!=0,"value":value,
                        "autoSupported":caps.is_auto_supported!=0,
                        "name":String::from_utf8_lossy(&caps.name.iter().take_while(|&&v|v!=0).map(|&v|v as u8).collect::<Vec<_>>())}),
                    ),
                    // Like the native NINA driver, ignore advertised controls that cannot be queried.
                    Err(e) => eprintln!("Unavailable control {}: {e}", caps.control_type),
                }
            }
            let version = self.symbol::<raw::GetSdkVersion>(b"ASIGetSDKVersion\0")?();
            let version = if version.is_null() {
                "unknown".into()
            } else {
                CStr::from_ptr(version).to_string_lossy().into_owned()
            };
            Ok(json!({"serial":serial,"controls":controls,"sdkVersion":version}))
        }
    }
    pub fn get(&self, control: i32) -> Result<i64> {
        Ok(self.control_state(control)?.0)
    }
    pub fn control_state(&self, control: i32) -> Result<(i64, bool)> {
        let (mut value, mut auto): (c_long, c_int) = (0, 0);
        unsafe {
            Self::check(
                self.symbol::<raw::GetControlValue>(b"ASIGetControlValue\0")?(
                    self.id()?,
                    control,
                    &mut value,
                    &mut auto,
                ),
                "get control",
            )?;
        }
        // C long is 32-bit on Windows and 64-bit on 64-bit Unix.
        #[allow(clippy::unnecessary_cast)]
        Ok((value as i64, auto != 0))
    }
    pub fn set(&self, control: i32, value: i64) -> Result<()> {
        self.set_control_state(control, value, false)
    }
    pub fn set_control_state(&self, control: i32, value: i64, auto: bool) -> Result<()> {
        let value = c_long::try_from(value)?;
        unsafe {
            Self::check(
                self.symbol::<raw::SetControlValue>(b"ASISetControlValue\0")?(
                    self.id()?,
                    control,
                    value,
                    i32::from(auto),
                ),
                "set control",
            )
        }
    }
    pub fn start(&self, p: &crate::Exposure) -> Result<()> {
        unsafe {
            let id = self.id()?;
            // No SDK automatic exposure, orientation changes or hidden dark subtraction.
            Self::check(
                self.symbol::<raw::DisableDarkSubtract>(b"ASIDisableDarkSubtract\0")?(id),
                "disable dark subtract",
            )?;
            Self::check(
                self.symbol::<raw::SetStartPos>(b"ASISetStartPos\0")?(id, 0, 0),
                "reset origin",
            )?;
            Self::check(
                self.symbol::<raw::SetRoiFormat>(b"ASISetROIFormat\0")?(
                    id, p.width, p.height, p.bin, 2,
                ),
                "RAW16 ROI",
            )?;
            Self::check(
                self.symbol::<raw::SetStartPos>(b"ASISetStartPos\0")?(id, p.x, p.y),
                "ROI origin",
            )?;
            let (mut width, mut height, mut bin, mut format) = (0, 0, 0, 0);
            Self::check(
                self.symbol::<raw::GetRoiFormat>(b"ASIGetROIFormat\0")?(
                    id,
                    &mut width,
                    &mut height,
                    &mut bin,
                    &mut format,
                ),
                "read ROI",
            )?;
            ensure!(
                (width, height, bin, format) == (p.width, p.height, p.bin, 2),
                "SDK ROI differs from requested buffer geometry"
            );
            let (mut x, mut y) = (0, 0);
            Self::check(
                self.symbol::<raw::GetStartPos>(b"ASIGetStartPos\0")?(id, &mut x, &mut y),
                "read origin",
            )?;
            ensure!((x, y) == (p.x, p.y), "SDK ROI origin differs from request");
            self.set(1, p.microseconds)?;
            Self::check(
                self.symbol::<unsafe extern "C" fn(c_int, c_int) -> c_int>(b"ASIStartExposure\0")?(
                    id,
                    p.dark as i32,
                ),
                "start exposure",
            )
        }
    }
    pub fn status(&self) -> Result<i32> {
        let mut status = 0;
        unsafe {
            Self::check(
                self.symbol::<unsafe extern "C" fn(c_int, *mut c_int) -> c_int>(
                    b"ASIGetExpStatus\0",
                )?(self.id()?, &mut status),
                "exposure status",
            )?;
        }
        Ok(status)
    }
    pub fn download(&self, bytes: &mut [u8]) -> Result<()> {
        ensure!(self.status()? == 2, "exposure not ready for download");
        unsafe {
            Self::check(
                self.symbol::<unsafe extern "C" fn(c_int, *mut u8, c_long) -> c_int>(
                    b"ASIGetDataAfterExp\0",
                )?(
                    self.id()?,
                    bytes.as_mut_ptr(),
                    c_long::try_from(bytes.len())?,
                ),
                "download",
            )
        }
    }
    pub fn stop(&self) -> Result<()> {
        unsafe {
            Self::check(
                self.symbol::<raw::CloseCamera>(b"ASIStopExposure\0")?(self.id()?),
                "stop",
            )
        }
    }
    pub fn close(&mut self) -> Result<()> {
        if let Some(id) = self.id.take() {
            unsafe {
                Self::check(
                    self.symbol::<raw::CloseCamera>(b"ASICloseCamera\0")?(id),
                    "close",
                )?;
            }
        }
        Ok(())
    }
}
impl Drop for Sdk {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
