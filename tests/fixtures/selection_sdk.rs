#![allow(dead_code, non_snake_case)]
#[path="../../crates/regain-host/src/raw.rs"] mod raw;
#[no_mangle] pub extern "C" fn ASIGetNumOfConnectedCameras() -> i32 { 2 }
#[no_mangle] pub unsafe extern "C" fn ASIGetCameraProperty(out: *mut raw::CameraInfo, id: i32) -> i32 {
    let mut info: raw::CameraInfo = std::mem::zeroed();
    for (a,b) in info.name.iter_mut().zip(b"Review Twin") { *a=*b as i8; }
    info.camera_id=id; info.max_width=64; info.max_height=64;
    info.supported_bins[0]=1; info.supported_video_formats=[2,-1,-1,-1,-1,-1,-1,-1];
    info.pixel_size=1.0; info.bit_depth=16; *out=info; 0
}
#[no_mangle] pub extern "C" fn ASIOpenCamera(id: i32) -> i32 {
    eprintln!("FAKE ASIOpenCamera({id})"); if id==0 {16} else {0}
}
#[no_mangle] pub extern "C" fn ASICloseCamera(_:i32) -> i32 {0}
#[no_mangle] pub extern "C" fn ASIInitCamera(_:i32) -> i32 {0}
#[no_mangle] pub unsafe extern "C" fn ASIGetSerialNumber(id:i32, out:*mut u8) -> i32 {
    for n in 0..8 { *out.add(n)=(id+1) as u8; } 0
}
#[no_mangle] pub unsafe extern "C" fn ASIGetNumOfControls(_:i32, out:*mut i32) -> i32 { *out=0; 0 }
#[no_mangle] pub extern "C" fn ASIGetSDKVersion() -> *const i8 { b"fake\0".as_ptr() as *const i8 }
