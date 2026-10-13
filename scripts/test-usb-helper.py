"""Bounded Windows USB-helper rejection checks; never prepare or reset hardware.

Requires no prepared helper for this Windows user. Elevated disposable CI runs
the same checks in a restricted token child, proving the ordinary non-admin path.
No valid prepare command, real device identity, or production test switch
is used. Successful broker lifecycle/policy cases belong to Rust fixtures.
"""
import argparse
import ctypes
from ctypes import wintypes
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid


MISSING = "USB reset helper is not prepared"
TIMEOUT_SECONDS = 4


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


@contextmanager
def unelevated_pipe_squatter():
    """Own one inert pipe; prove the client authenticates the kernel server token.

    This process is already verified non-admin. The exclusive flag prevents
    replacing or joining a real helper that appeared after the precondition.
    No protocol handler, elevation, target approval, or native device I/O exists.
    """
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.GetCurrentProcessId.restype = wintypes.DWORD
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.ProcessIdToSessionId.argtypes = [wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    kernel.CreateNamedPipeW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                       wintypes.DWORD, wintypes.DWORD, wintypes.DWORD,
                                       wintypes.DWORD, ctypes.c_void_p]
    kernel.CreateNamedPipeW.restype = wintypes.HANDLE
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                        ctypes.POINTER(wintypes.HANDLE)]
    advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                           wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    advapi.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]

    def checked(success, operation):
        if not success:
            raise ctypes.WinError(ctypes.get_last_error(), operation)

    token = wintypes.HANDLE()
    sid = wintypes.LPWSTR()
    pipe = None
    try:
        checked(advapi.OpenProcessToken(kernel.GetCurrentProcess(), 8, ctypes.byref(token)),
                "Open squatter test user token")
        required = wintypes.DWORD()
        advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(required))
        require(0 < required.value <= 65536, "Invalid squatter token size")
        user = ctypes.create_string_buffer(required.value)
        checked(advapi.GetTokenInformation(token, 1, user, len(user), ctypes.byref(required)),
                "Read squatter test user SID")
        user_sid = ctypes.cast(user, ctypes.POINTER(ctypes.c_void_p))[0]
        checked(advapi.ConvertSidToStringSidW(user_sid, ctypes.byref(sid)), "Format squatter test SID")
        session = wintypes.DWORD()
        checked(kernel.ProcessIdToSessionId(kernel.GetCurrentProcessId(), ctypes.byref(session)),
                "Read squatter test Windows session")
        path = rf"\\.\pipe\PulsarFab.Regain.UsbReset.v1.{sid.value}.{session.value}"
        # PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE; reject remote clients.
        pipe = kernel.CreateNamedPipeW(path, 3 | 0x80000, 8, 1, 4096, 4096, 0, None)
        require(pipe != ctypes.c_void_p(-1).value, "Cannot exclusively own squatter fixture pipe")
        yield
    finally:
        if pipe is not None and pipe != ctypes.c_void_p(-1).value:
            kernel.CloseHandle(pipe)
        if sid:
            kernel.LocalFree(ctypes.cast(sid, ctypes.c_void_p))
        if token:
            kernel.CloseHandle(token)


@contextmanager
def restricted_launch_desktop(token):
    """Give the child private GUI objects accessible with its restricted token.

    A hosted runner can own an administrator-only, high-integrity window
    station. Inheriting it can make User32 fail during DLL initialization,
    before Python or our non-admin assertions run. Keep the runner's existing
    objects unchanged and create a noninteractive pair for this test only.
    """
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    user = ctypes.WinDLL("user32", use_last_error=True)

    class SecurityAttributes(ctypes.Structure):
        _fields_ = [("length", wintypes.DWORD), ("descriptor", ctypes.c_void_p),
                    ("inherit", wintypes.BOOL)]

    class TokenOwner(ctypes.Structure):
        _fields_ = [("Sid", ctypes.c_void_p)]

    class TokenDefaultDacl(ctypes.Structure):
        _fields_ = [("DefaultDacl", ctypes.c_void_p)]

    kernel.GetCurrentThreadId.restype = wintypes.DWORD
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                           wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    advapi.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(wintypes.DWORD)]
    advapi.GetSecurityDescriptorDacl.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.BOOL),
                                                ctypes.POINTER(ctypes.c_void_p),
                                                ctypes.POINTER(wintypes.BOOL)]
    advapi.SetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                          ctypes.c_void_p, wintypes.DWORD]
    user.GetProcessWindowStation.restype = wintypes.HANDLE
    user.GetThreadDesktop.argtypes = [wintypes.DWORD]
    user.GetThreadDesktop.restype = wintypes.HANDLE
    user.SetProcessWindowStation.argtypes = [wintypes.HANDLE]
    user.SetThreadDesktop.argtypes = [wintypes.HANDLE]
    user.CreateWindowStationW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                         ctypes.POINTER(SecurityAttributes)]
    user.CreateWindowStationW.restype = wintypes.HANDLE
    user.CreateDesktopW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, ctypes.c_void_p,
                                    wintypes.DWORD, wintypes.DWORD,
                                    ctypes.POINTER(SecurityAttributes)]
    user.CreateDesktopW.restype = wintypes.HANDLE
    user.CloseDesktop.argtypes = [wintypes.HANDLE]
    user.CloseWindowStation.argtypes = [wintypes.HANDLE]

    def checked(success, operation):
        if not success:
            raise ctypes.WinError(ctypes.get_last_error(), operation)
        return success

    sid, descriptor = wintypes.LPWSTR(), ctypes.c_void_p()
    station = desktop = None
    original_station = checked(user.GetProcessWindowStation(), "Read test window station")
    original_desktop = checked(user.GetThreadDesktop(kernel.GetCurrentThreadId()),
                               "Read test thread desktop")
    try:
        size = wintypes.DWORD()
        advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(size))
        require(0 < size.value <= 65536, "Invalid restricted token user size")
        token_user = ctypes.create_string_buffer(size.value)
        checked(advapi.GetTokenInformation(token, 1, token_user, len(token_user),
                                           ctypes.byref(size)), "Read restricted token user")
        user_sid = ctypes.cast(token_user, ctypes.POINTER(ctypes.c_void_p))[0]
        checked(advapi.ConvertSidToStringSidW(user_sid, ctypes.byref(sid)),
                "Format restricted token user SID")
        # Only the unchanged user SID and SYSTEM receive access. Explicit medium
        # integrity prevents a high-integrity runner labeling these objects high.
        sddl = f"O:{sid.value}D:P(A;;GA;;;SY)(A;;GA;;;{sid.value})S:(ML;;NW;;;ME)"
        checked(advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl, 1, ctypes.byref(descriptor), None), "Build private test desktop security")
        # Do not inherit administrator-only defaults for child-created objects.
        # This changes only the new restricted token, never the caller's token.
        present, defaulted, acl = wintypes.BOOL(), wintypes.BOOL(), ctypes.c_void_p()
        checked(advapi.GetSecurityDescriptorDacl(descriptor, ctypes.byref(present),
                                                 ctypes.byref(acl), ctypes.byref(defaulted)),
                "Read private test default DACL")
        require(present.value and acl.value, "Missing private test DACL")
        default_dacl = TokenDefaultDacl(acl)
        owner = TokenOwner(user_sid)
        checked(advapi.SetTokenInformation(token, 6, ctypes.byref(default_dacl),
                                            ctypes.sizeof(default_dacl)),
                "Set restricted child default DACL")
        checked(advapi.SetTokenInformation(token, 4, ctypes.byref(owner), ctypes.sizeof(owner)),
                "Set restricted child owner")
        security = SecurityAttributes(ctypes.sizeof(SecurityAttributes), descriptor, False)
        station_name = "RegainUsbHelperTest-" + uuid.uuid4().hex
        # CWF_CREATE_ONLY; fail rather than opening an existing named object.
        station = checked(user.CreateWindowStationW(station_name, 1, 0x10000000,
                                                     ctypes.byref(security)),
                          "Create private test window station")
        try:
            checked(user.SetProcessWindowStation(station), "Select private test window station")
            desktop = checked(user.CreateDesktopW("ProtocolChecks", None, None, 0,
                                                    0x10000000, ctypes.byref(security)),
                              "Create private test desktop")
        finally:
            checked(user.SetProcessWindowStation(original_station), "Restore test window station")
            checked(user.SetThreadDesktop(original_desktop), "Restore test thread desktop")
        yield station_name + "\\ProtocolChecks"
    finally:
        if desktop:
            checked(user.CloseDesktop(desktop), "Close private test desktop")
        if station:
            checked(user.CloseWindowStation(station), "Close private test window station")
        if descriptor:
            kernel.LocalFree(descriptor)
        if sid:
            kernel.LocalFree(ctypes.cast(sid, ctypes.c_void_p))


def restricted_child(arguments):
    """Start this test with the caller's admin membership disabled, not runas."""
    import msvcrt

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)

    class SidAttributes(ctypes.Structure):
        _fields_ = [("Sid", ctypes.c_void_p), ("Attributes", wintypes.DWORD)]

    class StartupInfo(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("reserved", wintypes.LPWSTR),
                    ("desktop", wintypes.LPWSTR), ("title", wintypes.LPWSTR),
                    ("x", wintypes.DWORD), ("y", wintypes.DWORD),
                    ("width", wintypes.DWORD), ("height", wintypes.DWORD),
                    ("charsX", wintypes.DWORD), ("charsY", wintypes.DWORD),
                    ("fill", wintypes.DWORD), ("flags", wintypes.DWORD),
                    ("show", wintypes.WORD), ("reservedSize", wintypes.WORD),
                    ("reservedBytes", ctypes.c_void_p), ("stdin", wintypes.HANDLE),
                    ("stdout", wintypes.HANDLE), ("stderr", wintypes.HANDLE)]

    class ProcessInfo(ctypes.Structure):
        _fields_ = [("process", wintypes.HANDLE), ("thread", wintypes.HANDLE),
                    ("processId", wintypes.DWORD), ("threadId", wintypes.DWORD)]

    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                        ctypes.POINTER(wintypes.HANDLE)]
    advapi.ConvertStringSidToSidW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p)]
    advapi.GetLengthSid.argtypes = [ctypes.c_void_p]
    advapi.GetLengthSid.restype = wintypes.DWORD
    advapi.CreateRestrictedToken.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                            wintypes.DWORD, ctypes.POINTER(SidAttributes),
                                            wintypes.DWORD, ctypes.c_void_p,
                                            wintypes.DWORD, ctypes.c_void_p,
                                            ctypes.POINTER(wintypes.HANDLE)]
    advapi.SetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                          ctypes.c_void_p, wintypes.DWORD]
    advapi.CreateProcessAsUserW.argtypes = [wintypes.HANDLE, wintypes.LPCWSTR,
                                           wintypes.LPWSTR, ctypes.c_void_p,
                                           ctypes.c_void_p, wintypes.BOOL,
                                           wintypes.DWORD, ctypes.c_void_p,
                                           wintypes.LPCWSTR, ctypes.POINTER(StartupInfo),
                                           ctypes.POINTER(ProcessInfo)]

    def checked(success, operation):
        if not success:
            raise ctypes.WinError(ctypes.get_last_error(), operation)

    original, restricted = wintypes.HANDLE(), wintypes.HANDLE()
    admin_sid, medium_sid = ctypes.c_void_p(), ctypes.c_void_p()
    info = ProcessInfo()
    try:
        # TOKEN_ASSIGN_PRIMARY | DUPLICATE | QUERY | ADJUST_DEFAULT.
        checked(advapi.OpenProcessToken(kernel.GetCurrentProcess(), 0x008b,
                                         ctypes.byref(original)), "Open test token")
        checked(advapi.ConvertStringSidToSidW("S-1-5-32-544", ctypes.byref(admin_sid)),
                "Resolve Administrators SID")
        disabled = SidAttributes(admin_sid, 0)
        checked(advapi.CreateRestrictedToken(original, 1, 1, ctypes.byref(disabled),
                                              0, None, 0, None, ctypes.byref(restricted)),
                "Disable administrator membership and privileges")
        checked(advapi.ConvertStringSidToSidW("S-1-16-8192", ctypes.byref(medium_sid)),
                "Resolve medium integrity SID")
        integrity = SidAttributes(medium_sid, 0x20)  # SE_GROUP_INTEGRITY.
        checked(advapi.SetTokenInformation(restricted, 25, ctypes.byref(integrity),
                                            ctypes.sizeof(integrity) + advapi.GetLengthSid(medium_sid)),
                "Lower test child integrity")
        command = ctypes.create_unicode_buffer(subprocess.list2cmdline(
            [sys.executable, str(Path(__file__).resolve()), *arguments, "--require-unelevated"]))
        with restricted_launch_desktop(restricted) as desktop, \
                tempfile.TemporaryFile() as output, open(os.devnull, "rb") as input_file:
            for stream in (output, input_file):
                os.set_inheritable(stream.fileno(), True)
            startup = StartupInfo()
            startup.cb, startup.flags = ctypes.sizeof(startup), 0x101
            startup.desktop = desktop
            startup.stdin = msvcrt.get_osfhandle(input_file.fileno())
            startup.stdout = startup.stderr = msvcrt.get_osfhandle(output.fileno())
            try:
                checked(advapi.CreateProcessAsUserW(restricted, sys.executable, command,
                                                     None, None, True, 0x08000000,
                                                     None, None, ctypes.byref(startup),
                                                     ctypes.byref(info)), "Start restricted test child")
            finally:
                for stream in (output, input_file):
                    os.set_inheritable(stream.fileno(), False)
            waited = kernel.WaitForSingleObject(info.process, 60000)
            if waited != 0:
                # This handle belongs only to the child created above.
                checked(kernel.TerminateProcess(info.process, 124), "Stop timed-out test child")
                kernel.WaitForSingleObject(info.process, 5000)
            output.seek(0)
            print(output.read().decode("utf-8", errors="replace"), end="", flush=True)
            require(waited == 0, "Restricted USB-helper process checks exceeded 60 seconds")
            code = wintypes.DWORD()
            checked(kernel.GetExitCodeProcess(info.process, ctypes.byref(code)), "Read test exit code")
            require(code.value == 0,
                    f"Restricted USB-helper process checks failed ({code.value}, 0x{code.value:08X})")
    finally:
        for handle in (info.thread, info.process, restricted, original):
            if handle:
                kernel.CloseHandle(handle)
        for sid in (medium_sid, admin_sid):
            if sid:
                kernel.LocalFree(sid)


def process_checks(helper, evidence):
    # A unique, deliberately nonexistent USB instance prevents any physical
    # binding even if a helper appears between the absent-status checks.
    target = dict(vendor=0x03c3, product=0x620b, serial="1234567890abcdef",
                  location="USB\\VID_03C3&PID_620B\\REGAIN_TEST_" + uuid.uuid4().hex,
                  address=None, generation=1)

    def token(value):
        return json.dumps(value, separators=(",", ":")).encode().hex()

    encoded = token(target)
    rows = []
    digest = hashlib.sha256(helper.read_bytes()).hexdigest()

    def rejected(label, arguments, expected=None):
        require(hashlib.sha256(helper.read_bytes()).hexdigest() == digest,
                "USB helper executable changed during process checks")
        started = time.monotonic()
        result = subprocess.run([str(helper), "usb", *arguments], capture_output=True,
                                text=True, encoding="utf-8", errors="replace",
                                timeout=TIMEOUT_SECONDS, creationflags=subprocess.CREATE_NO_WINDOW)
        elapsed = time.monotonic() - started
        require(result.returncode != 0, f"{label}: unsafe/unexpected success")
        require(bool(result.stderr.strip()), f"{label}: missing actionable error")
        if expected is not None:
            require(expected.casefold() in result.stderr.casefold(),
                    f"{label}: expected {expected!r}, got {result.stderr!r}")
        rows.append(dict(case=label, elapsedSeconds=round(elapsed, 6),
                         returnCode=result.returncode, error=result.stderr.strip()))

    def absent_response(label, arguments, expected):
        require(hashlib.sha256(helper.read_bytes()).hexdigest() == digest,
                "USB helper executable changed during process checks")
        started = time.monotonic()
        result = subprocess.run([str(helper), "usb", *arguments], capture_output=True,
                                text=True, encoding="utf-8", errors="replace",
                                timeout=TIMEOUT_SECONDS, creationflags=subprocess.CREATE_NO_WINDOW)
        elapsed = time.monotonic() - started
        require(result.returncode == 0, f"{label}: absent status failed: {result.stderr!r}")
        require(not result.stderr.strip(), f"{label}: unexpected status error")
        value = json.loads(result.stdout)
        require(value == expected,
                f"{label}: expected a genuinely absent helper, got {value!r}")
        rows.append(dict(case=label, elapsedSeconds=round(elapsed, 6),
                         returnCode=result.returncode, status=value))

    # This read-only precondition also prevents stopping a real prepared helper.
    missing_status = dict(protocolVersion=1, running=False, elevated=False,
                          sessionScoped=True, expiresAtUnixSeconds=None, targets=[])
    absent_response("helper absent precondition", ["helper", "status"], missing_status)
    absent_response("target approval unavailable", ["helper", "status", encoded],
                    dict(missing_status, approved=False))
    absent_response("revoke absent helper", ["helper", "revoke", encoded],
                    dict(protocolVersion=1, revoked=False, targetsRemaining=0, stopped=True))
    rejected("stop absent helper", ["helper", "stop"], MISSING)
    rejected("reset missing helper", ["reset", encoded], MISSING)
    rejected("cycle missing helper", ["cycle", encoded], MISSING)
    rejected("expired one-shot authorization", ["reset", encoded, "--elevated", "0"], "expired")
    rejected("invalid one-shot deadline", ["reset", encoded, "--elevated", "invalid"])
    rejected("unknown legacy flag", ["reset", encoded, "--unexpected", "0"])
    for label, arguments in (
        ("missing USB operation", []),
        ("unknown USB operation", ["unknown", encoded]),
        ("missing reset token", ["reset"]),
        ("extra reset argument", ["reset", encoded, "extra"]),
        ("missing helper operation", ["helper"]),
        ("unknown helper operation", ["helper", "unknown"]),
        ("status extra argument", ["helper", "status", encoded, "extra"]),
        ("stop extra argument", ["helper", "stop", "extra"]),
        ("prepare missing target", ["helper", "prepare"]),
        ("revoke missing target", ["helper", "revoke"]),
        ("revoke extra argument", ["helper", "revoke", encoded, "extra"]),
    ):
        rejected(label, arguments)
    for label, invalid in (
        ("empty token", ""), ("odd token", "0"), ("nonhex token", "invalid"),
        ("invalid JSON token", "00"), ("oversized token", "00" * 2049),
        ("unknown token field", token(dict(target, unexpected=True))),
        ("unsupported vendor", token(dict(target, vendor=0))),
        ("unsupported product", token(dict(target, product=0))),
        ("zero serial", token(dict(target, serial="0000000000000000"))),
        ("invalid serial", token(dict(target, serial="not-a-camera"))),
        ("unsafe location", token(dict(target, location="../../arbitrary"))),
    ):
        rejected(label, ["reset", invalid])
    # Token validation must also protect the read-only management entry point.
    rejected("status rejects invalid token", ["helper", "status", "invalid"])
    rejected("revoke rejects invalid token", ["helper", "revoke", "invalid"])
    with unelevated_pipe_squatter():
        rejected("medium pipe squatter rejected before protocol", ["helper", "status"],
                 "server is not elevated")
    with unelevated_pipe_squatter():
        rejected("medium pipe squatter rejected before revoke", ["helper", "revoke", encoded],
                 "server is not elevated")
    absent_response("helper still absent", ["helper", "status"], missing_status)
    report = dict(passed=True, administrator=False, physicalDeviceUsed=False,
                  helperPrepared=False, helperSha256=digest,
                  timeoutSecondsPerProcess=TIMEOUT_SECONDS, cases=rows)
    if evidence is not None:
        evidence.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"USB helper: {len(rows)} bounded non-admin protocol/rejection checks passed; "
          "no preparation, UAC, or physical reset", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--require-unelevated", action="store_true", help=argparse.SUPPRESS)
    options = parser.parse_args()
    if os.name != "nt":
        print("Windows prepared USB-helper process checks skipped on this OS", flush=True)
        return
    helper = (options.bin_dir.resolve() / "regain-device.exe").resolve(strict=True)
    elevated = bool(ctypes.windll.shell32.IsUserAnAdmin())
    require(not (options.require_unelevated and elevated), "Test child retained administrator access")
    if elevated:
        restricted_child(sys.argv[1:])
    else:
        process_checks(helper, options.evidence)


if __name__ == "__main__":
    main()
