//! Validate complete frame boundaries before interpreting a single image byte.
use anyhow::{Result, ensure};
const START: [u8; 8] = [0x55, 0x55, 0x44, 0x44, 0xee, 0x11, 0xdd, 0x22];
const END: [u8; 8] = [0xee, 0x11, 0xdd, 0x22, 0x55, 0x55, 0x44, 0x44];
const PREFIX: usize = 4 * 1024 * 1024;

pub fn locate(
    wire: &[u8],
    candidate: &mut Option<usize>,
    frame_bytes: usize,
) -> Result<Option<usize>> {
    locate_markers(wire, candidate, frame_bytes, START, END)
}

/// RAW8 uses the same delimiter for the end of one frame and start of the next.
/// Unlike RAW16, the caller must retain this footer as the next frame's header.
pub fn locate_raw8(
    wire: &[u8],
    candidate: &mut Option<usize>,
    frame_bytes: usize,
) -> Result<Option<usize>> {
    const MARKER: [u8; 8] = [0xee, 0x11, 0xdd, 0x22, 0xee, 0x11, 0xdd, 0x22];
    locate_markers(wire, candidate, frame_bytes, MARKER, MARKER)
}

fn locate_markers(
    wire: &[u8],
    candidate: &mut Option<usize>,
    frame_bytes: usize,
    start_marker: [u8; 8],
    end_marker: [u8; 8],
) -> Result<Option<usize>> {
    if candidate.is_none() {
        *candidate = wire[..wire.len().min(PREFIX)]
            .windows(8)
            .position(|w| w == start_marker)
            .map(|i| i + 8);
        ensure!(
            candidate.is_some() || wire.len() < PREFIX,
            "QHY start marker missing in bounded prefix"
        );
    }
    while let Some(at) = *candidate {
        ensure!(at <= PREFIX, "Invalid QHY frame prefix");
        let end = at
            .checked_add(frame_bytes)
            .and_then(|v| v.checked_add(8))
            .ok_or_else(|| anyhow::anyhow!("Frame boundary overflow"))?;
        if wire.len() < end {
            return Ok(None);
        }
        if wire[end - 8..end] == end_marker {
            return Ok(Some(at));
        }
        // SDK traces contain marker-like bytes inside a transport preamble. Only a
        // candidate with a footer at the exact frame length can delimit pixels.
        // Search stays inside the prefix, so a corrupt frame cannot silently
        // advance to a different exposure later in the stream.
        *candidate = wire
            .get(at..wire.len().min(PREFIX))
            .and_then(|p| p.windows(8).position(|w| w == start_marker))
            .map(|i| at + i + 8);
        crate::diagnostics::record(
            serde_json::json!({"operation":"Discard transport preamble marker","candidate":at,"nextCandidate":candidate}),
        );
        ensure!(
            candidate.is_some(),
            "QHY frame boundary mismatch; no valid preamble candidate"
        );
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw8_full_sensor_frames_share_delimiter_and_reject_corruption() {
        let marker = [0xee, 0x11, 0xdd, 0x22, 0xee, 0x11, 0xdd, 0x22];
        let size = 9600 * 6456;
        let mut wire = marker.to_vec();
        wire.resize(46_464, 0x19); // RAW8 SDK trace contains a false preamble marker.
        wire.extend_from_slice(&marker);
        let first = wire.len();
        wire.resize(first + size, 0x31);
        wire.extend_from_slice(&marker); // Footer is also frame 2's header.
        let second = wire.len();
        wire.resize(second + size, 0x62);
        wire.extend_from_slice(&marker);
        let mut candidate = None;
        for bytes in (512 * 1024..first + size + 8).step_by(512 * 1024) {
            assert_eq!(
                locate_raw8(&wire[..bytes], &mut candidate, size).unwrap(),
                None
            );
        }
        assert_eq!(
            locate_raw8(&wire[..first + size + 7], &mut candidate, size).unwrap(),
            None
        );
        assert_eq!(
            locate_raw8(&wire, &mut candidate, size).unwrap(),
            Some(first)
        );
        // Retain all eight bytes of the shared delimiter when consuming frame 1.
        let next = &wire[first + size..];
        assert_eq!(locate_raw8(next, &mut None, size).unwrap(), Some(8));
        assert_eq!(&next[8..8 + size], vec![0x62; size]);
        wire[first + size] ^= 1;
        // Never skip the corrupt first frame to deliver a later exposure.
        assert!(locate_raw8(&wire, &mut None, size).is_err());
        assert!(locate(&wire, &mut None, size).is_err());
    }
    #[test]
    fn full_sensor_preamble_split_footer_and_corruption() {
        let size = 9600 * 6456 * 2;
        let mut wire = START.to_vec();
        wire.resize(4472, 0x19); // separation observed in sdk-reference-wire
        wire.extend_from_slice(&START);
        let pixels = wire.len();
        wire.resize(pixels + size, 0x22);
        wire.extend_from_slice(&END);
        let mut candidate = None;
        assert_eq!(locate(&wire[..81920], &mut candidate, size).unwrap(), None);
        assert_eq!(
            locate(&wire[..wire.len() - 4], &mut candidate, size).unwrap(),
            None
        );
        assert_eq!(locate(&wire, &mut candidate, size).unwrap(), Some(pixels));
        wire[pixels + size] = 0;
        assert!(locate(&wire, &mut None, size).is_err());
        // A later valid exposure is outside the permitted preamble search.
        wire.extend_from_slice(&START);
        wire.resize(wire.len() + size, 0x22);
        wire.extend_from_slice(&END);
        assert!(locate(&wire, &mut None, size).is_err());
    }
}
