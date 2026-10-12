//! Observe only the vendor DLL's public Windows USB imports. There are no SDK
//! offsets, executable detours, external injectors, or borrowed image slices.
use libloading::Library;

#[cfg(not(windows))]
pub(super) fn install(_: &Library) {
    crate::asi::diagnostics::queued_details(
        "info",
        "sdk.usb.instrumentation",
        "SDK USB instrumentation unavailable on this platform",
        serde_json::json!({"available":false,"reason":"Windows SDK import instrumentation only"}),
    );
}

#[cfg(windows)]
pub(super) use windows::install;

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::asi::diagnostics::{UsbTrace, queued_details};
    use anyhow::{Context, Result, ensure};
    use serde_json::{Value, json};
    use std::{
        collections::HashMap,
        ffi::c_void,
        mem::size_of,
        panic::{AssertUnwindSafe, catch_unwind},
        ptr::null_mut,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Instant,
    };
    use windows_sys::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, GetLastError, HANDLE,
                SetLastError, WAIT_IO_COMPLETION, WAIT_TIMEOUT,
            },
            System::{
                Diagnostics::Debug::ReadProcessMemory,
                IO::{
                    CancelIoEx, DeviceIoControl, GetOverlappedResult, GetOverlappedResultEx,
                    OVERLAPPED,
                },
                LibraryLoader::{
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
                    GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
                },
                Memory::{PAGE_READWRITE, VirtualProtect},
                Threading::GetCurrentProcess,
            },
        },
        core::BOOL,
    };

    const CONTROL: u32 = 0x220020;
    const BULK: u32 = 0x22004b;
    const HEADER: usize = 38;
    const MAX_PENDING: usize = 4096;
    const OBSERVED: &[&str] = &[
        "DeviceIoControl",
        "GetOverlappedResult",
        "GetOverlappedResultEx",
        "CancelIoEx",
        "CloseHandle",
    ];

    /// Reading via the OS avoids constructing references to SDK/driver-owned
    /// memory, handles inaccessible metadata gracefully, and bounds each copy.
    fn copy_metadata(address: usize, length: usize) -> Option<Vec<u8>> {
        if address == 0 || length > 4096 || address.checked_add(length).is_none() {
            return None;
        }
        let mut bytes = vec![0; length];
        let mut copied = 0;
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                address as *const c_void,
                bytes.as_mut_ptr().cast(),
                length,
                &mut copied,
            )
        };
        (ok != 0 && copied == length).then_some(bytes)
    }
    fn returned_bytes(address: *mut u32) -> Option<u32> {
        let bytes = copy_metadata(address as usize, 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
    }
    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    struct LastError(u32);
    impl Drop for LastError {
        fn drop(&mut self) {
            unsafe {
                SetLastError(self.0);
            }
        }
    }
    /// Diagnostic failure cannot unwind through a Win32 ABI or change the
    /// caller's last-error state. The original Win32 call is always executed.
    fn observe<T>(f: impl FnOnce() -> T) -> Option<T> {
        let _last_error = LastError(unsafe { GetLastError() });
        catch_unwind(AssertUnwindSafe(f)).ok()
    }

    struct Pending {
        trace: UsbTrace,
        terminal: AtomicBool,
    }
    type Key = (usize, usize);
    fn pending() -> &'static Mutex<HashMap<Key, Arc<Pending>>> {
        static PENDING: OnceLock<Mutex<HashMap<Key, Arc<Pending>>>> = OnceLock::new();
        PENDING.get_or_init(|| Mutex::new(HashMap::new()))
    }
    fn find(key: Key) -> Option<Arc<Pending>> {
        pending()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
    }
    fn remove(key: Key, expected: &Arc<Pending>) {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        if map
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(entry, expected))
        {
            map.remove(&key);
        }
    }
    fn track(key: Key, transfer: &Arc<Pending>) {
        let (replaced, full) = {
            let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
            if map.len() >= MAX_PENDING && !map.contains_key(&key) {
                (None, true)
            } else {
                (map.insert(key, transfer.clone()), false)
            }
        };
        if let Some(prior) = replaced {
            prior.trace.event(
                "tracking_lost",
                json!({"reason":"SDK reused an uncompleted OVERLAPPED"}),
            );
        }
        if full {
            transfer.trace.event(
                "tracking_lost",
                json!({"reason":"bounded SDK transfer tracker full","limit":MAX_PENDING}),
            );
        }
    }
    fn terminal(transfer: &Pending, success: BOOL, error: u32, actual: Option<u32>, elapsed: u128) {
        // A fast completion can be harvested by a second SDK thread before
        // DeviceIoControl returns. Report it exactly once without retaining or
        // reading any SDK-owned buffer after submission.
        if transfer.terminal.swap(true, Ordering::AcqRel) {
            return;
        }
        transfer.trace.event(
            "complete",
            json!({"success":success != 0,
            "win32Error":if success == 0 {Some(error)} else {None},"lastError":error,
            "ioctlReturnedBytes":actual,"callElapsedUs":elapsed}),
        );
    }

    fn metadata(ioctl: u32, input: usize, input_len: u32, output_len: u32) -> Value {
        let mut fields = json!({"transport":"sdk-windows-iat","function":"DeviceIoControl",
            "ioctl":format!("0x{ioctl:08x}"),"inputBytes":input_len,"outputBytes":output_len});
        // SINGLE_TRANSFER's public Cypress driver ABI: copy only the fixed
        // setup/endpoint/length header BEFORE calling the SDK's original import.
        // The SDK owns all buffers; no pointer survives this observation.
        if matches!(ioctl, CONTROL | BULK)
            && input_len as usize >= HEADER
            && let Some(header) = copy_metadata(input, HEADER)
        {
            fields["endpoint"] = json!(format!("0x{:02x}", header[13]));
            let length = u32_at(&header, 34);
            fields["headerBufferLength"] = json!(length);
            fields["requestedBytes"] = json!(if ioctl == BULK && length == 0 {
                output_len
            } else {
                length
            });
            fields["direction"] = json!(if header[13] & 0x80 != 0 { "in" } else { "out" });
            if ioctl == CONTROL {
                fields["requestType"] = json!(format!("0x{:02x}", header[0]));
                fields["request"] = json!(format!("0x{:02x}", header[1]));
                fields["wValue"] = json!(format!("0x{:04x}", u16_at(&header, 2)));
                fields["wIndex"] = json!(format!("0x{:04x}", u16_at(&header, 4)));
                fields["direction"] = json!(if header[0] & 0x80 != 0 { "in" } else { "out" });
                fields["requestedBytes"] = json!(u16_at(&header, 6));
            }
        }
        fields
    }

    unsafe extern "system" fn device_io_control(
        handle: HANDLE,
        ioctl: u32,
        input: *const c_void,
        input_len: u32,
        output: *mut c_void,
        output_len: u32,
        returned: *mut u32,
        overlap: *mut OVERLAPPED,
    ) -> BOOL {
        let incoming = unsafe { GetLastError() };
        let key = (handle as usize, overlap as usize);
        let transfer = observe(|| {
            let mut fields = metadata(ioctl, input as usize, input_len, output_len);
            fields["handle"] = json!(format!("0x{:x}", key.0));
            fields["overlapped"] = json!(format!("0x{:x}", key.1));
            let transfer = Arc::new(Pending {
                trace: UsbTrace::start("sdk.DeviceIoControl", fields),
                terminal: AtomicBool::new(false),
            });
            // Install before submission: another SDK thread can harvest a fast
            // completion before this submitting thread returns from the API.
            if !overlap.is_null() {
                track(key, &transfer);
            }
            transfer
        });
        let started = Instant::now();
        unsafe {
            SetLastError(incoming);
        }
        let success = unsafe {
            DeviceIoControl(
                handle, ioctl, input, input_len, output, output_len, returned, overlap,
            )
        };
        let error = unsafe { GetLastError() };
        let elapsed = started.elapsed().as_micros();
        observe(|| {
            if let Some(transfer) = transfer {
                if success == 0 && error == ERROR_IO_PENDING {
                    transfer.trace.event(
                        "submitted",
                        json!({"success":false,"win32Error":if success == 0 {Some(error)} else {None},"lastError":error,"callElapsedUs":elapsed}),
                    );
                } else {
                    if !overlap.is_null() {
                        remove(key, &transfer);
                    }
                    terminal(
                        &transfer,
                        success,
                        error,
                        if success != 0 {
                            returned_bytes(returned)
                        } else {
                            None
                        },
                        elapsed,
                    );
                }
            }
        });
        unsafe {
            SetLastError(error);
        }
        success
    }

    fn completion_observation(
        handle: HANDLE,
        overlap: *const OVERLAPPED,
        function: &str,
        fields: Value,
    ) -> (UsbTrace, Option<Arc<Pending>>) {
        let mut fields = fields;
        fields["transport"] = json!("sdk-windows-iat");
        fields["function"] = json!(function);
        fields["handle"] = json!(format!("0x{:x}", handle as usize));
        fields["overlapped"] = json!(format!("0x{:x}", overlap as usize));
        (
            UsbTrace::start(function, fields),
            find((handle as usize, overlap as usize)),
        )
    }
    fn completed(
        observation: Option<(UsbTrace, Option<Arc<Pending>>)>,
        key: Key,
        success: BOOL,
        error: u32,
        returned: *mut u32,
        elapsed: u128,
    ) {
        if let Some((call, transfer)) = observation {
            let actual = if success != 0 {
                returned_bytes(returned)
            } else {
                None
            };
            let terminal_result = success != 0
                || !matches!(
                    error,
                    ERROR_IO_INCOMPLETE | WAIT_TIMEOUT | WAIT_IO_COMPLETION | ERROR_IO_PENDING
                );
            call.event("complete",json!({"success":success != 0,"win32Error":if success == 0 {Some(error)} else {None},"lastError":error,"ioctlReturnedBytes":actual,"callElapsedUs":elapsed,"terminal":terminal_result,"trackedTransfer":transfer.is_some()}));
            if terminal_result && let Some(transfer) = transfer {
                remove(key, &transfer);
                terminal(&transfer, success, error, actual, elapsed);
            }
        }
    }
    unsafe extern "system" fn get_overlapped_result(
        handle: HANDLE,
        overlap: *const OVERLAPPED,
        returned: *mut u32,
        wait: BOOL,
    ) -> BOOL {
        let incoming = unsafe { GetLastError() };
        let observation = observe(|| {
            completion_observation(
                handle,
                overlap,
                "sdk.GetOverlappedResult",
                json!({"wait":wait != 0}),
            )
        });
        let started = Instant::now();
        unsafe {
            SetLastError(incoming);
        }
        let success = unsafe { GetOverlappedResult(handle, overlap, returned, wait) };
        let error = unsafe { GetLastError() };
        let elapsed = started.elapsed().as_micros();
        observe(|| {
            completed(
                observation,
                (handle as usize, overlap as usize),
                success,
                error,
                returned,
                elapsed,
            )
        });
        unsafe {
            SetLastError(error);
        }
        success
    }
    unsafe extern "system" fn get_overlapped_result_ex(
        handle: HANDLE,
        overlap: *const OVERLAPPED,
        returned: *mut u32,
        timeout: u32,
        alertable: BOOL,
    ) -> BOOL {
        let incoming = unsafe { GetLastError() };
        let observation = observe(|| {
            completion_observation(
                handle,
                overlap,
                "sdk.GetOverlappedResultEx",
                json!({"timeoutMs":timeout,"alertable":alertable != 0}),
            )
        });
        let started = Instant::now();
        unsafe {
            SetLastError(incoming);
        }
        let success =
            unsafe { GetOverlappedResultEx(handle, overlap, returned, timeout, alertable) };
        let error = unsafe { GetLastError() };
        let elapsed = started.elapsed().as_micros();
        observe(|| {
            completed(
                observation,
                (handle as usize, overlap as usize),
                success,
                error,
                returned,
                elapsed,
            )
        });
        unsafe {
            SetLastError(error);
        }
        success
    }
    unsafe extern "system" fn cancel_io_ex(handle: HANDLE, overlap: *const OVERLAPPED) -> BOOL {
        let incoming = unsafe { GetLastError() };
        let observation = observe(|| {
            completion_observation(
                handle,
                overlap,
                "sdk.CancelIoEx",
                json!({"allRequests":overlap.is_null()}),
            )
        });
        let started = Instant::now();
        unsafe {
            SetLastError(incoming);
        }
        let success = unsafe { CancelIoEx(handle, overlap) };
        let error = unsafe { GetLastError() };
        let elapsed = started.elapsed().as_micros();
        observe(|| {
            if let Some((trace, transfer)) = observation {
                trace.event(
                    "complete",
                    json!({"success":success != 0,"win32Error":if success == 0 {Some(error)} else {None},"lastError":error,"callElapsedUs":elapsed}),
                );
                if let Some(transfer) = transfer {
                    transfer
                        .trace
                        .event("cancel", json!({"success":success != 0,"win32Error":if success == 0 {Some(error)} else {None},"lastError":error}));
                }
                // Cancellation requests never make an OVERLAPPED terminal.
            }
        });
        unsafe {
            SetLastError(error);
        }
        success
    }
    unsafe extern "system" fn close_handle(handle: HANDLE) -> BOOL {
        let incoming = unsafe { GetLastError() };
        let trace = observe(|| {
            UsbTrace::start(
                "sdk.CloseHandle",
                json!({"transport":"sdk-windows-iat","handle":format!("0x{:x}",handle as usize)}),
            )
        });
        let started = Instant::now();
        unsafe {
            SetLastError(incoming);
        }
        let success = unsafe { CloseHandle(handle) };
        let error = unsafe { GetLastError() };
        let elapsed = started.elapsed().as_micros();
        observe(|| {
            if let Some(trace) = trace {
                trace.event(
                    "complete",
                    json!({"success":success != 0,"win32Error":if success == 0 {Some(error)} else {None},"lastError":error,"callElapsedUs":elapsed}),
                );
            }
            if success != 0 {
                let removed = {
                    let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
                    let keys: Vec<_> = map
                        .keys()
                        .filter(|key| key.0 == handle as usize)
                        .copied()
                        .collect();
                    keys.into_iter()
                        .filter_map(|key| map.remove(&key))
                        .collect::<Vec<_>>()
                };
                for transfer in removed {
                    transfer.trace.event("tracking_lost",json!({"reason":"SDK closed handle before an observed terminal completion"}));
                }
            }
        });
        unsafe {
            SetLastError(error);
        }
        success
    }

    struct Import {
        name: String,
        slot: usize,
    }
    /// Every PE read is bounded by SizeOfImage after a small fixed header read;
    /// no unbounded C strings or SDK-specific code offsets are accepted.
    fn imports(read: impl Fn(usize, usize) -> Option<Vec<u8>>) -> Result<Vec<Import>> {
        let dos = read(0, 64).context("unreadable DOS header")?;
        ensure!(&dos[..2] == b"MZ", "invalid DOS header");
        let pe = u32_at(&dos, 60) as usize;
        ensure!((64..=1024 * 1024).contains(&pe), "invalid PE header offset");
        let file = read(pe, 24).context("unreadable PE header")?;
        ensure!(&file[..4] == b"PE\0\0", "invalid PE signature");
        let opt_len = usize::from(u16_at(&file, 20));
        ensure!(
            (120..=4096).contains(&opt_len),
            "invalid optional header size"
        );
        let opt = read(pe + 24, opt_len).context("unreadable optional header")?;
        let width = match u16_at(&opt, 0) {
            0x20b => 8,
            0x10b => 4,
            _ => anyhow::bail!("unsupported PE format"),
        };
        ensure!(width == size_of::<usize>(), "SDK pointer width mismatch");
        let directory = if width == 8 { 112 } else { 96 };
        ensure!(
            opt.len() >= directory + 16 && u32_at(&opt, directory - 4) >= 2,
            "missing import directory"
        );
        let image = u32_at(&opt, 56) as usize;
        ensure!(
            (pe + 24 + opt_len..=256 * 1024 * 1024).contains(&image),
            "invalid image size"
        );
        let bounded = |offset: usize, length: usize| -> Option<Vec<u8>> {
            (offset.checked_add(length)? <= image)
                .then(|| read(offset, length))
                .flatten()
        };
        let name = |offset: usize| -> Option<String> {
            let mut bytes = Vec::new();
            for index in 0..128 {
                let byte = *bounded(offset.checked_add(index)?, 1)?.first()?;
                if byte == 0 {
                    return String::from_utf8(bytes).ok();
                }
                bytes.push(byte);
            }
            None
        };
        let (start, length) = (
            u32_at(&opt, directory + 8) as usize,
            u32_at(&opt, directory + 12) as usize,
        );
        ensure!(
            start != 0 && (20..=1024 * 1024).contains(&length),
            "invalid import directory bounds"
        );
        let end = start
            .checked_add(length)
            .context("import directory overflow")?;
        ensure!(end <= image, "import directory exceeds image");
        let mut result = Vec::new();
        for descriptor in (start..end).step_by(20).take(256) {
            let entry = bounded(descriptor, 20).context("unreadable import descriptor")?;
            if entry.iter().all(|byte| *byte == 0) {
                return Ok(result);
            }
            let lookup = u32_at(&entry, 0) as usize;
            let iat = u32_at(&entry, 16) as usize;
            ensure!(lookup != 0 && iat != 0, "import names unavailable");
            let dll = name(u32_at(&entry, 12) as usize)
                .context("invalid import DLL name")?
                .to_ascii_lowercase();
            // API-set DLL names resolve to these same public Win32 functions.
            if dll != "kernel32.dll"
                && dll != "kernelbase.dll"
                && !dll.starts_with("api-ms-win-core-")
            {
                continue;
            }
            let mut terminated = false;
            for index in 0..4096_usize {
                let thunk =
                    bounded(lookup + index * width, width).context("unreadable import lookup")?;
                let address = if width == 8 {
                    u64::from_le_bytes(thunk.try_into().unwrap())
                } else {
                    u64::from(u32_at(&thunk, 0))
                };
                if address == 0 {
                    terminated = true;
                    break;
                }
                if address & (1 << (width * 8 - 1)) != 0 {
                    continue;
                }
                ensure!(address <= u64::from(u32::MAX), "invalid import name RVA");
                let function =
                    name(address as usize + 2).context("invalid import function name")?;
                if OBSERVED.contains(&function.as_str()) {
                    let slot = iat + index * width;
                    ensure!(
                        slot.is_multiple_of(width) && bounded(slot, width).is_some(),
                        "invalid import slot"
                    );
                    result.push(Import {
                        name: function,
                        slot,
                    });
                }
            }
            ensure!(terminated, "unterminated import lookup");
        }
        anyhow::bail!("unterminated import directory")
    }
    fn wrapper(name: &str) -> usize {
        match name {
            "DeviceIoControl" => device_io_control as *const () as usize,
            "GetOverlappedResult" => get_overlapped_result as *const () as usize,
            "GetOverlappedResultEx" => get_overlapped_result_ex as *const () as usize,
            "CancelIoEx" => cancel_io_ex as *const () as usize,
            "CloseHandle" => close_handle as *const () as usize,
            _ => unreachable!(),
        }
    }
    fn install_inner(lib: &Library) -> Result<Value> {
        static INSTALL: Mutex<()> = Mutex::new(());
        let _installation = INSTALL.lock().unwrap_or_else(|error| error.into_inner());
        // Resolve an exported address solely to find/pin its owning loaded DLL;
        // no vendor API is executed until all import slots have been examined.
        let exported =
            unsafe { lib.get::<unsafe extern "C" fn() -> *const i8>(b"ASIGetSDKVersion\0")? };
        let address = *exported as *const ();
        let mut module = null_mut();
        ensure!(
            unsafe {
                GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                    address.cast(),
                    &mut module,
                )
            } != 0,
            "pin SDK DLL failed"
        );
        let base = module as usize;
        let slots = imports(|offset, length| {
            base.checked_add(offset)
                .and_then(|address| copy_metadata(address, length))
        })?;
        let not_imported: Vec<_> = OBSERVED
            .iter()
            .filter(|name| !slots.iter().any(|slot| slot.name == **name))
            .copied()
            .collect();
        let kernel: Vec<_> = "kernel32.dll\0".encode_utf16().collect();
        let kernel = unsafe { GetModuleHandleW(kernel.as_ptr()) };
        ensure!(!kernel.is_null(), "kernel32 is not loaded");
        let mut patched = Vec::new();
        let mut skipped = Vec::new();
        for import in slots {
            let mut name = import.name.as_bytes().to_vec();
            name.push(0);
            let original = unsafe { GetProcAddress(kernel, name.as_ptr()) }
                .context("resolve original Windows import")? as *const ()
                as usize;
            let pointer = (base + import.slot) as *const AtomicUsize;
            // The DLL is pinned, the parser validated the aligned pointer-sized
            // slot, and only that slot's page is temporarily made writable.
            let current = unsafe { &*pointer }.load(Ordering::Acquire);
            if current == wrapper(&import.name) {
                patched.push(import.name);
                continue;
            }
            if current != original {
                skipped.push(json!({"function":import.name,"reason":"import already redirected or resolved to an unexpected address"}));
                continue;
            }
            let mut protection = 0;
            if unsafe {
                VirtualProtect(
                    pointer.cast(),
                    size_of::<usize>(),
                    PAGE_READWRITE,
                    &mut protection,
                )
            } == 0
            {
                skipped.push(json!({"function":import.name,"reason":"VirtualProtect failed","win32Error":unsafe {GetLastError()}}));
                continue;
            }
            let swapped = unsafe { &*pointer }
                .compare_exchange(
                    original,
                    wrapper(&import.name),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok();
            let mut ignored = 0;
            let restored = unsafe {
                VirtualProtect(pointer.cast(), size_of::<usize>(), protection, &mut ignored)
            } != 0;
            if swapped {
                patched.push(import.name.clone());
            }
            if !swapped || !restored {
                skipped.push(json!({"function":import.name,"reason":if !swapped {"import changed during installation"} else {"page protection restoration failed"}}));
            }
        }
        let available = patched.iter().any(|name| name == "DeviceIoControl")
            && patched
                .iter()
                .any(|name| name == "GetOverlappedResult" || name == "GetOverlappedResultEx");
        Ok(
            json!({"available":available,"transport":"sdk-windows-iat","patchedImports":patched,"skippedImports":skipped,"notImported":not_imported,
            "scope":"SDK DLL's resolved Win32 import slots only; helper DLLs and dynamically resolved functions are outside this boundary",
            "modulePinnedUntilProcessExit":true,"pendingTrackingLimit":MAX_PENDING,"bulkPayloadLogged":false,"outputMetadata":"unavailable: SDK owns asynchronous buffer lifetimes","ioctlReturnedBytesIsUsbPayloadCount":false}),
        )
    }
    pub(crate) fn install(lib: &Library) {
        let result = observe(|| install_inner(lib));
        match result {
            Some(Ok(fields)) => queued_details(
                if fields["available"] == true {
                    "info"
                } else {
                    "warning"
                },
                "sdk.usb.instrumentation",
                "SDK Windows import instrumentation coverage",
                fields,
            ),
            Some(Err(error)) => queued_details(
                "warning",
                "sdk.usb.instrumentation",
                "SDK USB instrumentation unavailable; SDK calls continue normally",
                json!({"available":false,"reason":format!("{error:#}")}),
            ),
            None => queued_details(
                "warning",
                "sdk.usb.instrumentation",
                "SDK USB instrumentation failed; SDK calls continue normally",
                json!({"available":false,"reason":"diagnostic observer panic"}),
            ),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn observer_preserves_last_error_on_success_and_panic() {
            unsafe {
                SetLastError(12345);
            }
            assert_eq!(
                observe(|| {
                    unsafe {
                        SetLastError(7);
                    }
                    3
                }),
                Some(3)
            );
            assert_eq!(unsafe { GetLastError() }, 12345);
            assert!(
                observe(|| {
                    unsafe {
                        SetLastError(8);
                    }
                    panic!("observer failure")
                })
                .is_none()
            );
            assert_eq!(unsafe { GetLastError() }, 12345);
        }
        #[test]
        fn metadata_reads_only_presubmit_setup_and_never_output_or_payload() {
            let mut buffer = [0_u8; HEADER + 8];
            buffer[0] = 0xc0;
            buffer[1] = 0xbc;
            buffer[6..8].copy_from_slice(&2_u16.to_le_bytes());
            buffer[13] = 0x81;
            buffer[30..34].copy_from_slice(&(HEADER as u32).to_le_bytes());
            buffer[34..38].copy_from_slice(&8_u32.to_le_bytes());
            let fields = metadata(
                CONTROL,
                buffer.as_ptr() as usize,
                buffer.len() as u32,
                buffer.len() as u32,
            );
            assert_eq!(fields["request"], "0xbc");
            assert_eq!(fields["requestedBytes"], 2);
            buffer[1] = 0xc3;
            let fields = metadata(
                CONTROL,
                buffer.as_ptr() as usize,
                buffer.len() as u32,
                buffer.len() as u32,
            );
            assert!(fields.get("controlValue").is_none());
            let fields = metadata(BULK, buffer.as_ptr() as usize, HEADER as u32, 122_344_064);
            assert_eq!(fields["endpoint"], "0x81");
            assert!(fields.get("payload").is_none());
        }
        #[test]
        fn pe_parser_finds_only_expected_aligned_windows_imports_and_rejects_invalid_bounds() {
            let mut image = vec![0_u8; 4096];
            image[..2].copy_from_slice(b"MZ");
            image[60..64].copy_from_slice(&64_u32.to_le_bytes());
            image[64..68].copy_from_slice(b"PE\0\0");
            image[84..86].copy_from_slice(&240_u16.to_le_bytes());
            let opt = 88;
            let width = size_of::<usize>();
            let dir = if width == 8 { 112 } else { 96 };
            image[opt..opt + 2]
                .copy_from_slice(&(if width == 8 { 0x20b_u16 } else { 0x10b_u16 }).to_le_bytes());
            image[opt + 56..opt + 60].copy_from_slice(&4096_u32.to_le_bytes());
            image[opt + dir - 4..opt + dir].copy_from_slice(&16_u32.to_le_bytes());
            image[opt + dir + 8..opt + dir + 12].copy_from_slice(&512_u32.to_le_bytes());
            image[opt + dir + 12..opt + dir + 16].copy_from_slice(&40_u32.to_le_bytes());
            image[512..516].copy_from_slice(&768_u32.to_le_bytes());
            image[524..528].copy_from_slice(&640_u32.to_le_bytes());
            image[528..532].copy_from_slice(&800_u32.to_le_bytes());
            image[640..653].copy_from_slice(b"KERNEL32.dll\0");
            image[768..768 + width].copy_from_slice(&1024_usize.to_le_bytes());
            image[1026..1042].copy_from_slice(b"DeviceIoControl\0");
            let read = |offset: usize, length: usize| {
                image
                    .get(offset..offset.checked_add(length)?)
                    .map(|bytes| bytes.to_vec())
            };
            let slots = imports(read).unwrap();
            assert_eq!(slots.len(), 1);
            assert_eq!(slots[0].name, "DeviceIoControl");
            assert_eq!(slots[0].slot, 800);
            image[opt + dir + 12..opt + dir + 16].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(
                imports(|offset: usize, length: usize| image
                    .get(offset..offset.checked_add(length)?)
                    .map(|bytes| bytes.to_vec()))
                .is_err()
            );
        }
    }
}
