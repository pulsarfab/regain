use super::Target;
use anyhow::{Context, Result, ensure};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::GUID,
};

const HUB: GUID = GUID::from_u128(0xf18a0e88_c30c_11d0_8815_00a0c906bed8);
struct Set(HDEVINFO);
impl Drop for Set {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn os_error() -> std::io::Error {
    std::io::Error::last_os_error()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn generation(location: &str) -> Result<u64> {
    use windows_sys::Win32::Devices::Properties::{
        DEVPKEY_Device_LastArrivalDate, DEVPROP_TYPE_FILETIME,
    };
    unsafe {
        let mut instance = wide(location);
        let mut node = 0;
        ensure!(
            CM_Locate_DevNodeW(&mut node, instance.as_mut_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                == CR_SUCCESS,
            "Bound USB camera is missing"
        );
        let mut kind = 0;
        let mut bytes = 8;
        let mut value = 0u64;
        ensure!(
            CM_Get_DevNode_PropertyW(
                node,
                &DEVPKEY_Device_LastArrivalDate,
                &mut kind,
                (&mut value as *mut u64).cast(),
                &mut bytes,
                0
            ) == CR_SUCCESS
                && kind == DEVPROP_TYPE_FILETIME
                && bytes == 8,
            "Cannot verify USB camera arrival generation"
        );
        Ok(value)
    }
}

pub fn run(target: &Target, cycle: bool, args: &[String]) -> Result<()> {
    if args.len() == 4 {
        ensure!(args[2] == "--elevated", "Unknown helper flag");
        let deadline: u64 = args[3].parse()?;
        ensure!(
            now() <= deadline && deadline <= now() + 65,
            "USB reset authorization expired"
        );
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(70));
        std::process::exit(124);
    });
    if unsafe { IsUserAnAdmin() } == 0 {
        ensure!(
            args.len() == 2,
            "USB port cycling requires administrator access"
        );
        // The payload is strictly hexadecimal. No shell parsing or caller-supplied executable.
        let exe = wide(
            std::env::current_exe()?
                .to_str()
                .context("Invalid helper executable path")?,
        );
        let verb = wide("runas");
        let params = wide(&format!(
            "usb {} {} --elevated {}",
            if cycle { "cycle" } else { "reset" },
            target.encode()?,
            now() + 60
        ));
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
            info.lpVerb = verb.as_ptr();
            info.lpFile = exe.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            ensure!(
                ShellExecuteExW(&mut info) != 0,
                "USB reset elevation denied or unavailable: {}",
                os_error()
            );
            ensure!(
                !info.hProcess.is_null(),
                "Elevated USB helper did not start"
            );
            let process = Handle(info.hProcess);
            ensure!(
                WaitForSingleObject(process.0, 65000) == WAIT_OBJECT_0,
                "Elevated USB helper timed out"
            );
            let mut code = 0;
            ensure!(
                GetExitCodeProcess(process.0, &mut code) != 0 && code == 0,
                "Elevated USB helper failed (exit {code}); camera remains disconnected"
            );
        }
        return Ok(());
    }
    cycle_camera(target)
}

fn cycle_camera(target: &Target) -> Result<()> {
    ensure!(
        target.generation == Some(generation(&target.location)?),
        "USB camera has reenumerated since binding; reconnect before recovery"
    );
    let expected = format!(
        "USB\\VID_{:04X}&PID_{:04X}\\",
        target.vendor, target.product
    );
    ensure!(
        target.location.to_ascii_uppercase().starts_with(&expected)
            && target.location.split('\\').count() == 3,
        "Target must be one physical camera device instance"
    );
    unsafe {
        let set = Set(SetupDiCreateDeviceInfoList(null(), null_mut()));
        ensure!(set.0 != -1, "Create device list: {}", os_error());
        let mut camera: SP_DEVINFO_DATA = std::mem::zeroed();
        camera.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        ensure!(
            SetupDiOpenDeviceInfoW(
                set.0,
                wide(&target.location).as_ptr(),
                null_mut(),
                0,
                &mut camera
            ) != 0,
            "Bound USB camera is missing: {}",
            os_error()
        );
        let mut parent = 0;
        ensure!(
            CM_Get_Parent(&mut parent, camera.DevInst, 0) == CR_SUCCESS,
            "Camera has no parent hub"
        );
        let mut port = 0u32;
        let mut bytes = 4u32;
        ensure!(
            CM_Get_DevNode_Registry_PropertyW(
                camera.DevInst,
                CM_DRP_ADDRESS,
                null_mut(),
                (&mut port as *mut u32).cast(),
                &mut bytes,
                0
            ) == CR_SUCCESS
                && bytes == 4
                && (1..=255).contains(&port),
            "Camera has no valid downstream USB port"
        );
        let hubs = Set(SetupDiGetClassDevsW(
            &HUB,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ));
        ensure!(hubs.0 != -1, "Enumerate USB hubs: {}", os_error());
        for i in 0..256 {
            let mut interface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(hubs.0, null(), &HUB, i, &mut interface) == 0 {
                ensure!(
                    GetLastError() == ERROR_NO_MORE_ITEMS,
                    "Enumerate USB hub interface: {}",
                    os_error()
                );
                break;
            }
            let mut required = 0;
            SetupDiGetDeviceInterfaceDetailW(
                hubs.0,
                &interface,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
            ensure!((8..=65536).contains(&required), "Invalid USB hub path size");
            let mut storage = vec![0u64; (required as usize).div_ceil(8)];
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut dev: SP_DEVINFO_DATA = std::mem::zeroed();
            dev.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            ensure!(
                SetupDiGetDeviceInterfaceDetailW(
                    hubs.0,
                    &interface,
                    detail,
                    required,
                    null_mut(),
                    &mut dev
                ) != 0,
                "Read USB hub path: {}",
                os_error()
            );
            if dev.DevInst != parent {
                continue;
            }
            let handle = CreateFileW(
                (*detail).DevicePath.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            ensure!(
                handle != INVALID_HANDLE_VALUE,
                "Open camera parent hub (administrator required): {}",
                os_error()
            );
            let handle = Handle(handle);
            // USB_NODE_CONNECTION_INFORMATION_EX: port index + descriptor + state.
            // Verify VID/PID at the target port immediately before cycling it.
            let mut connection = [0u8; 64];
            connection[..4].copy_from_slice(&port.to_le_bytes());
            let mut returned = 0;
            ensure!(
                DeviceIoControl(
                    handle.0,
                    0x220448,
                    connection.as_ptr().cast(),
                    64,
                    connection.as_mut_ptr().cast(),
                    64,
                    &mut returned,
                    null_mut()
                ) != 0,
                "Query camera port: {}",
                os_error()
            );
            ensure!(
                returned >= 35
                    && u16::from_le_bytes([connection[12], connection[13]]) == target.vendor
                    && u16::from_le_bytes([connection[14], connection[15]]) == target.product
                    && connection[24] == 0
                    && u32::from_le_bytes(connection[31..35].try_into().unwrap()) == 1,
                "USB port identity changed, disconnected, or target is a hub"
            );
            let mut params = [port, 0u32];
            ensure!(
                DeviceIoControl(
                    handle.0,
                    0x220444,
                    params.as_ptr().cast(),
                    8,
                    params.as_mut_ptr().cast(),
                    8,
                    &mut returned,
                    null_mut()
                ) != 0
                    && params[1] == 0,
                "Cycle camera USB port: {}",
                os_error()
            );
            return Ok(());
        }
    }
    anyhow::bail!("Could not resolve the camera's immediate parent USB hub")
}
