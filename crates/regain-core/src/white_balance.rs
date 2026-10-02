//! Backend-independent, opt-in white balance for little-endian RAW16 Bayer frames.
//! Raw output is the default: AWB can estimate gains without modifying science data.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Off,
    Manual,
    Once,
    Continuous,
    Locked,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Output {
    #[default]
    Raw,
    Corrected,
}

/// Linear multipliers relative to green = 1, NOT the vendor's WB_R/WB_B scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gains {
    pub red: f64,
    pub blue: f64,
}
impl Default for Gains {
    fn default() -> Self {
        Self { red: 1., blue: 1. }
    }
}
impl Gains {
    pub fn validate(self) -> Result<()> {
        ensure!(
            [self.red, self.blue]
                .iter()
                .all(|v| v.is_finite() && (0.125..=8.).contains(v)),
            "white balance gains must be finite linear multipliers between 0.125 and 8"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    pub mode: Mode,
    pub gains: Gains,
    pub output: Output,
}

#[derive(Clone, Debug, Default)]
pub struct WhiteBalance {
    settings: Settings,
}
impl WhiteBalance {
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Lock freezes the last effective gains; to restore saved gains use Manual.
    pub fn configure(&mut self, settings: Settings) -> Result<()> {
        settings.gains.validate()?;
        let gains = match settings.mode {
            Mode::Off => Gains::default(),
            Mode::Locked => self.settings.gains,
            _ => settings.gains,
        };
        self.settings = Settings { gains, ..settings };
        Ok(())
    }

    pub fn capabilities(supported: bool) -> Value {
        json!({"supported":supported,"implementation":"regain-software-v1",
            "modes":["off","manual","once","continuous","locked"],
            "outputs":["raw","corrected"],"defaultOutput":"raw",
            "gainMin":0.125,"gainMax":8.0,"gainNeutral":1.0,
            "formats":["RAW16"],"bins":[1],"flip":0})
    }

    pub fn validate_geometry(color: bool, bayer: Option<u64>, bin: u32) -> Result<()> {
        ensure!(
            color && bayer.is_some_and(|b| b <= 3),
            "white balance requires a known color Bayer camera"
        );
        ensure!(
            bin == 1,
            "managed white balance requires unbinned Bayer data (bin 1)"
        );
        Ok(())
    }

    /// Bayer enum follows ASI: RGGB=0, BGGR=1, GRBG=2, GBRG=3.
    /// x/y are the unbinned sensor ROI origin; flips are rejected by the host.
    pub fn process(&mut self, pixels: &mut [u8], frame: Geometry) -> Result<Value> {
        let Geometry {
            width,
            height,
            x,
            y,
            bayer,
            dark,
        } = frame;
        ensure!(
            width > 0
                && height > 0
                && width.is_multiple_of(2)
                && height.is_multiple_of(2)
                && width.checked_mul(height).and_then(|n| n.checked_mul(2)) == Some(pixels.len())
                && bayer <= 3,
            "invalid white balance frame geometry"
        );
        let mut estimation = "not-requested";
        let mut samples = 0;
        if dark {
            estimation = "dark-frame";
        } else if matches!(self.settings.mode, Mode::Once | Mode::Continuous) {
            let (estimate, count, reason) = estimate(pixels, frame);
            samples = count;
            estimation = reason;
            if let Some(gains) = estimate {
                if self.settings.mode == Mode::Once {
                    self.settings.gains = gains;
                    self.settings.mode = Mode::Locked;
                } else {
                    self.settings.gains.red += 0.2 * (gains.red - self.settings.gains.red);
                    self.settings.gains.blue += 0.2 * (gains.blue - self.settings.gains.blue);
                }
            }
        }
        let applied =
            !dark && self.settings.output == Output::Corrected && self.settings.mode != Mode::Off;
        if applied {
            for (index, pixel) in pixels.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let gain = match channel(
                    bayer,
                    (x % 2) as usize + index % width,
                    (y % 2) as usize + index / width,
                ) {
                    0 => self.settings.gains.red,
                    2 => self.settings.gains.blue,
                    _ => 1.,
                };
                let value = (f64::from(u16::from_le_bytes([pixel[0], pixel[1]])) * gain)
                    .round()
                    .min(65535.) as u16;
                pixel.copy_from_slice(&value.to_le_bytes());
            }
        }
        Ok(
            json!({"implementation":"regain-software-v1","settings":self.settings,
            "applied":applied,"estimation":estimation,"sampleCells":samples,
            "input":"neutral-bayer-raw16","bayer":bayer,"roiX":x,"roiY":y}),
        )
    }
}

#[derive(Clone, Copy)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub x: u32,
    pub y: u32,
    pub bayer: u8,
    pub dark: bool,
}

fn channel(bayer: u8, x: usize, y: usize) -> usize {
    const CHANNELS: [[usize; 4]; 4] = [[0, 1, 1, 2], [2, 1, 1, 0], [1, 0, 2, 1], [1, 2, 0, 1]];
    CHANNELS[bayer as usize][(y % 2) * 2 + x % 2]
}

fn estimate(pixels: &[u8], g: Geometry) -> (Option<Gains>, usize, &'static str) {
    // Sample whole Bayer cells, not individual channels: clipped stars or one
    // dark channel exclude the entire cell and cannot bias the remaining colors.
    // Bound work to at most 65,536 cells, including very narrow ROIs.
    let cells = (g.width / 2) * (g.height / 2);
    let stride = cells.div_ceil(65536).max(1);
    let mut sum = [0u64; 3];
    let (mut count, mut attempted) = (0usize, 0usize);
    for index in (0..cells).step_by(stride) {
        let x = (index % (g.width / 2)) * 2;
        let y = (index / (g.width / 2)) * 2;
        attempted += 1;
        let mut cell = [0u64; 3];
        let mut usable = true;
        for dy in 0..2 {
            for dx in 0..2 {
                let index = ((y + dy) * g.width + x + dx) * 2;
                let value = u16::from_le_bytes([pixels[index], pixels[index + 1]]);
                usable &= (1024..=58981).contains(&value);
                cell[channel(
                    g.bayer,
                    (g.x % 2) as usize + x + dx,
                    (g.y % 2) as usize + y + dy,
                )] += u64::from(value);
            }
        }
        if usable {
            count += 1;
            for c in 0..3 {
                sum[c] += cell[c];
            }
        }
    }
    if count < 64 || count * 10 < attempted {
        return (None, count, "insufficient-signal");
    }
    let green = sum[1] as f64 / 2.;
    let gains = Gains {
        red: green / sum[0] as f64,
        blue: green / sum[2] as f64,
    };
    if gains.validate().is_err() {
        return (None, count, "gain-out-of-range");
    }
    (Some(gains), count, "updated")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(bayer: u8, x: u32, y: u32) -> (Geometry, Vec<u8>) {
        let g = Geometry {
            width: 32,
            height: 32,
            x,
            y,
            bayer,
            dark: false,
        };
        let p = (0..1024)
            .flat_map(|i| {
                [4000u16, 8000, 16000][channel(bayer, x as usize + i % 32, y as usize + i / 32)]
                    .to_le_bytes()
            })
            .collect();
        (g, p)
    }
    #[test]
    fn all_patterns_and_roi_phases_balance_and_lock() {
        for b in 0..4 {
            for x in 0..2 {
                for y in 0..2 {
                    let (g, mut p) = frame(b, x, y);
                    let mut wb = WhiteBalance::default();
                    wb.configure(Settings {
                        mode: Mode::Once,
                        output: Output::Corrected,
                        ..Settings::default()
                    })
                    .unwrap();
                    let meta = wb.process(&mut p, g).unwrap();
                    assert_eq!(wb.settings.mode, Mode::Locked);
                    assert_eq!(wb.settings.gains, Gains { red: 2., blue: 0.5 });
                    assert_eq!(meta["applied"], true);
                    assert!(
                        p.as_chunks::<2>()
                            .0
                            .iter()
                            .all(|v| u16::from_le_bytes([v[0], v[1]]) == 8000)
                    );
                }
            }
        }
    }
    #[test]
    fn raw_is_preserved_and_auto_is_smoothed() {
        let (g, mut p) = frame(0, 0, 0);
        let raw = p.clone();
        let mut wb = WhiteBalance::default();
        wb.configure(Settings {
            mode: Mode::Continuous,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(wb.process(&mut p, g).unwrap()["applied"], false);
        assert_eq!(p, raw);
        assert_eq!(
            wb.settings.gains,
            Gains {
                red: 1.2,
                blue: 0.9
            }
        );
        wb.configure(Settings {
            mode: Mode::Locked,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(wb.settings.gains.red, 1.2);
        wb.process(&mut p, g).unwrap();
        assert_eq!(wb.settings.gains.red, 1.2);
    }
    #[test]
    fn bad_signal_and_dark_frames_do_not_update_or_correct() {
        let (mut g, mut p) = frame(0, 0, 0);
        let mut wb = WhiteBalance::default();
        wb.configure(Settings {
            mode: Mode::Once,
            output: Output::Corrected,
            ..Settings::default()
        })
        .unwrap();
        g.dark = true;
        let raw = p.clone();
        assert_eq!(wb.process(&mut p, g).unwrap()["applied"], false);
        assert_eq!(raw, p);
        g.dark = false;
        for value in [0u8, 255] {
            p.fill(value);
            assert_eq!(
                wb.process(&mut p, g).unwrap()["estimation"],
                "insufficient-signal"
            );
        }
        assert_eq!(wb.settings.mode, Mode::Once);
        assert_eq!(wb.settings.gains, Gains::default());
    }
    #[test]
    fn clipped_cells_are_excluded_together_and_extreme_casts_hold_gains() {
        let (g, mut p) = frame(0, 0, 0);
        // A clipped red sample rejects its blue and both green partners too.
        for y in (0..16).step_by(2) {
            for x in (0..32).step_by(2) {
                let i = (y * 32 + x) * 2;
                p[i..i + 2].copy_from_slice(&65535u16.to_le_bytes());
                let i = ((y + 1) * 32 + x + 1) * 2;
                p[i..i + 2].copy_from_slice(&2000u16.to_le_bytes());
            }
        }
        let mut wb = WhiteBalance::default();
        wb.configure(Settings {
            mode: Mode::Once,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(wb.process(&mut p, g).unwrap()["sampleCells"], 128);
        assert_eq!(wb.settings.gains, Gains { red: 2., blue: 0.5 });
        wb.configure(Settings {
            mode: Mode::Continuous,
            gains: wb.settings.gains,
            ..Settings::default()
        })
        .unwrap();
        for (i, v) in p.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let value: u16 = if channel(0, i % 32, i / 32) == 0 {
                1024
            } else {
                50000
            };
            v.copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(
            wb.process(&mut p, g).unwrap()["estimation"],
            "gain-out-of-range"
        );
        assert_eq!(wb.settings.gains, Gains { red: 2., blue: 0.5 });
    }
    #[test]
    fn estimation_has_a_fixed_sample_budget() {
        let g = Geometry {
            width: 131072,
            height: 2,
            x: 0,
            y: 0,
            bayer: 0,
            dark: false,
        };
        let mut p = vec![0u8; g.width * g.height * 2];
        for v in p.as_chunks_mut::<2>().0 {
            v.copy_from_slice(&8000u16.to_le_bytes());
        }
        let mut wb = WhiteBalance::default();
        wb.configure(Settings {
            mode: Mode::Once,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(wb.process(&mut p, g).unwrap()["sampleCells"], 65536);
        assert_eq!(wb.settings.gains, Gains::default());
    }
    #[test]
    fn validation_is_atomic_and_correction_saturates() {
        let mut wb = WhiteBalance::default();
        for red in [f64::NAN, f64::INFINITY, 0., 8.1] {
            assert!(
                wb.configure(Settings {
                    gains: Gains { red, blue: 1. },
                    ..Settings::default()
                })
                .is_err()
            );
            assert_eq!(wb.settings, Settings::default());
        }
        assert!(WhiteBalance::validate_geometry(false, Some(0), 1).is_err());
        assert!(WhiteBalance::validate_geometry(true, Some(4), 1).is_err());
        assert!(WhiteBalance::validate_geometry(true, Some(0), 2).is_err());
        let (g, mut p) = frame(0, 0, 0);
        wb.configure(Settings {
            mode: Mode::Manual,
            gains: Gains { red: 8., blue: 8. },
            output: Output::Corrected,
        })
        .unwrap();
        wb.process(&mut p, g).unwrap();
        assert_eq!(u16::from_le_bytes([p[66], p[67]]), 65535);
        assert!(wb.process(&mut p[..3], g).is_err());
        wb.configure(Settings::default()).unwrap();
        assert_eq!(wb.settings.gains, Gains::default());
    }
}
