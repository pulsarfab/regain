# Camera exercise kit validation

Tested on 2026-09-14 with capped cameras, SDK 1.41 and the installed Windows
camera driver. NINA was disconnected during kit runs. The standalone executable
was used for hardware captures. [Sanitized results and evidence hashes](camera-kit-validation.json)
record the exact host/SDK/tracer hashes and build provenance; raw calibration
and pixel bundles remain local.

| Camera | Set | Passed / attempted | Result |
| --- | --- | --- | --- |
| ASI2600MM Pro main | Quick, pixel samples, cooling controls | 21 / 21 | Complete; 25 USB sample chunks; all ten saved controls matched on restoration read-back. |
| ASI2600MM Pro main | Extended, pixel samples, cooling controls | 47 / 47 | Includes 60 seconds, bins 1–4 and processing-control variants; 52 USB sample chunks. |
| ASI220MM Mini guide | Quick, pixel samples | 18 / 19 | SDK exposure state 3 for 64 × 64; failure retained and subsequent cases completed. |
| ASI676MC | Quick, pixel samples | 20 / 21 | SDK changed the requested moved-ROI origin; recorded as a failed case rather than claiming the requested geometry. |

Every run recorded USB control payloads and completed bulk data, produced its
ZIP, and matched all saved controls on restoration read-back. File hashes were
verified against each manifest. Main/guide quick runs used clean build
`f680756`; the extended and ASI676 runs used the preceding build before the
crash-cleanup fixes. The JSON preserves those builds' original dirty-tree
provenance instead of presenting them as clean release binaries.

The cooler exercise validates SDK control transactions and read-back, not
thermal recovery. It only runs for five seconds; cooler output remained zero
in the sampled interval. SDK temperature telemetry immediately after opening
or reopening returned zero before refreshing, so those snapshots must not be
interpreted as settled temperatures. SDK initialization/close can affect state;
the kit restores its post-initialization snapshot and verifies it before close.
Configure the next imaging session normally.

## Automated checks

- Ten kit unit tests cover bounded plans, unsupported modes, duplicate model
  names, ZIP integrity, invalid/truncated protocol replies, instance-path
  omission, descriptor/device association, opt-in pixels and recording limits.
- Real simulator hosts exercise control values with automatic flags, a failed
  SDK download followed by a valid read, and a hung-download deadline.
- End-to-end collector checks inject an SDK error, a worker crash and
  cancellation. They require partial ZIPs, failure exit codes, saved-control
  restoration, and no remaining owned worker processes. Crash cleanup must
  reopen the same simulated camera in a replacement worker.
- The packaged executable exercises Frida attachment, binary frames, pixel
  files and ZIP creation with Python/toolchain directories removed from PATH.
- Existing 57 recovery, 13 NINA contract, 16 Rust and five inspection tests pass.

These checks validate the collector and known camera baselines. They do not
establish support for an unseen camera's direct protocol, illuminated-image
processing, physical-disconnect recovery, video or formats other than RAW16.
