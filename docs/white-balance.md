# Shared color-camera white balance (current source)

Regain owns one opt-in software white-balance implementation for the SDK and
Direct USB camera workers. Applications choose settings and display status; they
do not need separate AWB algorithms for each backend. This is a **Rust/worker API**
addition, not a new NINA, ASCOM, Alpaca setup-page, or AutoPierCam UI setting.
Existing clients that never configure it retain their previous behavior.

## Contract

- Supported input: a known color Bayer pattern, RAW16 little-endian, bin 1,
  flip 0. The worker rejects monochrome cameras and unsupported geometry rather
  than guessing colors. Direct currently supports ASI662MC and ASI676MC.
- Linear red and blue gains are relative to green = 1; each is finite and in
  [0.125, 8]. These are **not** the SDK's nonlinear WB_R/WB_B values.
- `off` means unity gains; `manual` uses supplied gains; `once` estimates on the
  first usable light frame then enters `locked`; `continuous` updates on each
  usable light frame; `locked` freezes the previous effective gains.
- `output: "raw"` (default) estimates/reports gains without applying them.
  `output: "corrected"` explicitly scales Bayer samples, rounds to nearest, and
  saturates at 65535. It still returns a Bayer RAW16 layout, **not RGB**. Corrected
  samples are not untouched science data; callers must consult `applied` metadata
  and must not apply the gains again.
- Dark frames never update AWB or receive color scaling, even with corrected
  output selected. `off` also never scales pixels. To preserve calibration data,
  keep output raw; raw here means no Regain WB scaling, not a bypass of the
  backend's existing factory defect correction or other acquisition processing.
- Settings changes require an idle worker (no exposure or undelivered frame).
  Status queries do not open another camera or perform discovery.
- Settings are a complete replacement, with omitted fields taking defaults.
  Lock retains prior gains; to restore saved gains to a fresh worker, configure
  manual gains first and then lock. Settings can be read back, including the
  effective estimates after AWB. A fresh connection is unmanaged until opted in.

## SDK neutrality and compatibility

For a managed SDK connection, WB_R (3) and WB_B (4) are set to manual 50 before
each exposure and read back. This is the ZWO unity setting, not a gain/50
conversion. Both controls must advertise writable ranges containing 50; otherwise
the capability is unavailable. Native WB and flip writes are rejected while
managed WB owns the connection. Existing nonzero flips must be cleared before
opting in. Turning the new mode off still keeps neutral SDK input; closing and
reopening returns to legacy control ownership.

Prior vendor WB values and auto flags are saved before the first neutralization
and restored on orderly close, with readback. A neutralization failure prevents
exposure; a restoration failure is reported, not suppressed. Forced worker
termination, USB loss, or process crashes cannot guarantee restoration. A recovered
managed worker neutralizes the SDK again before capture. Legacy/unmanaged clients
continue using their native controls, which may include vendor-default WB on raw
output. Thus opting in can deliberately change SDK raw values relative to legacy.

The SDK unity convention is documented by ZWO staff in their
[RAW16 white-balance discussion](https://bbs.zwoastro.com/d/14860-nonzero-lsbs-with-raw16-from-asi178-with-linux-driver).
This implementation does not claim calibrated colorimetry or identical raw
acquisition across SDK and Direct: only identical Regain color processing of
the same neutral Bayer input. No physical-camera WB validation was performed
for this change.

## AWB algorithm and limits

The versioned `regain-software-v1` algorithm is gray-world estimation over sampled
2x2 Bayer cells, accounting for all four Bayer patterns and ROI origin parity.
A cell is eligible only if **all four** samples are between 1024 and 58981 on the
16-bit output scale. At least 64 cells and 10% of attempted cells must qualify.
Ratios outside the gain limits are rejected. Uniform cell-index sampling is
bounded to 65,536 cells; averaging both greens avoids double-weighting them.

Red and blue estimates are green mean divided by their respective means.
Continuous mode moves 20% toward each accepted estimate; once mode applies its
first estimate immediately and locks. Rejected frames retain the last gains;
once mode remains pending. Metadata reports `updated`, `insufficient-signal`,
`gain-out-of-range`, `dark-frame`, or `not-requested` and the eligible cell count.

This is not a scene classifier: a strongly colored scene, light pollution,
narrowband imaging, or an unusual pedestal can bias gray-world AWB. A night image
with enough signal may still qualify. For stable night-sky rendering, estimate
on a suitable neutral scene and lock, or supply manual gains. Black-level
calibration, color matrices, gamma, debayering, and tone mapping are not performed.

## Worker protocol

The `open` response has an additive `whiteBalance` capability object. Existing
version-1 framing is unchanged. Query `white-balance` with null or `{}` parameters:

```json
{"version":1,"id":2,"method":"white-balance","params":null}
```

Opt into AWB without altering pixels:

```json
{"version":1,"id":3,"method":"white-balance","params":{"mode":"once","output":"raw"}}
```

Or request explicit correction with manual gains:

```json
{"version":1,"id":4,"method":"white-balance","params":{"mode":"manual","gains":{"red":1.6,"blue":1.2},"output":"corrected"}}
```

The reply includes capabilities, `managed`, and current `settings`. Each managed
download contains `whiteBalance` metadata with effective settings, `applied`,
estimation outcome, sample count, input interpretation, Bayer pattern, and ROI.
Unmanaged downloads have no new WB metadata. Retained-frame rereads still happen
inside acquisition; WB runs once after successful delivery, not on failed reads.
Low-level standalone research capture switches remain unchanged; this API is
available through the SDK and Direct **pipe workers**.

## Rust integration

```rust,ignore
use regain_core::white_balance::{Mode, Output, Settings};

// On the capture/session owner, after connect; do not open another handle.
session.set_white_balance(Settings {
    mode: Mode::Once,
    output: Output::Raw,
    ..Settings::default()
}, &token).await?;
let frame = session.capture(exposure, &token).await?;
let effective = session.snapshot().white_balance;
```

`Session` preserves settings and the last delivered AWB estimate across worker
recovery/fallback. Its status exposes `white_balance_capabilities` and
`white_balance` (camelCase in JSON). Legacy WB/flip controls cannot override
managed settings. `Worker::white_balance` is a lower-level typed helper; clients
using Worker directly own reconnect/reapply policy. The pure
`regain_core::white_balance::WhiteBalance` engine is also public for rendering a
separate copy of neutral raw pixels. Do not feed already-corrected pixels to it.

## Verification without hardware

`cargo test --workspace --locked` covers CFA/ROI phases, clipping, raw preservation,
dark/low-signal frames, AWB locking/smoothing, input validation, and session
recovery. Build the workers first. `python scripts/test-rust.py` exercises both
simulated pipe backends and compiles an inert SDK ABI fixture to check manual/auto
restoration and neutralization/restore failures. It never loads a vendor SDK or
opens USB unless a separate explicit `--sdk-fixture` test library is supplied;
that option is for a synthetic library too, not a physical camera.
