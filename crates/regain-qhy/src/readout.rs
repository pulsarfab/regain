//! QHY600 U3G20 read-mode fields traced against the installed SDK.
use anyhow::{Result, ensure};

pub const MODES: &[(u8, &str)] = &[
    (0, "PhotoGraphic DSO 16BIT"),
    (1, "High Gain Mode 16BIT"),
    (2, "Extend Fullwell Mode"),
    (3, "Extend Fullwell 2CMS"),
    (8, "2CMS-0"),
    (9, "2CMS-1"),
];

pub fn validate(mode: u64) -> Result<u8> {
    ensure!(
        MODES.iter().any(|(id, _)| u64::from(*id) == mode),
        regain_core::invalid("Unsupported QHY readMode; expected 0, 1, 2, 3, 8 or 9")
    );
    Ok(mode as u8)
}

#[cfg(any(windows, test))]
pub fn stream_fields(mode: u8, video: bool) -> Result<[u8; 6]> {
    validate(u64::from(mode))?;
    // D1 A0: payload[1] selects still/live. Payload[6] is 03 in the
    // three 2CMS modes and 00 otherwise. This is the observed selector,
    // not an assertion about the meaning of each individual hardware bit.
    Ok([
        u8::from(!video),
        0,
        0,
        0,
        0,
        if matches!(mode, 3 | 8 | 9) { 3 } else { 0 },
    ])
}

// Exact SDK full-well analogue fields for gain 0..100. Fourteen entries
// are one below the nominal 2400 + 16 * gain line; preserve the traced
// values instead of rounding them to an inferred ideal mapping.
#[cfg(any(windows, test))]
const FULL_WELL_ANALOG: [u16; 101] = [
    2400, 2416, 2432, 2448, 2464, 2480, 2496, 2512, 2528, 2544, 2560, 2576, 2592, 2608, 2623, 2640,
    2656, 2671, 2688, 2703, 2720, 2736, 2752, 2768, 2784, 2800, 2816, 2832, 2848, 2864, 2880, 2896,
    2912, 2928, 2944, 2960, 2976, 2992, 3008, 3023, 3040, 3056, 3072, 3088, 3103, 3120, 3136, 3151,
    3168, 3183, 3200, 3216, 3231, 3248, 3264, 3280, 3296, 3312, 3328, 3344, 3360, 3376, 3392, 3408,
    3424, 3440, 3456, 3472, 3488, 3503, 3520, 3536, 3552, 3568, 3583, 3600, 3616, 3631, 3648, 3663,
    3680, 3696, 3711, 3727, 3744, 3760, 3776, 3792, 3808, 3824, 3840, 3856, 3872, 3888, 3904, 3920,
    3936, 3952, 3968, 3984, 4000,
];

#[cfg(any(windows, test))]
pub fn gain_fields(mode: u8, gain: u16) -> Result<[u8; 16]> {
    validate(u64::from(mode))?;
    ensure!(gain <= 200, "QHY gain outside 0..200");
    // A4's analogue/digital u16 fields and the two selector bytes are
    // mode-dependent. Unused SDK stack bytes are intentionally zeroed.
    let (analog, digital, selector, conversion) = match mode {
        0 | 8 => {
            let effective = if gain > 25 { gain - 25 } else { gain };
            (
                (2400 + 40 * effective).min(4000),
                8 + effective.saturating_sub(40),
                0,
                u8::from(gain > 25),
            )
        }
        1 | 9 if gain <= 55 => (40 * gain, 8, 3, 0),
        1 | 9 => (
            ((u32::from(gain - 55) * 4000) / 45).min(4000) as u16,
            8 + gain.saturating_sub(100),
            2,
            1,
        ),
        2 | 3 => (
            FULL_WELL_ANALOG[usize::from(gain.min(100))],
            8 + gain.saturating_sub(100),
            0,
            0,
        ),
        _ => unreachable!(),
    };
    let mut fields = [0; 16];
    fields[..2].copy_from_slice(&analog.to_be_bytes());
    fields[2..4].copy_from_slice(&digital.to_be_bytes());
    fields[13] = selector; // A4 payload byte 14
    fields[15] = conversion; // A4 payload byte 16
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gain_matches_the_sdk_trace_for_every_supported_read_mode() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/qhy600_sdk_gain_maps.json")).unwrap();
        for &(mode, _) in MODES {
            let expected = fixture["gains"][mode.to_string()].as_array().unwrap();
            assert_eq!(expected.len(), 201);
            for (gain, row) in expected.iter().enumerate() {
                let fields = gain_fields(mode, gain as u16).unwrap();
                let actual = [
                    u16::from_be_bytes([fields[0], fields[1]]),
                    u16::from_be_bytes([fields[2], fields[3]]),
                    u16::from(fields[13]),
                    u16::from(fields[15]),
                ];
                let expected: Vec<u16> = row
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u16)
                    .collect();
                assert_eq!(actual.as_slice(), expected, "mode {mode}, gain {gain}");
                assert!(
                    fields
                        .iter()
                        .enumerate()
                        .all(|(i, v)| matches!(i, 0 | 1 | 2 | 3 | 13 | 15) || *v == 0)
                );
            }
            assert!(gain_fields(mode, 201).is_err());
            assert!(gain_fields(mode, u16::MAX).is_err());
        }
        for mode in [4, 5, 6, 7, 10, 255] {
            assert!(gain_fields(mode, 30).is_err());
            assert!(stream_fields(mode, true).is_err());
        }
    }

    #[test]
    fn two_cms_selector_and_default_mode_match_sdk_initialization() {
        for &(mode, _) in MODES {
            let live = stream_fields(mode, true).unwrap();
            let still = stream_fields(mode, false).unwrap();
            assert_eq!(live[0], 0);
            assert_eq!(still[0], 1);
            assert_eq!(&live[1..], &still[1..]);
            assert_eq!(live[5], if matches!(mode, 3 | 8 | 9) { 3 } else { 0 });
        }
        assert_eq!(
            gain_fields(0, 30).unwrap(),
            [0x0a, 0x28, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
    }
}
