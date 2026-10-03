//! Inert ABI fixture: every hardware-facing call is logged, no USB/vendor SDK.
#![allow(dead_code, non_snake_case)]
#[path = "../../crates/regain-zwo/src/asi/sdk/raw.rs"] mod raw;
use std::ffi::{c_char, c_int};
#[no_mangle] pub extern "C" fn ASIGetNumOfConnectedCameras() -> c_int {
    eprintln!("CALL enumerate"); 2
}
#[no_mangle] pub extern "C" fn ASIGetCameraProperty(_: *mut raw::CameraInfo, _: c_int) -> c_int {
    eprintln!("CALL FORBIDDEN property-sweep"); 16
}
#[no_mangle] pub extern "C" fn ASIOpenCamera(id: c_int) -> c_int {
    eprintln!("CALL open {id}"); if id == 1 { 0 } else { 2 }
}
#[no_mangle] pub unsafe extern "C" fn ASIGetCameraPropertyByID(id: c_int, out: *mut raw::CameraInfo) -> c_int {
    eprintln!("CALL property {id}");
    let mut info: raw::CameraInfo = std::mem::zeroed();
    for (a, b) in info.name.iter_mut().zip(b"Target fixture") { *a = *b as c_char; }
    info.camera_id = id; info.max_width = 64; info.max_height = 64;
    info.bit_depth = 16; info.supported_bins[0] = 1;
    info.supported_video_formats = [2, -1, -1, -1, -1, -1, -1, -1];
    *out = info; 0
}
#[no_mangle] pub extern "C" fn ASIInitCamera(id: c_int) -> c_int { eprintln!("CALL init {id}"); 0 }
#[no_mangle] pub unsafe extern "C" fn ASIGetSerialNumber(id: c_int, out: *mut u8) -> c_int {
    eprintln!("CALL serial {id}"); for n in 0..8 { *out.add(n) = 1; } 0
}
#[no_mangle] pub unsafe extern "C" fn ASIGetNumOfControls(_: c_int, out: *mut c_int) -> c_int { *out = 0; 0 }
#[no_mangle] pub extern "C" fn ASIGetSDKVersion() -> *const c_char { c"fixture".as_ptr() }
#[no_mangle] pub extern "C" fn ASICloseCamera(id: c_int) -> c_int { eprintln!("CALL close {id}"); 0 }
