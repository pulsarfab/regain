//! ASI676MC RAW16/bin-1 factory correction, independently derived from SDK 1.41.
//! Calibration and image bytes stay in memory. Unsupported maps fail closed.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub const SENSOR: usize = 3552;

pub fn declared_length(data: &[u8]) -> Result<usize> {
    ensure!(
        data.len() >= 8 && &data[..4] == b"ASID",
        "missing ASID calibration header"
    );
    let length = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    ensure!(
        (8..=0x30000).contains(&length) && length.is_multiple_of(2),
        "invalid ASID length"
    );
    Ok(length)
}

pub struct Defects {
    pub indices: Vec<usize>,
    mask: Vec<bool>,
    width: usize,
    height: usize,
}

impl Defects {
    pub fn decode(data: &[u8], width: usize, height: usize, x: usize, y: usize) -> Result<Self> {
        ensure!(
            width >= 8
                && height >= 4
                && width <= SENSOR
                && height <= SENSOR
                && x <= SENSOR - width
                && y <= SENSOR - height,
            "invalid correction ROI"
        );
        let length = declared_length(data)?;
        ensure!(data.len() >= length, "truncated ASID calibration");
        let mut packed = vec![0_u8; SENSOR * SENSOR / 8];
        let mut base = 0_usize;
        for pair in data[8..length].as_chunks::<2>().0 {
            if pair == &[0, 0] {
                base += 256;
                ensure!(
                    base <= packed.len().next_multiple_of(256),
                    "ASID block overflow"
                );
            } else {
                let offset = pair[0].rotate_left(4) as usize;
                ensure!(base + offset < packed.len(), "ASID pixel offset overflow");
                packed[base + offset] = pair[1];
            }
        }
        let mut mask = vec![false; width * height];
        let mut indices = Vec::new();
        for row in 0..height {
            for column in 0..width {
                let sensor = (y + row) * SENSOR + x + column;
                if packed[sensor / 8] & (1 << (sensor % 8)) != 0 {
                    let index = row * width + column;
                    mask[index] = true;
                    indices.push(index);
                }
            }
        }
        ensure!(indices.len() <= 100_000, "unsupported defect count");
        ensure!(
            !(0..height).any(|r| (0..width).all(|c| mask[r * width + c]))
                && !(0..width).any(|c| (0..height).all(|r| mask[r * width + c])),
            "row/column defect correction is not implemented"
        );
        Ok(Self {
            indices,
            mask,
            width,
            height,
        })
    }

    pub fn index_hash(&self) -> String {
        let mut hash = Sha256::new();
        for &index in &self.indices {
            hash.update((index as u32).to_le_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    pub fn correct(&self, data: &mut [u8]) -> Result<()> {
        ensure!(
            data.len() == self.width * self.height * 2,
            "correction frame length mismatch"
        );
        let pixel = |data: &[u8], i: usize| u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
        for (position, &index) in self.indices.iter().enumerate() {
            let x = index % self.width;
            let y = index / self.width;
            let value = if x < 2 || y < 2 {
                let right = (index + 2).min(self.mask.len() - 1);
                let source = if self.indices.get(position + 1) != Some(&right) {
                    right
                } else if index >= 2 * self.width {
                    index - 2 * self.width
                } else {
                    let below = index + 2 * self.width;
                    if self.indices[position + 1..].binary_search(&below).is_ok() {
                        (below + 2).min(self.mask.len() - 1)
                    } else {
                        below
                    }
                };
                pixel(data, source)
            } else if x >= self.width - 2 || y >= self.height - 2 {
                pixel(data, index - 2)
            } else {
                let mut sum = 0_u32;
                let mut count = 0;
                for neighbor in [
                    index - 2 * self.width,
                    index - 2,
                    index + 2,
                    index + 2 * self.width,
                ] {
                    if !self.mask[neighbor] || neighbor <= index {
                        sum += u32::from(pixel(data, neighbor));
                        count += 1;
                    }
                }
                let value = sum
                    .checked_div(count)
                    .map(|v| v as u16)
                    .unwrap_or_else(|| pixel(data, index - 1));
                value & 0xfff0
            };
            data[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

/// In-memory comparison harness: u32 JSON length, JSON, calibration, RAW16 wire frame.
pub fn process_stream() -> Result<()> {
    use std::io::{Read, Write};
    let mut input = std::io::stdin().lock();
    let mut size = [0_u8; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    ensure!(size <= 4096, "processing header too large");
    let mut header = vec![0; size];
    input.read_exact(&mut header)?;
    let request: serde_json::Value = serde_json::from_slice(&header)?;
    let number = |key: &str| -> Result<usize> {
        Ok(usize::try_from(
            request[key]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("missing {key}"))?,
        )?)
    };
    let (width, height, x, y) = (
        number("width")?,
        number("height")?,
        number("x")?,
        number("y")?,
    );
    let length = number("calibrationBytes")?;
    ensure!(length <= 0x30000, "calibration too large");
    let mut calibration = vec![0; length];
    input.read_exact(&mut calibration)?;
    let defects = Defects::decode(&calibration, width, height, x, y)?;
    let mut data = vec![0; width * height * 2];
    input.read_exact(&mut data)?;
    crate::protocol::replace_envelope(&mut data, width)?;
    defects.correct(&mut data)?;
    let metadata = serde_json::to_vec(&serde_json::json!({
        "defectCount":defects.indices.len(),"defectIndexSha256":defects.index_hash(),"bytes":data.len()
    }))?;
    let mut output = std::io::stdout().lock();
    output.write_all(&(metadata.len() as u32).to_le_bytes())?;
    output.write_all(&metadata)?;
    output.write_all(&data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asid_decodes_nibbles_blocks_and_lsb_bits() {
        let blob = [
            b'A', b'S', b'I', b'D', 0, 0, 0, 14, 0x10, 0x81, 0, 0, 0x20, 1,
        ];
        let defects = Defects::decode(&blob, SENSOR, 4, 0, 0).unwrap();
        assert_eq!(defects.indices, [8, 15, 2064]);
        assert!(Defects::decode(&blob[..13], SENSOR, 4, 0, 0).is_err());
    }
    #[test]
    fn corrupt_calibration_is_rejected() {
        assert!(declared_length(b"wrong").is_err());
        assert!(declared_length(b"ASID\0\x04\0\0").is_err());
        assert!(declared_length(b"ASID\0\0\0\x09").is_err());
    }
    #[test]
    fn correction_uses_same_color_neighbors_in_order() {
        let indices = vec![27, 29];
        let mut mask = vec![false; 64];
        for &i in &indices {
            mask[i] = true;
        }
        let defects = Defects {
            indices,
            mask,
            width: 8,
            height: 8,
        };
        let mut data: Vec<u8> = (0..64_u16).flat_map(|v| (v * 16).to_le_bytes()).collect();
        data[54..56].copy_from_slice(&65535_u16.to_le_bytes());
        data[58..60].copy_from_slice(&65535_u16.to_le_bytes());
        defects.correct(&mut data).unwrap();
        assert_eq!(u16::from_le_bytes(data[54..56].try_into().unwrap()), 416);
        assert_eq!(u16::from_le_bytes(data[58..60].try_into().unwrap()), 448);
    }
}
