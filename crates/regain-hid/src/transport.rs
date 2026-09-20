//! Platform HID APIs. No libusb, hidapi C library, or ZWO SDK is linked.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Device, enumerate};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Device, enumerate};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Device, enumerate};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceInfo {
    pub path: String,
}

// HID report descriptor parser for Linux hidraw. Count bits for each report
// type/ID; byte zero in our transport always contains the report ID.
#[cfg(any(target_os = "linux", test))]
fn report_lengths(descriptor: &[u8]) -> anyhow::Result<(usize, usize)> {
    use anyhow::ensure;
    let mut cursor = 0;
    let (mut size, mut count, mut id) = (0_u32, 0_u32, 0_u32);
    let mut stack = Vec::new();
    let mut input_bits = 0_u32;
    let mut output_bits = 0_u32;
    while cursor < descriptor.len() {
        let prefix = descriptor[cursor];
        cursor += 1;
        ensure!(prefix != 0xfe, "unsupported HID long item");
        let length = match prefix & 3 {
            3 => 4,
            x => x as usize,
        };
        ensure!(
            cursor + length <= descriptor.len(),
            "truncated HID descriptor"
        );
        let mut bytes = [0_u8; 4];
        bytes[..length].copy_from_slice(&descriptor[cursor..cursor + length]);
        cursor += length;
        let value = u32::from_le_bytes(bytes);
        match prefix & 0xfc {
            0x74 => size = value,
            0x94 => count = value,
            0x84 => id = value,
            0xa4 => stack.push((size, count, id)),
            0xb4 => {
                (size, count, id) = stack
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("unbalanced HID globals"))?
            }
            0x80 if id == 1 => {
                input_bits = input_bits
                    .checked_add(
                        size.checked_mul(count)
                            .ok_or_else(|| anyhow::anyhow!("HID size overflow"))?,
                    )
                    .ok_or_else(|| anyhow::anyhow!("HID size overflow"))?
            }
            0x90 if id == 3 => {
                output_bits = output_bits
                    .checked_add(
                        size.checked_mul(count)
                            .ok_or_else(|| anyhow::anyhow!("HID size overflow"))?,
                    )
                    .ok_or_else(|| anyhow::anyhow!("HID size overflow"))?
            }
            _ => (),
        }
    }
    let input = input_bits.div_ceil(8) as usize + 1;
    let output = output_bits.div_ceil(8) as usize + 1;
    ensure!(
        (16..=128).contains(&input) && (16..=128).contains(&output),
        "unexpected HID report sizes: {input}/{output}"
    );
    Ok((input, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_lengths_and_bad_input() {
        assert_eq!(
            report_lengths(&[
                0x75, 8, 0x95, 17, 0x85, 1, 0x81, 2, 0x95, 15, 0x85, 3, 0x91, 2
            ])
            .unwrap(),
            (18, 16)
        );
        assert!(report_lengths(&[0x75]).is_err());
        assert!(report_lengths(&[]).is_err());
        assert!(report_lengths(&[0xb4]).is_err());
    }
}
