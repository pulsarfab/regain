//! Synthetic SDK ABI fixture. It never loads a vendor library or accesses USB.
#![allow(non_snake_case, dead_code)]
#[path = "../../crates/regain-zwo/src/asi/sdk/raw.rs"]
mod raw;
use std::ffi::{c_char, c_int, c_long};
use std::sync::Mutex;
struct State {
    wb: [(c_long, c_int); 2],
    width: c_int,
    height: c_int,
    x: c_int,
    y: c_int,
    active: bool,
}
static STATE: Mutex<State> = Mutex::new(State {
    wb: [(62, 0), (99, 1)],
    width: 64,
    height: 64,
    x: 0,
    y: 0,
    active: false,
});
#[no_mangle]
pub extern "C" fn ASIGetNumOfConnectedCameras() -> c_int {
    1
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetCameraProperty(out: *mut raw::CameraInfo, _: c_int) -> c_int {
    let mut info: raw::CameraInfo = std::mem::zeroed();
    for (a, b) in info.name.iter_mut().zip(b"WB fixture") {
        *a = *b as c_char;
    }
    info.max_width = 64;
    info.max_height = 64;
    info.is_color_camera = 1;
    info.bit_depth = 16;
    info.supported_bins[0] = 1;
    info.supported_video_formats = [2, -1, -1, -1, -1, -1, -1, -1];
    *out = info;
    0
}
#[no_mangle]
pub extern "C" fn ASIOpenCamera(_: c_int) -> c_int {
    0
}
#[no_mangle]
pub extern "C" fn ASIInitCamera(_: c_int) -> c_int {
    0
}
#[no_mangle]
pub extern "C" fn ASICloseCamera(_: c_int) -> c_int {
    STATE.lock().unwrap().active = false;
    0
}
#[no_mangle]
pub extern "C" fn ASIGetSDKVersion() -> *const c_char {
    b"WB fixture\0".as_ptr() as *const c_char
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetSerialNumber(_: c_int, out: *mut u8) -> c_int {
    for i in 0..8 {
        *out.add(i) = 1;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetNumOfControls(_: c_int, out: *mut c_int) -> c_int {
    *out = 3;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetControlCaps(
    _: c_int,
    index: c_int,
    out: *mut raw::ControlCaps,
) -> c_int {
    let mut caps: raw::ControlCaps = std::mem::zeroed();
    caps.control_type = [3, 4, 9][index as usize];
    caps.min_value = 0;
    caps.max_value = 100;
    caps.is_auto_supported = 1;
    caps.is_writable = 1;
    *out = caps;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetControlValue(
    _: c_int,
    c: c_int,
    out: *mut c_long,
    auto: *mut c_int,
) -> c_int {
    let (v, a) = if c == 3 || c == 4 {
        STATE.lock().unwrap().wb[(c - 3) as usize]
    } else {
        (0, 0)
    };
    *out = v;
    *auto = a;
    0
}
#[no_mangle]
pub extern "C" fn ASISetControlValue(_: c_int, c: c_int, value: c_long, auto: c_int) -> c_int {
    if c == 3 || c == 4 {
        if c == 4 && value == 50 && std::env::var_os("REGAIN_FIXTURE_REJECT_WB").is_some() {
            return 0;
        }
        if c == 3 && value == 62 && std::env::var_os("REGAIN_FIXTURE_RESTORE_FAIL").is_some() {
            return 16;
        }
        STATE.lock().unwrap().wb[(c - 3) as usize] = (value, auto);
    }
    0
}
#[no_mangle]
pub extern "C" fn ASIDisableDarkSubtract(_: c_int) -> c_int {
    0
}
#[no_mangle]
pub extern "C" fn ASISetROIFormat(_: c_int, w: c_int, h: c_int, _: c_int, _: c_int) -> c_int {
    let mut s = STATE.lock().unwrap();
    s.width = w;
    s.height = h;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetROIFormat(
    _: c_int,
    w: *mut c_int,
    h: *mut c_int,
    b: *mut c_int,
    f: *mut c_int,
) -> c_int {
    let s = STATE.lock().unwrap();
    *w = s.width;
    *h = s.height;
    *b = 1;
    *f = 2;
    0
}
#[no_mangle]
pub extern "C" fn ASISetStartPos(_: c_int, x: c_int, y: c_int) -> c_int {
    let mut s = STATE.lock().unwrap();
    s.x = x;
    s.y = y;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetStartPos(_: c_int, x: *mut c_int, y: *mut c_int) -> c_int {
    let s = STATE.lock().unwrap();
    *x = s.x;
    *y = s.y;
    0
}
#[no_mangle]
pub extern "C" fn ASIStartExposure(_: c_int, _: c_int) -> c_int {
    let mut s = STATE.lock().unwrap();
    if s.wb != [(50, 0), (50, 0)] {
        return 16;
    }
    s.active = true;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetExpStatus(_: c_int, out: *mut c_int) -> c_int {
    *out = if STATE.lock().unwrap().active { 2 } else { 0 };
    0
}
#[no_mangle]
pub extern "C" fn ASIStopExposure(_: c_int) -> c_int {
    STATE.lock().unwrap().active = false;
    0
}
#[no_mangle]
pub unsafe extern "C" fn ASIGetDataAfterExp(_: c_int, data: *mut u8, size: c_long) -> c_int {
    let mut s = STATE.lock().unwrap();
    if size != c_long::from(s.width) * c_long::from(s.height) * 2 {
        return 9;
    }
    for i in 0..size as usize / 2 {
        let (x, y) = (i % s.width as usize, i / s.width as usize);
        let v: u16 = match (x % 2, y % 2) {
            (0, 0) => 4000,
            (1, 1) => 16000,
            _ => 8000,
        };
        let bytes = v.to_le_bytes();
        *data.add(i * 2) = bytes[0];
        *data.add(i * 2 + 1) = bytes[1];
    }
    s.active = false;
    0
}
