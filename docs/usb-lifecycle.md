# USB lifecycle and retained frames

The direct driver can restart a download from camera DDR without exposing again.
On the ASI2600MM Pro P25, Windows tests also recovered that memory after closing
the USB handle and after replacing the process. These extra paths are research
commands for now; normal frontend recovery still uses the existing retry policy.

## Production changes

- ASI2600/6200 capture logs distinguish initialization, exposure, sensor readout,
  retained memory, download, and validation. The server logs image-ready or failure.
- USB failures include a category, chunk number, completed byte count, expected
  frame size, deadline flag, and available Windows/NT/USB or native status.
  Terminal error replies include an optional `transportFailure` object. Existing
  error text and protocol version remain compatible.
- `transferTimeoutSeconds` bounds each complete USB read attempt. NINA and the
  Rust supervisor use the configured download timeout, default 60 seconds.
  Every retained reread receives a fresh budget; exposure time does not consume it.
  Individual bulk requests remain capped at five seconds (some models allow a
  longer first request). Cancellation must still drain before buffers are freed.
  A control request already in progress or cancellation drain can extend the
  observed failure time beyond the transfer deadline. The outer watchdog remains.
- No partial or late frame is returned as a successful image.

The CLI accepts `--transfer-timeout-seconds`. Its watchdog now accounts for the
configured transfer/replay budgets instead of assuming every capture needs only
15 seconds beyond the exposure.

## Reopen experiments

With a capped ASI2600MM Pro P25 on Windows USB3, the test sequence is:

1. Take a full 6248 × 4176 RAW16 exposure and download a prefix.
2. Close the exclusive USB handle; wait 0–5 seconds.
3. Open the same enumerated interface and verify its serial without publishing it.
4. Stop/reset/restart the retained sender without initializing the sensor.
5. Download the frame from byte zero and compare the prefix.
6. Replay the full frame again and compare all interior pixel bytes.

The matrix covers a 1 KiB prefix, 12 MiB, and the final full MiB chunk, with
0.1-, 2-, and 60-second exposures. USB traces must show exactly one exposure-start
command and no sensor writes between reopening and the next bulk read.
The first restart can time out; another restart can then succeed. Increasing
the reconnect delay alone does not establish a fix for this behavior.

The replacement-process experiment first records a complete raw interior hash
and deliberately leaves DDR intact. A fresh worker retrieves that frame using
only the retained-sender sequence. Another test terminates a reader after its
12th successful 1 MiB completion, then verifies the full hash in a fresh worker.
This tests abrupt process exit during a replay of a known frame. It does not
prove recovery from an undrainable kernel request, USB removal, or power loss.

Frame envelopes contain changing counters. Comparisons exclude only the first
and last four-byte envelopes; a counter alone is not a reliable cross-process
frame identity. Pixels are compared before defect correction or software binning.

## Run the tests

Disconnect other camera apps and leave the camera capped. On Windows, use the
inspection Python environment with Frida:

```powershell
cargo build -p zwogain-direct --release --locked
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_usb_lifecycle.py --output artifacts/usb-lifecycle-new
```

The output directory must be new. `results.json` contains sanitized evidence;
`private-*.jsonl` files contain local transport traces and must be reviewed before
sharing. The script rejects changed binaries, additional exposures, mismatched
prefixes, and mismatched full-frame hashes.

The recorded [hardware evidence](usb-lifecycle-evidence.json) includes all five
reopen cases and both process-replacement cases. A separate
`validate_transfer_deadline.py --output artifacts/NEW-deadline.json` test sets a
5 ms whole-read budget on the real camera, checks that `status` and `download`
return the same structured failure with no pixels, then requests and validates
a fresh full frame with the normal timeout.

Individual research commands:

```powershell
target/release/zwogain-direct.exe --capture-2600-p25 --gain 100 --offset 50 --reopen-after-bytes 12582912 --reopen-delay-ms 1000 --replay
target/release/zwogain-direct.exe --capture-2600-p25 --keep-retained
target/release/zwogain-direct.exe --verify-retained-2600-p25 --expected-wire-sha256 HASH
```

Use `wireInteriorSha256` from the preceding capture for `HASH`. For a cropped or
binned capture, supply its physical `rawWidth`/`rawHeight` to the verifier. It
returns verification metadata only, never an image to a frontend. Ordinary
captures still clear DDR during cleanup; `--keep-retained` is a CLI-only opt-in.

## Before production worker replacement

The supervisor needs a retained-frame record containing camera identity, exposure
generation, physical geometry, correction context, and evidence from completed
chunks. It also needs to know whether any initialization, new exposure, USB reset,
or other owner invalidated that record. If continuity cannot be established, it
must reject the retained image and apply the existing replacement-exposure limit.
The full hash used by this experiment is unavailable when the original download
never finished; production recovery needs a different continuity check.

Still open: a definitive sensor-readout-complete flag, physical USB reattachment,
endpoint stalls, short reads on real hardware, retained state after USB reset,
Linux/macOS hardware tests, and an overlapped read queue. Power loss must be treated
as loss of DDR. No arbitrary byte-offset resume command has been found.
