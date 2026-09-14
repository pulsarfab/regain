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

ZWOgain records the phase and the original numeric SDK error. After a download
error it probes exposure status before destroying the host. This distinguishes
"still ready" from failed/idle/missing camera when the SDK can still respond.
The optional same-frame experiment reissues the complete download only for
state 2. It is disabled by default pending real fault validation. A failed
status probe or a stalled download instead causes host replacement.

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
