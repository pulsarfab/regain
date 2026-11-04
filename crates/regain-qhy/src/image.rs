//! Fallible image allocation and in-place packing for the 61-megapixel sensor.
use anyhow::{Context, Result, ensure};

pub(crate) fn zeroed(bytes: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(bytes)
        .with_context(|| format!("Cannot allocate QHY image buffer ({bytes} bytes)"))?;
    buffer.resize(bytes, 0);
    Ok(buffer)
}

/// Compact validated RAW16 rows in the original allocation. Full-frame delivery
/// must not allocate a second 123 MB image while the SDK buffer is still live.
pub(crate) fn crop_raw16(
    mut pixels: Vec<u8>,
    source_width: usize,
    source_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>> {
    let source_bytes = source_width
        .checked_mul(source_height)
        .and_then(|n| n.checked_mul(2))
        .context("QHY source geometry overflow")?;
    ensure!(
        pixels.len() == source_bytes
            && width > 0
            && height > 0
            && x.checked_add(width).is_some_and(|end| end <= source_width)
            && y.checked_add(height)
                .is_some_and(|end| end <= source_height),
        "Invalid QHY crop geometry"
    );
    if x != 0 || y != 0 || width != source_width {
        for row in 0..height {
            let start = ((y + row) * source_width + x) * 2;
            pixels.copy_within(start..start + width * 2, row * width * 2);
        }
    }
    pixels.truncate(width * height * 2);
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_sensor_and_overlapping_crop_reuse_allocation() {
        let mut pixels = zeroed(9600 * 6422 * 2).unwrap();
        for (index, pixel) in pixels.chunks_exact_mut(2).enumerate() {
            pixel.copy_from_slice(&(index as u16).to_le_bytes());
        }
        let allocation = pixels.as_ptr();
        let pixels = crop_raw16(pixels, 9600, 6422, 0, 0, 9600, 6422).unwrap();
        assert_eq!(pixels.as_ptr(), allocation);
        let pixels = crop_raw16(pixels, 9600, 6422, 3, 2, 9597, 6420).unwrap();
        assert_eq!(pixels.as_ptr(), allocation);
        assert_eq!(pixels.len(), 9597 * 6420 * 2);
        for (index, pixel) in pixels.chunks_exact(2).enumerate() {
            let source = ((index / 9597 + 2) * 9600 + index % 9597 + 3) as u16;
            assert_eq!(pixel, source.to_le_bytes());
        }
    }

    #[test]
    fn allocation_failure_and_invalid_geometry_are_errors() {
        assert!(zeroed(usize::MAX).is_err());
        assert!(crop_raw16(vec![0; 8], 2, 2, usize::MAX, 0, 1, 1).is_err());
        assert!(crop_raw16(vec![0; 8], 2, 2, 0, 1, 2, 2).is_err());
        assert!(crop_raw16(vec![0; 7], 2, 2, 0, 0, 2, 2).is_err());
    }
}
