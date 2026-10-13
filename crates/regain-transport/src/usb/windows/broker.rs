//! Prepared, bounded elevation. The only executable launch is explicit preparation.
//! Recovery runs in this already elevated process; no late child launch can turn a
//! replaceable plugin executable into an unattended elevation trampoline.
use super::{Handle, Set, Target, cycle_camera, generation, now, os_error, validate_binding, wide};
use crate::usb::helper::{Broker, MAX_MESSAGE, OPERATION_SECONDS, PROTOCOL_VERSION, Request};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    mem::size_of,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::*,
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Console::*, IO::*, Pipes::*, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};

const MISSING: &str = "USB reset helper is not prepared; explicitly prepare this camera before observing (automatic recovery never requests elevation)";
// FILE_GENERIC_READ | FILE_WRITE_DATA, deliberately excluding bit 4:
// FILE_APPEND_DATA aliases FILE_CREATE_PIPE_INSTANCE for named pipes. OWNER_RIGHTS
// also removes the owner's implicit WRITE_DAC; otherwise a medium process could
// rewrite its own pipe ACL and manufacture a second instance.
const CLIENT_ACCESS: u32 = 0x0012_008b;
const CALL_SECONDS: u64 = 5;
// Windows SDK group attributes and mandatory label RID. Keep token inspection
// query-only: an ordinary frontend must not need permission to duplicate the
// elevated helper's token in order to authenticate it.
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x0000_0010;
const SECURITY_MANDATORY_HIGH_RID: u32 = 0x0000_3000;

#[derive(Debug)]
struct Identity {
    sid: String,
    session: u32,
    elevated: bool,
}
impl Identity {
    fn same_user(&self, other: &Self) -> bool {
        self.sid == other.sid && self.session == other.session
    }
    fn pipe(&self) -> String {
        format!(
            r"\\.\pipe\PulsarFab.Regain.UsbReset.v1.{}.{}",
            self.sid, self.session
        )
    }
}

fn token_value<T: Copy>(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<T> {
    unsafe {
        let mut value: T = std::mem::zeroed();
        let mut returned = 0;
        ensure!(
            GetTokenInformation(
                token,
                class,
                (&mut value as *mut T).cast(),
                size_of::<T>() as u32,
                &mut returned
            ) != 0
                && returned == size_of::<T>() as u32,
            "Read USB helper token identity: {}",
            os_error()
        );
        Ok(value)
    }
}
fn token_buffer(
    token: HANDLE,
    class: TOKEN_INFORMATION_CLASS,
    minimum: usize,
) -> Result<(Vec<u64>, usize)> {
    unsafe {
        let mut required = 0;
        GetTokenInformation(token, class, null_mut(), 0, &mut required);
        ensure!(
            (minimum..=65536).contains(&(required as usize)),
            "Invalid USB helper token information size"
        );
        let mut storage = vec![0u64; (required as usize).div_ceil(8)];
        let mut returned = 0;
        ensure!(
            GetTokenInformation(
                token,
                class,
                storage.as_mut_ptr().cast(),
                required,
                &mut returned
            ) != 0
                && returned == required,
            "Read USB helper token authority: {}",
            os_error()
        );
        Ok((storage, returned as usize))
    }
}
fn token_elevated_administrator(token: HANDLE) -> Result<bool> {
    // CreateRestrictedToken can retain the source token's elevation flag after
    // administrator membership is disabled and integrity is lowered. Neither
    // such a pipe server nor an approval/bootstrap caller has elevated authority.
    if token_value::<TOKEN_ELEVATION>(token, TokenElevation)?.TokenIsElevated == 0 {
        return Ok(false);
    }
    let (integrity, _) = token_buffer(
        token,
        TokenIntegrityLevel,
        size_of::<TOKEN_MANDATORY_LABEL>(),
    )?;
    unsafe {
        let label = &*integrity.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        ensure!(
            !sid.is_null() && IsValidSid(sid) != 0,
            "Invalid USB helper integrity SID"
        );
        let count = *GetSidSubAuthorityCount(sid);
        ensure!(count > 0, "Missing USB helper integrity level");
        if *GetSidSubAuthority(sid, u32::from(count) - 1) < SECURITY_MANDATORY_HIGH_RID {
            return Ok(false);
        }
        let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
        let (groups, length) = token_buffer(token, TokenGroups, size_of::<TOKEN_GROUPS>())?;
        let count = (*groups.as_ptr().cast::<TOKEN_GROUPS>()).GroupCount as usize;
        ensure!(
            count <= (length - offset) / size_of::<SID_AND_ATTRIBUTES>(),
            "Invalid USB helper token group count"
        );
        let groups = std::slice::from_raw_parts(
            groups
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SID_AND_ATTRIBUTES>(),
            count,
        );
        Ok(groups.iter().any(|group| {
            group.Attributes & SE_GROUP_ENABLED != 0
                && group.Attributes & SE_GROUP_USE_FOR_DENY_ONLY == 0
                && IsWellKnownSid(group.Sid, WinBuiltinAdministratorsSid) != 0
        }))
    }
}
fn identity(process: HANDLE) -> Result<Identity> {
    unsafe {
        let mut token = null_mut();
        ensure!(
            OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0,
            "Open USB helper process token: {}",
            os_error()
        );
        let token = Handle(token);
        let mut required = 0;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required);
        ensure!(
            (size_of::<TOKEN_USER>() as u32..=65536).contains(&required),
            "Invalid USB helper token size"
        );
        let mut storage = vec![0u64; (required as usize).div_ceil(8)];
        ensure!(
            GetTokenInformation(
                token.0,
                TokenUser,
                storage.as_mut_ptr().cast(),
                required,
                &mut required
            ) != 0,
            "Read USB helper user: {}",
            os_error()
        );
        let user = &*storage.as_ptr().cast::<TOKEN_USER>();
        let mut sid = null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "Read USB helper SID: {}",
            os_error()
        );
        let mut length = 0;
        while *sid.add(length) != 0 && length < 256 {
            length += 1;
        }
        let sid_text = String::from_utf16(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        let sid = sid_text.context("Invalid Windows user SID")?;
        let session = token_value::<u32>(token.0, TokenSessionId)?;
        let elevated = token_elevated_administrator(token.0)?;
        Ok(Identity {
            sid,
            session,
            elevated,
        })
    }
}
fn current_identity() -> Result<Identity> {
    identity(unsafe { GetCurrentProcess() })
}
fn peer(pipe: HANDLE, server: bool, owner: &Identity) -> Result<Identity> {
    unsafe {
        let mut pid = 0;
        let success = if server {
            GetNamedPipeServerProcessId(pipe, &mut pid)
        } else {
            GetNamedPipeClientProcessId(pipe, &mut pid)
        };
        ensure!(
            success != 0 && pid != 0,
            "Cannot authenticate USB helper pipe peer: {}",
            os_error()
        );
        // Keep the process handle alive through validation, preventing PID reuse.
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(
            !process.is_null(),
            "Cannot authenticate USB helper process: {}",
            os_error()
        );
        let process = Handle(process);
        let peer = identity(process.0)?;
        ensure!(
            owner.same_user(&peer),
            "USB helper belongs to another Windows user or session"
        );
        ensure!(
            !server || peer.elevated,
            "USB helper server is not elevated; refusing untrusted pipe"
        );
        Ok(peer)
    }
}

fn milliseconds(deadline: Instant) -> u32 {
    deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u32::MAX as u128 - 1) as u32
}
// Terminal completion is mandatory before releasing any OVERLAPPED or its buffer.
// A broken kernel driver that does not drain is contained by terminating this
// helper process, rather than freeing storage still referenced by kernel I/O.
unsafe fn completion(
    handle: HANDLE,
    overlap: &mut OVERLAPPED,
    deadline: Option<Instant>,
) -> Result<u32> {
    unsafe {
        // Only the idle pipe accept has no deadline. Connected clients and
        // native recovery operations always retain their bounded deadlines.
        let wait = WaitForSingleObject(overlap.hEvent, deadline.map_or(INFINITE, milliseconds));
        if wait != WAIT_OBJECT_0 {
            CancelIoEx(handle, overlap);
            if WaitForSingleObject(overlap.hEvent, 2000) != WAIT_OBJECT_0 {
                eprintln!("USB helper pipe cancellation did not drain; terminating helper");
                std::process::exit(124);
            }
            let mut ignored = 0;
            GetOverlappedResult(handle, overlap, &mut ignored, 0);
            anyhow::bail!("USB helper request deadline expired");
        }
        let mut transferred = 0;
        ensure!(
            GetOverlappedResult(handle, overlap, &mut transferred, 0) != 0,
            "USB helper pipe I/O failed: {}",
            os_error()
        );
        Ok(transferred)
    }
}
fn event() -> Result<Handle> {
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    ensure!(
        !handle.is_null(),
        "Create USB helper I/O event: {}",
        os_error()
    );
    Ok(Handle(handle))
}
fn transfer(handle: HANDLE, data: &mut [u8], write: bool, deadline: Instant) -> Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        ensure!(
            Instant::now() < deadline,
            "USB helper request deadline expired"
        );
        let event = event()?;
        let mut overlap: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlap.hEvent = event.0;
        let length = (data.len() - offset) as u32;
        let success = unsafe {
            if write {
                WriteFile(
                    handle,
                    data[offset..].as_ptr(),
                    length,
                    null_mut(),
                    &mut overlap,
                )
            } else {
                ReadFile(
                    handle,
                    data[offset..].as_mut_ptr(),
                    length,
                    null_mut(),
                    &mut overlap,
                )
            }
        };
        ensure!(
            success != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING,
            "USB helper pipe I/O failed: {}",
            os_error()
        );
        let transferred = unsafe { completion(handle, &mut overlap, Some(deadline))? } as usize;
        ensure!(
            transferred > 0 && transferred <= data.len() - offset,
            "USB helper pipe closed before completing its message"
        );
        offset += transferred;
    }
    Ok(())
}
fn send(handle: HANDLE, value: &Value, deadline: Instant) -> Result<()> {
    let mut message = serde_json::to_vec(value)?;
    ensure!(
        !message.is_empty() && message.len() <= MAX_MESSAGE,
        "USB helper response exceeds protocol limit"
    );
    transfer(
        handle,
        &mut (message.len() as u32).to_le_bytes(),
        true,
        deadline,
    )?;
    transfer(handle, &mut message, true, deadline)
}
fn receive(handle: HANDLE, deadline: Instant) -> Result<Value> {
    let mut header = [0u8; 4];
    transfer(handle, &mut header, false, deadline)?;
    let length = u32::from_le_bytes(header) as usize;
    ensure!(
        (1..=MAX_MESSAGE).contains(&length),
        "Invalid USB helper message size"
    );
    let mut message = vec![0u8; length];
    transfer(handle, &mut message, false, deadline)?;
    serde_json::from_slice(&message).context("Invalid USB helper protocol JSON")
}

fn client(request: &Request) -> Result<Value> {
    request.validate()?;
    let owner = current_identity()?;
    let path = wide(&owner.pipe());
    let deadline = Instant::now()
        + Duration::from_secs(match request {
            Request::Recover { .. } => OPERATION_SECONDS + 5,
            _ => CALL_SECONDS,
        });
    let handle = loop {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break Handle(handle);
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_PIPE_BUSY {
            ensure!(
                unsafe { WaitNamedPipeW(path.as_ptr(), 1500) } != 0,
                "USB reset helper is busy; another recovery may be running"
            );
            ensure!(
                Instant::now() < deadline,
                "USB reset helper is busy; request deadline expired"
            );
            continue;
        }
        if error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND {
            if let Request::Status { target, .. } = request {
                return Ok(absent_status(target.as_ref()));
            }
            if matches!(request, Request::Revoke { .. }) {
                return Ok(json!({"protocolVersion":PROTOCOL_VERSION,"revoked":false,
                    "targetsRemaining":0,"stopped":true}));
            }
            anyhow::bail!(MISSING);
        }
        anyhow::bail!(
            "Cannot open prepared USB reset helper (no elevation attempted): {}",
            os_error()
        );
    };
    peer(handle.0, true, &owner)?;
    send(handle.0, &serde_json::to_value(request)?, deadline)?;
    let response = receive(handle.0, deadline)?;
    // A bounded acknowledgement replaces blocking FlushFileBuffers. The server
    // disconnects only after the client has actually consumed the response.
    transfer(handle.0, &mut [1], true, deadline)?;
    ensure!(
        response["protocolVersion"] == PROTOCOL_VERSION,
        "Unsupported USB helper response version"
    );
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        if matches!(request, Request::Recover { .. }) {
            // Preserve timing and collateral evidence even though the process
            // exits nonzero. The supervisor includes bounded stderr in its log.
            eprintln!("USB helper failure diagnostics: {response}");
        }
        anyhow::bail!("{error}");
    }
    Ok(response)
}

fn absent_status(target: Option<&Target>) -> Value {
    let mut value = json!({"protocolVersion":PROTOCOL_VERSION,"running":false,"elevated":false,
        "sessionScoped":true,"expiresAtUnixSeconds":null,"targets":[]});
    if target.is_some() {
        value["approved"] = json!(false);
    }
    value
}

pub(super) fn recover(target: &Target, cycle: bool) -> Result<()> {
    let response = client(&Request::Recover {
        protocol_version: PROTOCOL_VERSION,
        target: target.clone(),
        cycle,
        deadline_unix_seconds: now() + OPERATION_SECONDS,
    })?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
fn status(target: Option<&Target>) -> Result<Value> {
    client(&Request::Status {
        protocol_version: PROTOCOL_VERSION,
        target: target.cloned(),
    })
}
fn approved_status(target: &Target) -> Result<Value> {
    let status = status(Some(target))?;
    ensure!(
        status["running"] == true && status["elevated"] == true && status["approved"] == true,
        "Prepared USB helper did not approve this camera"
    );
    Ok(status)
}

pub(super) fn helper(args: &[String]) -> Result<()> {
    let command = args
        .first()
        .context("Usage: usb helper prepare TARGET | revoke TARGET | status [TARGET] | stop")?;
    match command.as_str() {
        "prepare" => {
            ensure!(args.len() == 2, "Usage: usb helper prepare TARGET");
            let target = Target::decode(&args[1])?;
            eprintln!(
                "Note: cycling camera USB can also disconnect peripherals on the camera's internal hub; reconnect those devices after recovery."
            );
            prepare(&target)?;
        }
        "status" => {
            ensure!(args.len() <= 2, "Usage: usb helper status [TARGET]");
            let target = args
                .get(1)
                .map(|encoded| Target::decode(encoded))
                .transpose()?;
            println!("{}", status(target.as_ref())?);
        }
        "revoke" => {
            ensure!(args.len() == 2, "Usage: usb helper revoke TARGET");
            let target = Target::decode(&args[1])?;
            println!(
                "{}",
                client(&Request::Revoke {
                    protocol_version: PROTOCOL_VERSION,
                    target
                })?
            );
        }
        "stop" => {
            ensure!(args.len() == 1, "Usage: usb helper stop");
            println!(
                "{}",
                client(&Request::Stop {
                    protocol_version: PROTOCOL_VERSION
                })?
            );
        }
        // Internal bootstrap: permitted only in an elevated process with a fresh
        // explicit startup deadline and the original user's kernel token identity.
        "serve" => {
            ensure!(args.len() == 5, "Invalid USB helper bootstrap");
            let target = Target::decode(&args[1])?;
            let deadline: u64 = args[2].parse()?;
            ensure!(
                now() < deadline && deadline <= now() + 65,
                "USB helper preparation authorization expired"
            );
            let owner = current_identity()?;
            ensure!(
                owner.elevated
                    && owner.sid == args[3]
                    && owner.session == args[4].parse::<u32>()?,
                "Prepare USB helper using elevation for the same Windows user and session"
            );
            detach_standard_handles()?;
            // A second explicit preparation can add one binding to the
            // independently running helper for this Windows user/session.
            if let Ok(response) = client(&Request::Approve {
                protocol_version: PROTOCOL_VERSION,
                target: target.clone(),
            }) {
                println!("{response}");
                return Ok(());
            }
            validate_binding(&target)?;
            serve(Broker::new(target)?, &owner)?;
        }
        _ => anyhow::bail!("Unknown USB helper command: {command}"),
    }
    Ok(())
}

fn detach_standard_handles() -> Result<()> {
    // ShellExecuteEx does not expose a standard-handle inheritance policy.
    // A session-long broker must not keep a short-lived prepare client's
    // redirected pipes open. Detach only after the elevated bootstrap is
    // authenticated; ordinary prepare/status/revoke clients retain their JSON.
    let identifiers = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    let original = identifiers.map(|identifier| unsafe { GetStdHandle(identifier) });
    let mut replacement = Vec::with_capacity(identifiers.len());
    for access in [GENERIC_READ, GENERIC_WRITE, GENERIC_WRITE] {
        let handle = unsafe {
            CreateFileW(
                wide("NUL").as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(), // Non-inheritable; kept valid for this process's lifetime.
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "Detach USB helper standard handles: {}",
            os_error()
        );
        replacement.push(Handle(handle));
    }
    for (index, identifier) in identifiers.iter().enumerate() {
        if unsafe { SetStdHandle(*identifier, replacement[index].0) } == 0 {
            let error = os_error();
            // Preserve the original reporting path if setup fails partway.
            for restore in 0..index {
                unsafe { SetStdHandle(identifiers[restore], original[restore]) };
            }
            anyhow::bail!("Detach USB helper standard handles: {error}");
        }
    }
    for (index, handle) in original.iter().enumerate() {
        if (*handle as isize) > 0 // Excludes missing, invalid, and pseudo handles.
            && !original[..index].contains(handle)
            && !replacement.iter().any(|replacement| replacement.0 == *handle)
        {
            // Some launchers assign the same pipe handle to stdout and stderr.
            // Close it exactly once. A stale numeric standard-handle value may
            // have been reused for a new NUL handle; never close that replacement.
            unsafe { CloseHandle(*handle) };
        }
    }
    // Windows closes these three non-inheritable NUL handles at process exit.
    // Keep them valid through Rust main's final error/diagnostic reporting too.
    for handle in replacement {
        std::mem::forget(handle);
    }
    Ok(())
}

fn prepare(target: &Target) -> Result<()> {
    ensure!(
        target.generation.is_some_and(|generation| generation != 0),
        "USB helper preparation requires a verified arrival generation"
    );
    validate_binding(target)?;
    if let Ok(status) = approved_status(target) {
        println!("{status}");
        return Ok(());
    }
    // Read-only binding check before displaying UAC or starting a persistent process.
    let owner = current_identity()?;
    let exe = std::env::current_exe()?;
    let exe = wide(exe.to_str().context("Invalid helper executable path")?);
    let verb = wide(if owner.elevated { "open" } else { "runas" });
    let params = wide(&format!(
        "usb helper serve {} {} {} {}",
        target.encode()?,
        now() + 60,
        owner.sid,
        owner.session
    ));
    let process = unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        ensure!(
            ShellExecuteExW(&mut info) != 0,
            "USB helper preparation elevation denied or unavailable: {}",
            os_error()
        );
        ensure!(
            !info.hProcess.is_null(),
            "Prepared USB helper did not start"
        );
        Handle(info.hProcess)
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(status) = approved_status(target) {
            println!("{status}");
            return Ok(());
        }
        let mut code = 0;
        ensure!(
            unsafe { GetExitCodeProcess(process.0, &mut code) } != 0,
            "Read USB helper startup status: {}",
            os_error()
        );
        ensure!(
            code == STILL_ACTIVE as u32 || code == 0,
            "USB helper preparation failed (exit {code}); verify the same Windows user is elevated and camera remains connected"
        );
        ensure!(
            Instant::now() < deadline,
            "USB helper preparation timed out; no camera was reset"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn descriptor(owner: &Identity) -> String {
    format!(
        "D:P(A;;GA;;;BA)(A;;RC;;;OW)(A;;0x{CLIENT_ACCESS:08x};;;{})S:(ML;;NW;;;ME)",
        owner.sid
    )
}
fn pipe(owner: &Identity) -> Result<Handle> {
    unsafe {
        let mut descriptor_ptr = null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&descriptor(owner)).as_ptr(),
                1,
                &mut descriptor_ptr,
                null_mut()
            ) != 0,
            "Create USB helper pipe security: {}",
            os_error()
        );
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_ptr,
            bInheritHandle: 0,
        };
        let handle = CreateNamedPipeW(
            wide(&owner.pipe()).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            MAX_MESSAGE as u32,
            MAX_MESSAGE as u32,
            0,
            &security,
        );
        LocalFree(descriptor_ptr);
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "Cannot exclusively create USB helper pipe; another helper or untrusted pipe already owns this user/session: {}",
            os_error()
        );
        Ok(Handle(handle))
    }
}
fn connect(handle: HANDLE) -> Result<()> {
    let event = event()?;
    let mut overlap: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlap.hEvent = event.0;
    if unsafe { ConnectNamedPipe(handle, &mut overlap) } != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    ensure!(
        error == ERROR_IO_PENDING,
        "Accept USB helper client: {}",
        os_error()
    );
    unsafe {
        completion(handle, &mut overlap, None)?;
    }
    Ok(())
}

fn serve(mut broker: Broker, owner: &Identity) -> Result<()> {
    let pipe = pipe(owner)?;
    // The detached elevated process belongs to the Windows logon session,
    // rather than a camera worker, supervisor, or NINA instance. An idle helper
    // stays ready until explicit Stop or Windows terminates it at logoff/reboot.
    loop {
        connect(pipe.0)?;
        let result = (|| -> Result<(Value, bool)> {
            let client = peer(pipe.0, false, owner)?;
            let value = receive(pipe.0, Instant::now() + Duration::from_secs(CALL_SECONDS))?;
            let request: Request =
                serde_json::from_value(value).context("Invalid USB helper request")?;
            request.validate()?;
            match request {
                Request::Status { target, .. } => Ok((broker.status(target.as_ref())?, false)),
                Request::Approve { target, .. } => {
                    // Check elevation before even performing a live device probe.
                    ensure!(
                        client.elevated,
                        "Adding a USB camera requires explicit elevated preparation"
                    );
                    validate_binding(&target)?;
                    broker.approve(target.clone(), client.elevated)?;
                    Ok((broker.status(Some(&target))?, false))
                }
                Request::Revoke { target, .. } => {
                    let response = broker.revoke(&target)?;
                    let stopped = response["stopped"] == true;
                    Ok((response, stopped))
                }
                Request::Recover {
                    target,
                    cycle,
                    deadline_unix_seconds,
                    ..
                } => {
                    broker.authorize(&target, deadline_unix_seconds, now())?;
                    let response = recovery(&target, cycle, deadline_unix_seconds)?;
                    Ok((response, false))
                }
                Request::Stop { .. } => Ok((
                    json!({"protocolVersion":PROTOCOL_VERSION,"stopped":true}),
                    true,
                )),
            }
        })();
        let (response, stop) = match result {
            Ok(result) => result,
            Err(error) => (
                json!({"protocolVersion":PROTOCOL_VERSION,"error":format!("{error:#}")}),
                false,
            ),
        };
        // Bound writes even if the client vanished during a native reset.
        let response_deadline = Instant::now() + Duration::from_secs(CALL_SECONDS);
        if send(pipe.0, &response, response_deadline).is_ok() {
            let _ = transfer(pipe.0, &mut [0], false, response_deadline);
        }
        unsafe {
            DisconnectNamedPipe(pipe.0);
        }
        if stop {
            break;
        }
    }
    Ok(())
}

struct Watchdog(Option<std::sync::mpsc::Sender<()>>);
impl Watchdog {
    fn start(seconds: u64) -> Self {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if receive.recv_timeout(Duration::from_secs(seconds)).is_err() {
                eprintln!(
                    "USB native recovery exceeded its hard deadline; terminating prepared helper, reconnect and prepare again"
                );
                std::process::exit(124);
            }
        });
        Self(Some(send))
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}

fn inventory() -> Result<BTreeMap<String, Option<u64>>> {
    unsafe {
        let set = Set(SetupDiGetClassDevsW(
            null(),
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        ));
        ensure!(
            set.0 != -1,
            "Enumerate USB reset diagnostics: {}",
            os_error()
        );
        let mut result = BTreeMap::new();
        for index in 0..4096 {
            let mut device: SP_DEVINFO_DATA = std::mem::zeroed();
            device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set.0, index, &mut device) == 0 {
                ensure!(
                    GetLastError() == ERROR_NO_MORE_ITEMS,
                    "Enumerate USB reset diagnostics: {}",
                    os_error()
                );
                return Ok(result);
            }
            let mut id = vec![0u16; 4096];
            if SetupDiGetDeviceInstanceIdW(
                set.0,
                &device,
                id.as_mut_ptr(),
                id.len() as u32,
                null_mut(),
            ) == 0
            {
                continue;
            }
            let length = id.iter().position(|&c| c == 0).unwrap_or(id.len());
            let id = String::from_utf16_lossy(&id[..length]).to_ascii_uppercase();
            if id.starts_with("USB\\") {
                let arrival = generation(&id).ok();
                result.insert(id, arrival);
            }
        }
        anyhow::bail!("USB reset diagnostics exceeded device enumeration limit")
    }
}
fn changes(before: &BTreeMap<String, Option<u64>>, after: &BTreeMap<String, Option<u64>>) -> Value {
    let missing: Vec<_> = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .collect();
    let appeared: Vec<_> = after
        .keys()
        .filter(|key| !before.contains_key(*key))
        .collect();
    let reenumerated: Vec<_> = before
        .iter()
        .filter_map(|(key, arrival)| match (arrival, after.get(key)) {
            (Some(old), Some(Some(new))) if old != new => Some(key),
            _ => None,
        })
        .collect();
    let counts = [missing.len(), appeared.len(), reenumerated.len()];
    let mut limit = 16;
    loop {
        let value = json!({"missingUsbInstances":missing.iter().take(limit).collect::<Vec<_>>(),
            "appearedUsbInstances":appeared.iter().take(limit).collect::<Vec<_>>(),
            "reenumeratedUsbInstances":reenumerated.iter().take(limit).collect::<Vec<_>>(),
            "missingCount":counts[0],"appearedCount":counts[1],"reenumeratedCount":counts[2],
            "truncated":counts.iter().any(|count|*count>limit),
            "scope":"present USB device instances on this host; brief disconnects may not be observed"});
        if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= MAX_MESSAGE / 2) {
            return value;
        }
        limit /= 2;
    }
}
fn recovery(target: &Target, cycle: bool, deadline: u64) -> Result<Value> {
    let start = Instant::now();
    let native_deadline = start + Duration::from_secs(deadline.saturating_sub(now()));
    let _watchdog = Watchdog::start(deadline.saturating_sub(now()).min(OPERATION_SECONDS + 5) + 2);
    validate_binding(target)?;
    let before = inventory();
    // Both Windows commands use USB_HUB_CYCLE_PORT for this single verified
    // downstream port. Internal camera hubs may still disrupt their peripherals.
    let result = cycle_camera(target, deadline, native_deadline);
    let mut returned_generation = None;
    if result.is_ok() {
        let return_deadline =
            Instant::now() + Duration::from_secs(10.min(deadline.saturating_sub(now())));
        while Instant::now() < return_deadline {
            if let Ok(arrival) = generation(&target.location)
                && Some(arrival) != target.generation
            {
                returned_generation = Some(arrival);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let after = inventory();
    let collateral = match (before, after) {
        (Ok(before), Ok(after)) => changes(&before, &after),
        (before, after) => {
            json!({"unavailable":true,"beforeError":before.err().map(|error|format!("{error:#}")),"afterError":after.err().map(|error|format!("{error:#}"))})
        }
    };
    let deadline_expired = now() >= deadline;
    let mut response = json!({"protocolVersion":PROTOCOL_VERSION,"operation":if cycle {"cycle"} else {"reset"},
        "nativeOperation":"USB_HUB_CYCLE_PORT","elapsedMs":start.elapsed().as_millis(),
        "target":target,"returnedGeneration":returned_generation,"nativeOperationAcknowledged":result.is_ok(),
        "deadlineExpired":deadline_expired,"peripheralNote":"Camera USB cycling can also disconnect peripherals on its internal hub", "collateral":collateral});
    if let Err(error) = result {
        response["error"] = json!(format!("{error:#}"));
    } else if deadline_expired {
        response["error"] = json!(
            "USB reset request deadline expired after native completion; inspect returned camera identity before reconnecting"
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_standard_handles_release_parent_pipes_while_child_remains_alive() {
        use std::{
            io::Read,
            process::{Child, Command, Stdio},
            sync::mpsc,
        };
        const CHILD_DIRECTORY: &str = "REGAIN_USB_STDIO_FIXTURE_DIRECTORY";
        const CHILD_VARIANT: &str = "REGAIN_USB_STDIO_FIXTURE_VARIANT";
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            let directory = std::path::PathBuf::from(directory);
            let variant = std::env::var(CHILD_VARIANT).unwrap();
            if variant == "duplicate" {
                let old = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
                let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
                assert_ne!(unsafe { SetStdHandle(STD_ERROR_HANDLE, output) }, 0);
                if !old.is_null() && old != INVALID_HANDLE_VALUE && old != output {
                    unsafe { CloseHandle(old) };
                }
            }
            if matches!(variant.as_str(), "invalid" | "stale" | "pseudo") {
                let old = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
                if !old.is_null() && old != INVALID_HANDLE_VALUE {
                    unsafe { CloseHandle(old) };
                }
                let replacement = match variant.as_str() {
                    "invalid" => INVALID_HANDLE_VALUE,
                    "pseudo" => unsafe { GetCurrentThread() },
                    _ => old,
                };
                assert_ne!(unsafe { SetStdHandle(STD_INPUT_HANDLE, replacement) }, 0);
            }
            detach_standard_handles().unwrap();
            for identifier in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                let handle = unsafe { GetStdHandle(identifier) };
                assert!(!handle.is_null() && handle != INVALID_HANDLE_VALUE);
                let mut flags = 0;
                assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
                assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
            }
            std::fs::write(directory.join("ready"), "ready").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !directory.join("release").exists() {
                assert!(
                    Instant::now() < deadline,
                    "Stdio fixture parent disappeared"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            return;
        }
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for variant in ["normal", "duplicate", "invalid", "stale", "pseudo"] {
            let directory = tempfile::tempdir().unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "usb::windows::broker::tests::detached_standard_handles_release_parent_pipes_while_child_remains_alive",
                    "--nocapture",
                ])
                .env(CHILD_DIRECTORY, directory.path())
                .env(CHILD_VARIANT, variant)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
            let mut child = OwnedChild(command.spawn().unwrap());
            let output = child.0.stdout.take().unwrap();
            let errors = child.0.stderr.take().unwrap();
            let (send, receive) = mpsc::channel();
            for mut pipe in [Box::new(output) as Box<dyn Read + Send>, Box::new(errors)] {
                let send = send.clone();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let result = pipe.read_to_end(&mut bytes);
                    let _ = send.send(result);
                });
            }
            drop(send);
            for _ in 0..2 {
                receive
                    .recv_timeout(Duration::from_secs(5))
                    .expect("Detached child retained a parent stdout/stderr pipe")
                    .unwrap();
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Fixture exited before EOF verification"
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while !directory.path().join("ready").exists() {
                assert!(
                    Instant::now() < deadline,
                    "Fixture did not confirm detached handles"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::write(directory.path().join("release"), "release").unwrap();
            while child.0.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "Stdio fixture failed to exit");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(child.0.wait().unwrap().success());
        }
    }
    #[test]
    fn absent_helper_has_explicit_unready_status_without_approval() {
        let status = absent_status(None);
        assert_eq!(status["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(status["running"], false);
        assert_eq!(status["elevated"], false);
        assert_eq!(status["sessionScoped"], true);
        assert!(status["expiresAtUnixSeconds"].is_null());
        assert_eq!(status["targets"], json!([]));
        assert!(status.get("approved").is_none());
        let target = Target {
            vendor: 0x03c3,
            product: 0x620b,
            serial: "1234567890abcdef".into(),
            location: "USB\\VID_03C3&PID_620B\\REGAIN_TEST".into(),
            address: None,
            generation: Some(1),
        };
        assert_eq!(absent_status(Some(&target))["approved"], false);
    }
    #[test]
    fn pipe_security_does_not_grant_client_instance_creation_or_owner_dacl_rewrite() {
        assert_eq!(CLIENT_ACCESS & FILE_CREATE_PIPE_INSTANCE, 0);
        assert_eq!(CLIENT_ACCESS & WRITE_DAC, 0);
        let owner = Identity {
            sid: "S-1-5-21-1-2-3-1001".into(),
            session: 7,
            elevated: true,
        };
        assert!(descriptor(&owner).contains("(A;;RC;;;OW)"));
        assert!(descriptor(&owner).contains("S:(ML;;NW;;;ME)"));
        assert!(owner.pipe().ends_with("S-1-5-21-1-2-3-1001.7"));
        let mut other = Identity {
            sid: owner.sid.clone(),
            session: 8,
            elevated: true,
        };
        assert!(!owner.same_user(&other));
        other.session = 7;
        assert!(owner.same_user(&other));
        other.sid.push('1');
        assert!(!owner.same_user(&other));
    }
    #[test]
    fn collateral_diagnostics_detect_missing_added_and_returned_instances() {
        let before = BTreeMap::from([
            ("camera".into(), Some(1)),
            ("peripheral".into(), Some(2)),
            ("gone".into(), None),
        ]);
        let after = BTreeMap::from([
            ("camera".into(), Some(3)),
            ("peripheral".into(), Some(2)),
            ("new".into(), Some(4)),
        ]);
        let changes = changes(&before, &after);
        assert_eq!(changes["missingUsbInstances"], json!(["gone"]));
        assert_eq!(changes["appearedUsbInstances"], json!(["new"]));
        assert_eq!(changes["reenumeratedUsbInstances"], json!(["camera"]));
    }
    #[test]
    fn caller_identity_is_kernel_derived_and_security_descriptor_is_valid() {
        let owner = current_identity().unwrap();
        assert!(owner.sid.starts_with("S-1-"));
        let mut security = null_mut();
        unsafe {
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(&descriptor(&owner)).as_ptr(),
                    1,
                    &mut security,
                    null_mut()
                ),
                0
            );
            LocalFree(security);
        }
    }
    #[test]
    fn restricted_tokens_cannot_authorize_helper_servers_or_approvals() {
        unsafe {
            let mut original = null_mut();
            assert_ne!(
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT,
                    &mut original
                ),
                0,
                "Open token for restriction fixture: {}",
                os_error()
            );
            let original = Handle(original);
            let mut administrator = [0u64; 9];
            let mut size = SECURITY_MAX_SID_SIZE;
            assert_ne!(
                CreateWellKnownSid(
                    WinBuiltinAdministratorsSid,
                    null_mut(),
                    administrator.as_mut_ptr().cast(),
                    &mut size
                ),
                0
            );
            let disabled = SID_AND_ATTRIBUTES {
                Sid: administrator.as_mut_ptr().cast(),
                Attributes: 0,
            };
            // Independently verify the genuine source token, including the
            // elevated hosted CI runner. CheckTokenMembership needs an
            // impersonation token; only this fixture duplicates its own token.
            let mut membership_token = null_mut();
            assert_ne!(
                DuplicateToken(original.0, SecurityIdentification, &mut membership_token),
                0,
                "Duplicate fixture token for membership check: {}",
                os_error()
            );
            let membership_token = Handle(membership_token);
            let mut administrator_enabled = 0;
            assert_ne!(
                CheckTokenMembership(membership_token.0, disabled.Sid, &mut administrator_enabled),
                0,
                "Check source administrator membership: {}",
                os_error()
            );
            let mut integrity = [0u64; 16];
            let mut returned = 0;
            assert_ne!(
                GetTokenInformation(
                    original.0,
                    TokenIntegrityLevel,
                    integrity.as_mut_ptr().cast(),
                    std::mem::size_of_val(&integrity) as u32,
                    &mut returned
                ),
                0,
                "Read source integrity independently: {}",
                os_error()
            );
            assert!(
                (size_of::<TOKEN_MANDATORY_LABEL>()..=std::mem::size_of_val(&integrity))
                    .contains(&(returned as usize))
            );
            let sid = (*integrity.as_ptr().cast::<TOKEN_MANDATORY_LABEL>())
                .Label
                .Sid;
            assert_ne!(IsValidSid(sid), 0);
            let count = *GetSidSubAuthorityCount(sid);
            assert!(count > 0);
            let high_integrity =
                *GetSidSubAuthority(sid, u32::from(count) - 1) >= SECURITY_MANDATORY_HIGH_RID;
            let genuinely_elevated = administrator_enabled != 0
                && high_integrity
                && token_value::<TOKEN_ELEVATION>(original.0, TokenElevation)
                    .unwrap()
                    .TokenIsElevated
                    != 0;
            assert_eq!(
                token_elevated_administrator(original.0).unwrap(),
                genuinely_elevated,
                "Genuine elevated administrator must be accepted; ordinary callers must be rejected"
            );
            for disable_administrator in [true, false] {
                let mut restricted = null_mut();
                assert_ne!(
                    CreateRestrictedToken(
                        original.0,
                        DISABLE_MAX_PRIVILEGE,
                        u32::from(disable_administrator),
                        if disable_administrator {
                            &disabled
                        } else {
                            null()
                        },
                        0,
                        null(),
                        0,
                        null(),
                        &mut restricted,
                    ),
                    0,
                    "Create token restriction fixture: {}",
                    os_error()
                );
                let restricted = Handle(restricted);
                if !disable_administrator {
                    // Preserve group membership and independently prove that a
                    // medium-integrity copy cannot masquerade as the helper.
                    let mut medium = [0u64; 9];
                    let mut size = SECURITY_MAX_SID_SIZE;
                    assert_ne!(
                        CreateWellKnownSid(
                            WinMediumLabelSid,
                            null_mut(),
                            medium.as_mut_ptr().cast(),
                            &mut size
                        ),
                        0
                    );
                    let label = TOKEN_MANDATORY_LABEL {
                        Label: SID_AND_ATTRIBUTES {
                            Sid: medium.as_mut_ptr().cast(),
                            Attributes: 0x20, // SE_GROUP_INTEGRITY.
                        },
                    };
                    assert_ne!(
                        SetTokenInformation(
                            restricted.0,
                            TokenIntegrityLevel,
                            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
                            size_of::<TOKEN_MANDATORY_LABEL>() as u32 + size,
                        ),
                        0,
                        "Lower fixture integrity: {}",
                        os_error()
                    );
                }
                assert!(!token_elevated_administrator(restricted.0).unwrap());
            }
        }
    }
}
