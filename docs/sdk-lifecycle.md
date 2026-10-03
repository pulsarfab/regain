# ASI SDK lifecycle investigation

Reference: bundled ASI Windows SDK 1.41 header and x64 DLL from ZWO's
[official SDK distribution](https://www.zwoastro.com/software/product-sdk/).
DLL SHA-256:
`0C8778C3CCE2012961B079E3C7D0D8348A8B3823939335D9E98148CB5D5DC34A`.
NINA's [native ASICamera implementation](https://github.com/isbeorn/nina/blob/0af8e12403c495d0dea76a553c54816f306c4981/NINA.Equipment/Equipment/MyCamera/ASICamera.cs)
was inspected for acquisition defaults and its camera interface; the plugin
is compiled and contract-tested against the NINA 3.2.0.9001 package.

## Documented observations

| Interface | Information available | Limit |
| --- | --- | --- |
| `ASIGetExpStatus` | 0 idle, 1 working, 2 success/ready, 3 failed | No USB transfer progress or byte offset |
| SDK return codes | Timeout 11, removed 5, closed 4, invalid ID 2, general 16, parameter failures | Generic errors do not identify the underlying USB fault |
| `ASIGetROIFormat`, `ASIGetStartPos` | Effective dimensions, bin, pixel type and origin | Does not establish whether the frame is still retained |
| `ASIGetControlValue` | Temperature, cooler power/target/enablement and acquisition settings where supported | These are separate SDK calls, also subject to hangs |
| `ASIGetSerialNumber` | Identity stable across camera index changes | Some older cameras may not expose it |
| `ASIGetCameraMode` | Current trigger mode on trigger-capable cameras | Not a transport lifecycle state |
| `ASIGetDroppedFrames` | Video capture dropped-frame count | Not a still exposure transfer cursor |
| `ASICameraCheck` | Recognizes USB VID/PID | Despite its name, not a live camera health check |

The header explicitly describes exposure failure as requiring a new exposure.
`ASIGetDataAfterExp` accepts a whole image buffer and size; there is no offset,
chunk ID, frame ID, continuation token, or public retry guarantee. A failure in
camera-to-SDK transfer may surface as `ASI_EXP_FAILED` during readiness polling,
not as a failed copy from SDK to application. Partial data cannot be returned
as a usable scientific image.

PulsarFab regain records the phase and the original numeric SDK error. After a download
error it probes exposure status before destroying the host. This distinguishes
"still ready" from failed/idle/missing camera when the SDK can still respond.
The same-frame path reissues the complete download only for state 2, with two
retries by default regardless of exposure duration. This is an attempt to
recover an available frame, not a guarantee that the SDK retains one. A failed
status probe or a stalled download instead causes host replacement.

## Discovery is an active hardware operation

Evidence shared from [AutoPierCam PR #12](https://github.com/theatrus/autopiercam/pull/12),
2026-09-26, SDK 1.41.0.0: a capture-owner process dump showed
`ASIGetCameraProperty -> ASIOpenCamera -> internal SDK code` during periodic
inventory refresh. Disassembly verified the call from the property export into
the open export. An internal parsing loop advanced by a zero-length item and did
not make progress. Discovery was querying camera index/ID 1 while capture used
ID 0. Private symbols were unavailable; large nearest-export offsets do not
identify the internal function. The source of the invalid data is not established.

The operator subsequently reported other ZWO cameras on the same system stopped
working too. This is an observed wider symptom, not proof of a particular USB
controller, kernel-driver or firmware failure. Killing a worker contains its
user-space hang; it does not guarantee recovery of other cameras or the USB stack.
No process dump, credentials, images or private log paths are published here.

Consequences for Regain and integrations:

- Never treat `ASIGetCameraProperty` as passive enumeration, even if application
  code does not explicitly call `ASIOpenCamera`. Do not periodically enumerate
  the SDK during live imaging, settling, or connected-idle intervals.
- SDK workers now reject `list` while connected, before entering the SDK. The
  library wrapper independently checks open ownership. Clients should retain
  discovery results; a refresh button must not silently probe hardware.
- This is a **per-worker guard**, not a system-wide interlock. `Runtime::list`
  creates another worker and a new connection still enumerates before opening.
  Such requests can probe cameras owned by other processes. Integrators must
  schedule SDK discovery before imaging; process isolation does not make these
  probes harmless. Cross-process discovery coordination / passive inventory and
  targeted identity resolution remain follow-up work.
- Regain already uses separate workers with parent-side command deadlines and
  termination. Keep deadlines independent of SDK code, and never reopen before
  the prior owner has exited. A successful IPC exchange or other device telemetry
  is not evidence that frames are advancing.
- The direct USB descriptor parser already rejects lengths below two or beyond
  the remaining buffer before type dispatch. Regression cases now explicitly
  cover unknown-type items with zero, one, and oversized lengths, guarding the
  same non-progressing-loop class without claiming the SDK's private parser is
  identical to Regain's descriptor parser.
- Keep destructive USB port resets/cycles explicit and device-scoped. Do not
  automatically reset an entire hub or controller to recover one pier camera.
- Preserve numeric errors, operation phases and monotonic frame-progress timing.
  Test stalled calls, shutdown and no-rescan behavior with simulation/fixtures,
  not live discovery on an imaging system.

AutoPierCam's intended migration is to Regain's **Direct USB** backend once
ASI662MC capture, identity, exposure limits and day/night soak behavior are
implemented and validated. Choosing Regain's SDK backend alone does not remove
this SDK discovery risk. ASI676MC has experimental direct support; that does not
establish support for ASI662MC or identical long-exposure limits.

## Undocumented debug exports

PE export inspection of the supplied DLL found:

- `ASIEnableDebugLog`
- `ASIGetDebugLogIsEnabled`
- `ASIGetDebugLogPath`

They are absent from the bundled public header, and official web searches did
not provide signatures. They are **not called** by this implementation. Getting
their ABI and log format from ZWO is a promising next step for diagnosing the
SDK's internal USB failure stage. No export advertising resumable/chunked still
transfer was found. Export names alone cannot establish internal capabilities.

Follow-up binary inspection and real-camera transport experiments found an
SDK-internal replay path, below the public download API. They also identified
version-specific debug argument usage without invoking those exports. See the
[transport investigation](transport-investigation.md) for the evidence,
limitations, and plan for a custom transport. In particular, the inspected
1.41 download implementation consumes the ready state after its internal
retrieval attempt, making a repeated public call unlikely to help there.

## Useful future hardware evidence

For the ASI2600/6200 series, collect the exact model, firmware/driver/SDK versions,
USB topology, exposure/ROI/bin/gain/offset/USB-limit settings, prior temperature
and cooler power, and NINA's log around the fault. Compare whether failures occur
in readiness polling or the download call, and whether post-error status remains
2. Test controlled unplug/replug and cooling recovery separately from software
host termination. Never treat the simulator's retained-frame behavior as proof
of the real camera's transfer semantics.
# Optional serial discovery

Camera workers accept `list` with `{"serials":true}` before opening capture.
The default list remains descriptor-only. Each result optionally includes
`serial`, or `discoveryError` when that device could not be identified. Direct
USB reads the identity without starting acquisition; SDK discovery opens,
initializes and closes each candidate. This is active probing, not a live
inventory API: callers must cache results and keep discovery on the camera
owner thread. Both backends reject discovery while owning a camera. Busy or
unidentified devices are not silently substituted. Empty/whitespace serial
filters are unspecified; nonempty filters are trimmed and case-normalized.

## Targeted reconnect protocol

Integrators should **not** opt into all-camera serial discovery on automatic
startup or fault retry. SDK `open` now accepts an optional `id` from cached
discovery together with `name` and, after first connection, the verified
`serial`. This calls the SDK's count API once to initialize its device table,
then opens only that ID. It verifies serial before `ASIInitCamera` and reads
properties with `ASIGetCameraPropertyByID` on the selected open handle. Missing
IDs, changed identities and property errors fail closed: no property sweep or
fallback to another camera. Legacy name-based open remains available to explicit
discovery clients. SDK internals can still enumerate USB; this is not a claim
that the vendor library makes zero OS discovery calls.

Direct USB list enumerates OS metadata once and includes an opaque `locator`.
Pass it with `open` to filter interfaces **before** opening a handle; verify
the saved serial as well. Returned descriptors retain the locator for retries.
Missing/ambiguous locators never fall back to a serial sweep. A locator describes
topology, not trusted identity. After a USB reset a changed interface may require
operator rediscovery. Do not persist or publish private interface paths.

USB target binding accepts the optional locator, and refuses to probe multiple
same-model interfaces without it. Normal capture reconnect should retain cached
inventory; failed discovery should have a separate, longer backoff.

Synthetic ABI tests assert the exact selected-ID call sequence across process
replacement and reject stale serials before initialization. Direct selector tests
prove missing, duplicate and busy targets cannot open another candidate. These
tests do not establish mixed-application hardware safety or resolve a USB-stack
deadlock already in progress.
