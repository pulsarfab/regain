//! ASI676MC, ASI2600MM Duo and ASI220MM Mini RAW16 processing from SDK 1.41.
//! Calibration and image bytes stay in memory. Unsupported maps fail closed.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub const SENSOR: usize = 3552;

/// ASI220 RAW16 unpacking and SDK 1.41 low-gain dither. The seed is captured
/// from the SDK for comparison; independent acquisition supplies its own seed.
pub fn unpack_guide(data: &mut [u8], gain: i32, mut seed: u32) -> Result<()> {
    ensure!(data.len().is_multiple_of(2), "odd guide wire length");
    for pair in data.chunks_exact_mut(2) {
        let mut value = (u16::from(pair[0]) << 4) | u16::from(pair[1] & 15);
        if gain < 100 && value > 31 {
            seed = seed.wrapping_mul(214013).wrapping_add(2531011);
            value ^= ((seed >> 16) & 1) as u16;
        }
        pair.copy_from_slice(&(value << 4).to_le_bytes());
    }
    Ok(())
}

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
    step: usize,
    depth_mask: u16,
}

/// SDK RAW16 software binning: integer average of each square, after correction.
pub fn bin_average(data: &[u8], width: usize, height: usize, bin: usize) -> Result<Vec<u8>> {
    ensure!(
        (1..=4).contains(&bin)
            && width.is_multiple_of(bin)
            && height.is_multiple_of(bin)
            && data.len() == width * height * 2,
        "invalid software bin geometry"
    );
    let mut result = Vec::with_capacity(data.len() / bin / bin);
    for y in (0..height).step_by(bin) {
        for x in (0..width).step_by(bin) {
            let mut sum = 0_u32;
            for dy in 0..bin {
                for dx in 0..bin {
                    let i = ((y + dy) * width + x + dx) * 2;
                    sum += u16::from_le_bytes([data[i], data[i + 1]]) as u32;
                }
            }
            result.extend_from_slice(&((sum / (bin * bin) as u32) as u16).to_le_bytes());
        }
    }
    Ok(result)
}

impl Defects {
    pub fn decode(data: &[u8], width: usize, height: usize, x: usize, y: usize) -> Result<Self> {
        Self::decode_profile(data, (width, height, x, y), (SENSOR, SENSOR, 2, 12))
    }

    pub fn decode_duo(
        data: &[u8],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
    ) -> Result<Self> {
        Self::decode_profile(data, (width, height, x, y), (6248, 4176, 1, 16))
    }

    pub fn decode_guide(
        data: &[u8],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
    ) -> Result<Self> {
        Self::decode_profile(data, (width, height, x, y), (1920, 1080, 1, 12))
    }

    fn decode_profile(
        data: &[u8],
        roi: (usize, usize, usize, usize),
        profile: (usize, usize, usize, u8),
    ) -> Result<Self> {
        let (width, height, x, y) = roi;
        let (sensor_width, sensor_height, step, depth) = profile;
        ensure!(
            width >= 8
                && height >= 4
                && width <= sensor_width
                && height <= sensor_height
                && x <= sensor_width - width
                && y <= sensor_height - height,
            "invalid correction ROI"
        );
        let length = declared_length(data)?;
        ensure!(data.len() >= length, "truncated ASID calibration");
        let mut packed = vec![0_u8; sensor_width * sensor_height / 8];
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
                let sensor = (y + row) * sensor_width + x + column;
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
            step,
            depth_mask: u16::MAX << (16 - depth),
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
        let step = self.step;
        for (position, &index) in self.indices.iter().enumerate() {
            let x = index % self.width;
            let y = index / self.width;
            let value = if x < step || y < step {
                let right = (index + step).min(self.mask.len() - 1);
                let source = if self.indices.get(position + 1) != Some(&right) {
                    right
                } else if index >= step * self.width {
                    index - step * self.width
                } else {
                    let below = index + step * self.width;
                    if self.indices[position + 1..].binary_search(&below).is_ok() {
                        (below + step).min(self.mask.len() - 1)
                    } else {
                        below
                    }
                };
                pixel(data, source)
            } else if x >= self.width - step || y >= self.height - step {
                pixel(data, index - step)
            } else {
                let mut sum = 0_u32;
                let mut count = 0;
                for neighbor in [
                    index - step * self.width,
                    index - step,
                    index + step,
                    index + step * self.width,
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
                value & self.depth_mask
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
    let (mut width, mut height, mut x, mut y) = (
        number("width")?,
        number("height")?,
        number("x")?,
        number("y")?,
    );
    let length = number("calibrationBytes")?;
    ensure!(length <= 0x30000, "calibration too large");
    let mut calibration = vec![0; length];
    input.read_exact(&mut calibration)?;
    let model = match request["model"].as_str() {
        None | Some("asi676mc") => "asi676mc",
        Some("asi2600mm-duo") => "asi2600mm-duo",
        Some("asi220mm-mini") => "asi220mm-mini",
        _ => anyhow::bail!("unsupported processing model"),
    };
    let bin = request["bin"].as_u64().unwrap_or(1) as usize;
    ensure!(
        (1..=4).contains(&bin) && (model != "asi676mc" || bin == 1),
        "unsupported processing bin"
    );
    width = width
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("width overflow"))?;
    height = height
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("height overflow"))?;
    x = x
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("origin overflow"))?;
    y = y
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("origin overflow"))?;
    let defects = match model {
        "asi2600mm-duo" => Defects::decode_duo(&calibration, width, height, x, y)?,
        "asi220mm-mini" => Defects::decode_guide(&calibration, width, height, x, y)?,
        _ => Defects::decode(&calibration, width, height, x, y)?,
    };
    let mut data = vec![0; width * height * 2];
    input.read_exact(&mut data)?;
    if model != "asi676mc" {
        let last = data.len() - 4;
        data.copy_within(width * 2..width * 2 + 4, 0);
        data.copy_within(last - width * 2..last - width * 2 + 4, last);
    } else {
        crate::protocol::replace_envelope(&mut data, width)?;
    }
    if model == "asi220mm-mini" {
        let gain = i32::try_from(
            request["gain"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("missing guide gain"))?,
        )?;
        let seed = u32::try_from(number("ditherSeed")?)?;
        unpack_guide(&mut data, gain, seed)?;
    }
    defects.correct(&mut data)?;
    if bin > 1 {
        data = bin_average(&data, width, height, bin)?;
    }
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
    fn software_bins_average_without_saturation_and_reject_bad_geometry() {
        let data: Vec<u8> = [0_u16, 1, 65535, 65535, 2, 4, 65535, 65535]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(bin_average(&data, 4, 2, 2).unwrap(), [1, 0, 255, 255]);
        assert_eq!(bin_average(&data, 4, 2, 1).unwrap(), data);
        assert!(bin_average(&data, 4, 2, 3).is_err());
        assert!(bin_average(&data[..6], 4, 2, 2).is_err());
    }
    #[test]
    fn guide_unpack_dithers_only_eligible_samples_and_wraps_rng() {
        let mut data = [1, 0, 1, 15, 255, 15, 0, 2];
        unpack_guide(&mut data, 100, 0).unwrap();
        assert_eq!(data, [0, 1, 240, 1, 240, 255, 32, 0]);
        let mut data = [1, 0, 2, 0, 255, 15, 2, 0];
        unpack_guide(&mut data, 0, 1).unwrap();
        // MSVC rand seed 1: 41,18467,6334; sample 16 does not consume RNG.
        assert_eq!(data, [0, 1, 16, 2, 224, 255, 0, 2]);
        assert!(unpack_guide(&mut [0], 0, 0).is_err());
    }
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
    fn duo_monochrome_correction_preserves_sixteen_bit_precision() {
        let blob = [b'A', b'S', b'I', b'D', 0, 0, 0, 10, 0, 2];
        let defects = Defects::decode_duo(&blob, 8, 4, 0, 0).unwrap();
        assert_eq!(defects.indices, [1]);
        let mut data: Vec<u8> = (0..32_u16)
            .flat_map(|v| (v * 19 + 3).to_le_bytes())
            .collect();
        defects.correct(&mut data).unwrap();
        assert_eq!(u16::from_le_bytes(data[2..4].try_into().unwrap()), 41);
        assert!(Defects::decode_duo(&blob, 6248, 4176, 16, 0).is_err());
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
            step: 2,
            depth_mask: 0xfff0,
        };
        let mut data: Vec<u8> = (0..64_u16).flat_map(|v| (v * 16).to_le_bytes()).collect();
        data[54..56].copy_from_slice(&65535_u16.to_le_bytes());
        data[58..60].copy_from_slice(&65535_u16.to_le_bytes());
        defects.correct(&mut data).unwrap();
        assert_eq!(u16::from_le_bytes(data[54..56].try_into().unwrap()), 416);
        assert_eq!(u16::from_le_bytes(data[58..60].try_into().unwrap()), 448);
    }
}
