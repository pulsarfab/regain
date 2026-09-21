# Factory correction without the ASI SDK

The ASI676MC direct reader now performs the active SDK 1.41 RAW16/bin-1
processing sequence: validate transport envelope, replace its four pixels from
the same Bayer colors two rows inward, then correct factory-mapped defects.
The implementation was derived from local SDK disassembly and our own USB
observations. It does not incorporate third-party driver code or calibration
payloads. Images and factory calibration remain in memory during validation.

## Calibration format and access

Vendor OUT `BE`, value 0, selects calibration access. Vendor IN `C3` reads at
address `0x40000`, with `wValue=0` and `wIndex=address >> 8`. Reads use 2048-byte
blocks and round the last read to 256 bytes. OUT `BE`, value 1, restores access
mode even when parsing fails. These operations do not write EEPROM contents.

The eight-byte header is ASCII `ASID` followed by a big-endian total length,
bounded at `0x30000`. Following two-byte records describe a packed defect mask:

- `00 00` advances the packed-byte base by 256.
- Otherwise swap the first byte's nibbles to obtain an offset within the block;
  store the second byte at that packed-byte position.
- Expand bits least-significant first to obtain the full sensor map, then crop
  to the requested ROI and enumerate defects in increasing pixel order.

The attached ASI676MC has 12,616 mapped defects across 3552 × 3552 pixels.
Its 512 × 256 origin ROI has 158. Both independently decoded index lists match
the SDK's lists exactly. No factory payload or pixel samples are committed.

## Correction behavior

Interior defects use the integer mean of available same-color cardinal
neighbors, two pixels away. Later defects are excluded; earlier defects have
already been corrected and may contribute. Interior results are masked with
`fff0` for this 12-bit sensor. Other pixels retain their original low bits.
The SDK uses separate copy rules at frame edges; the implementation preserves
those rules rather than replacing them with a different interpolation filter.
Correction is in-place and ordered, which matters for adjacent defects.

The observed SDK dead-pixel list is empty and its row/column maps are null.
Whole defective rows/columns are rejected by this prototype. Other calibration
classes are not implemented or advertised. This is verified parity for the
attached ASI676MC's active processing path, not every SDK mode or camera model.

## Reproduction and evidence

Build `cargo build -p regain-device`, disconnect the camera in NINA, then run:

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --camera-name 'ZWO ASI676MC' --width 3552 --height 3552 --seconds 1 --gain 180 --offset 10 --compare-wire --trace-processing --validate-direct-processing --output artifacts/inspection/correction-new.jsonl
```

The version-pinned, read-only hooks collect the SDK's wire input, calibration,
and output in memory. A separate Rust process receives the input over a binary
pipe and applies our decoder and correction without loading the DLL. The
validator compares every returned byte and the complete defect index list;
any mismatch fails the command. Trace files contain statistics and hashes,
not the binary side-channel payloads. Raw traces are ignored by Git.

The `--process-frame` harness accepts a little-endian u32 JSON length (maximum
4096), JSON fields `width`, `height`, `x`, `y`, `calibrationBytes`, then the ASID
blob and one RAW16 wire frame. It returns u32 JSON length, metadata and corrected
pixels. It neither opens hardware nor loads the SDK.

See [sanitized validation evidence](factory-correction-evidence.json). A 64 × 64
SDK test at origin (2,2) was rejected by the SDK's ROI round-trip check before
capture; the aligned (16,16) case passed. No parity claim is made for that
rejected origin. The separate direct capture matrix also checks correction
metadata, complete frame hashes, retained-frame replay and interrupted reads.
