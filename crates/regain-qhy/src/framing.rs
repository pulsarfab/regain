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
    if candidate.is_none() {
        *candidate = wire[..wire.len().min(PREFIX)]
            .windows(8)
            .position(|w| w == START)
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
        if wire[end - 8..end] == END {
            return Ok(Some(at));
        }
        // SDK traces contain START inside a 4–5 KiB transport preamble. Only a
        // candidate with a footer at the exact frame length can delimit pixels.
        // Search stays inside the prefix, so a corrupt frame cannot silently
        // advance to a different exposure later in the stream.
        *candidate = wire
            .get(at..wire.len().min(PREFIX))
            .and_then(|p| p.windows(8).position(|w| w == START))
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
