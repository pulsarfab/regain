//! ASI585MM Pro/ASI662MC/ASI676MC RAW16 video: configure once, consume successive envelopes.
//! Model-specific SDK 1.41 video traces; never replay a live video frame.
use super::{bayer, link, processing, protocol, settings::Settings, transport::Camera};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub struct Video {
    profile: &'static bayer::Profile,
    settings: Settings,
    defects: processing::Defects,
    previous: Option<u16>,
    delivered: u64,
    active: bool,
    cleanup_failed: bool,
}

pub fn validate(settings: &Settings, profile: &bayer::Profile) -> Result<()> {
    profile.validate(settings)?;
    frame_interval(settings.video_max_fps)?;
    ensure!(
        settings.microseconds <= 30_000_000,
        "video exposure exceeds the 30-second experimental limit"
    );
    ensure!(
        settings.interrupt_read_after_bytes == 0
            && settings.replay_prefix_bytes == 0
            && !settings.keep_retained,
        "video does not support retained-frame replay"
    );
    Ok(())
}

pub fn frame_interval(max_fps: f64) -> Result<Duration> {
    ensure!(
        max_fps.is_finite() && (0.01..=120.0).contains(&max_fps),
        "video maxFps must be 0.01..120"
    );
    Ok(Duration::from_secs_f64(1.0 / max_fps))
}

/// A cap, not a promise of throughput. Do not burst to catch up after a stall.
/// Kept by the owner across exposure/ROI changes, so AE cannot bypass the cap.
/// The interval begins at the previous completed grab. Exposure/readout add to
/// it: never grab early and merely hold back the published frame.
#[derive(Default)]
pub struct Pacer {
    previous: Option<Instant>,
}
impl Pacer {
    pub fn wait(&mut self, max_fps: f64, cancel: &AtomicBool) -> Result<()> {
        self.wait_servicing(max_fps, cancel, || Ok(()))
    }
    pub fn wait_servicing(
        &mut self,
        max_fps: f64,
        cancel: &AtomicBool,
        service: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let interval = frame_interval(max_fps)?;
        if let Some(previous) = self.previous {
            wait_until_servicing(previous, interval, cancel, service)?;
        }
        ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
        Ok(())
    }
    pub fn completed(&mut self) {
        self.previous = Some(Instant::now());
    }
}

#[cfg(test)]
pub(super) fn wait_until(start: Instant, duration: Duration, cancel: &AtomicBool) -> Result<()> {
    wait_until_servicing(start, duration, cancel, || Ok(()))
}

pub(super) fn wait_until_servicing(
    start: Instant,
    duration: Duration,
    cancel: &AtomicBool,
    mut service: impl FnMut() -> Result<()>,
) -> Result<()> {
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
        let remaining = duration.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Ok(());
        }
        service()?;
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

#[test]
fn wait_services_cooling_and_propagates_feedback_failure() {
    let mut calls = 0;
    let error = wait_until_servicing(
        Instant::now(),
        Duration::from_secs(60),
        &AtomicBool::new(false),
        || {
            calls += 1;
            if calls == 2 {
                anyhow::bail!("temperature unavailable");
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(calls, 2);
    assert!(error.to_string().contains("temperature unavailable"));
}

impl Video {
    pub fn can_update(&self, settings: &Settings) -> bool {
        let mut prior = self.settings.clone();
        prior.microseconds = settings.microseconds;
        prior.gain = settings.gain;
        prior.video_max_fps = settings.video_max_fps;
        self.active && prior == *settings
    }

    /// Called only between completed reads on the exclusive USB owner.
    /// Never touch ROI, calibration, standby or the bulk pipe for scalar edits.
    pub fn update(&mut self, camera: &Camera, info: &Value, settings: &Settings) -> Result<()> {
        ensure!(
            self.can_update(settings),
            "structural video change requires restart"
        );
        validate(settings, self.profile)?;
        if self.settings.microseconds != settings.microseconds {
            let speed = link::validate(info, u32::from(self.profile.pid))?;
            bayer::set_exposure(camera, self.profile, settings, speed)?;
        }
        if self.settings.gain != settings.gain {
            bayer::set_gain(camera, self.profile, settings.gain)?;
        }
        self.settings = settings.clone();
        Ok(())
    }

    pub fn matches(&self, settings: &Settings) -> bool {
        let mut prior = self.settings.clone();
        prior.video_max_fps = settings.video_max_fps;
        self.active && prior == *settings
    }
    pub fn set_max_fps(&mut self, max_fps: f64) {
        self.settings.video_max_fps = max_fps;
    }
    pub fn start(
        camera: &Camera,
        info: &Value,
        settings: Settings,
        profile: &'static bayer::Profile,
    ) -> Result<Self> {
        validate(&settings, profile)?;
        let speed = link::validate(info, u32::from(profile.pid))?;
        camera.phase("video_initializing");
        let result = (|| {
            let (defects, _, _) = bayer::configure(camera, info, &settings, profile)?;
            // SDK SetFPSPerc(40), DDR enabled: USB2 and USB3 use different
            // FPGA bandwidth pacing. This is independent of ROI dimensions.
            bayer::word(
                camera,
                0xbd,
                0x24,
                if speed == link::Link::HighSpeed {
                    0x161c
                } else {
                    0x0180
                },
                2,
            )?;
            bayer::stop(camera)?;
            camera.vendor(0xa9, 0, 0, 0)?;
            camera.vendor(0xb6, 0x3004, 0, 0)?;
            camera.vendor(0xb6, 0x3000, 0, 0)?;
            std::thread::sleep(Duration::from_millis(50));
            let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
            camera.vendor(0xbd, 0, u16::from(flags & !0x10), 0)?;
            camera.reset_pipe()?;
            camera.phase("video_streaming");
            Ok(Self {
                profile,
                settings,
                defects,
                previous: None,
                delivered: 0,
                active: true,
                cleanup_failed: false,
            })
        })();
        if result.is_err()
            && let Err(error) = bayer::stop(camera)
        {
            super::diagnostics::log(
                "warning",
                "capture.cleanup_failed",
                format_args!("{error:#}"),
            );
        }
        result
    }

    pub fn next(
        &mut self,
        camera: &Camera,
        info: &Value,
        cancel: &AtomicBool,
    ) -> Result<(Value, Vec<u8>)> {
        ensure!(self.active, "video is stopped; start a new session");
        let result = recover_frame_once(
            self,
            cancel,
            |session| session.read(camera, cancel),
            |session| {
                let discarded_at = Instant::now();
                session.stop(camera)?;
                // Failed grabs count against the FPS cap too. No rapid retry burst.
                if !session.settings.continuous_drain {
                    wait_until_servicing(
                        discarded_at,
                        frame_interval(session.settings.video_max_fps)?,
                        cancel,
                        || camera.service_environment(),
                    )?;
                }
                *session = Self::start(camera, info, session.settings.clone(), session.profile)?;
                Ok(())
            },
        )
        .map(|((mut metadata, pixels), recoveries)| {
            metadata["framingRecoveries"] = json!(recoveries);
            (metadata, pixels)
        });
        if result.is_err() {
            // Partial video data cannot be replayed as the same exposure. End
            // this stream, preserve the failure and require explicit restart.
            let cleanup = self.stop(camera);
            return super::completion::finish(result, cleanup);
        }
        result
    }

    fn read(&mut self, camera: &Camera, cancel: &AtomicBool) -> Result<(Value, Vec<u8>)> {
        let start = Instant::now();
        ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
        if self.settings.long_exposure() {
            camera.phase("video_exposing");
            std::thread::sleep(Duration::from_millis(30));
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags | 1), 0)?;
            let triggered = Instant::now();
            // SDK 1.41 WorkingFunc: low power after 60 polling ticks, XHS
            // stop after 80 (10 ms intent; Windows tick rounding runs later).
            // Monotonic deadlines keep these inside the integration window.
            let integration = Duration::from_micros(u64::from(self.settings.microseconds));
            let wake = integration - Duration::from_millis(200);
            for (delay, register, mask) in [(600, 0x19, 1), (800, 0x0b, 0x10)] {
                let delay = Duration::from_millis(delay);
                if delay < wake {
                    wait_until_servicing(triggered, delay, cancel, || {
                        camera.service_environment()
                    })?;
                    let flags = camera.vendor(0xbc, register, 0, 1)?[0];
                    camera.vendor(0xbd, register, u16::from(flags | mask), 0)?;
                }
            }
            // Same explicit end-of-integration sequence on every video frame.
            // Stop remains cancellable rather than sleeping the full exposure.
            wait_until_servicing(
                triggered,
                Duration::from_micros(u64::from(self.settings.microseconds - 200_000)),
                cancel,
                || camera.service_environment(),
            )?;
            let state = camera.vendor(0xbc, 0x19, 0, 1)?[0];
            camera.vendor(0xbd, 0x19, u16::from(state & !1), 0)?;
            wait_until_servicing(
                triggered,
                Duration::from_micros(u64::from(self.settings.microseconds)),
                cancel,
                || camera.service_environment(),
            )?;
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags & !0x10), 0)?;
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags & !1), 0)?;
        }
        let length = self.settings.width as usize * self.settings.height as usize * 2;
        let mut pixels = camera.read_video_frame(length, cancel)?;
        ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
        let sequence = protocol::frame_sequence(&pixels, length)?;
        let skipped = sequence_gap(self.previous, sequence)?;
        self.previous = Some(sequence);
        self.delivered += 1;
        self.profile
            .replace_envelope(&mut pixels, self.settings.width as usize)?;
        self.defects.correct(&mut pixels)?;
        camera.phase("video_streaming");
        let metadata = json!({"sdkLoaded":false,"mode":"video","model":self.profile.name,
            "width":self.settings.width,"height":self.settings.height,"x":self.settings.x,"y":self.settings.y,
            "bin":1,"format":"RAW16","bayer":if self.profile.color { json!("RGGB") } else { Value::Null },"gain":self.settings.gain,"offset":self.settings.offset,
            "exposureMicroseconds":self.settings.microseconds,"bytes":pixels.len(),
            "maxFps":self.settings.video_max_fps,
            "boundarySequence":sequence,"skippedFrames":skipped,"deliveredFrames":self.delivered,
            "retainedReplay":false,"defectCorrectionApplied":true,"transportPixelsReplaced":true,
            "sha256":format!("{:x}",Sha256::digest(&pixels)),"elapsedMs":start.elapsed().as_millis()});
        Ok((metadata, pixels))
    }

    pub fn stop(&mut self, camera: &Camera) -> Result<()> {
        if !self.active && !self.cleanup_failed {
            return Ok(());
        }
        self.active = false;
        camera.phase("video_stopping");
        let wake = (|| -> Result<()> {
            if self.settings.long_exposure() {
                let flags = camera.vendor(0xbc, 0x19, 0, 1)?[0];
                camera.vendor(0xbd, 0x19, u16::from(flags & !1), 0)?;
                std::thread::sleep(Duration::from_millis(200));
                let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
                camera.vendor(0xbd, 0x0b, u16::from(flags & !0x10), 0)?;
                camera.vendor(0xbd, 0x0b, u16::from(flags & !0x11), 0)?;
            }
            Ok(())
        })();
        let stopped = bayer::stop(camera);
        self.cleanup_failed = wake.is_err() || stopped.is_err();
        wake?;
        stopped?;
        camera.phase("idle");
        Ok(())
    }
}

/// One fresh exposure after a complete-but-malformed frame. Never retained replay,
/// never a USB port reset, and never a retry of arbitrary hardware/cleanup errors.
pub(super) fn recover_frame_once<S, T>(
    state: &mut S,
    cancel: &AtomicBool,
    mut read: impl FnMut(&mut S) -> Result<T>,
    mut restart: impl FnMut(&mut S) -> Result<()>,
) -> Result<(T, u32)> {
    ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
    match read(state) {
        Ok(frame) => Ok((frame, 0)),
        Err(error)
            if error
                .downcast_ref::<protocol::FrameBoundaryError>()
                .is_some() =>
        {
            ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
            super::diagnostics::log(
                "warning",
                "video.frame_discarded",
                format_args!("{error:#}; restarting stream once for a fresh exposure"),
            );
            restart(state).with_context(|| {
                format!("video framing recovery restart failed after {error:#}")
            })?;
            ensure!(!cancel.load(Ordering::Relaxed), "video read cancelled");
            let frame =
                read(state).context("video framing recovery failed after one stream restart")?;
            super::diagnostics::log(
                "info",
                "video.framing_recovered",
                format_args!(
                    "Discarded malformed frame; validated a fresh frame after stream restart"
                ),
            );
            Ok((frame, 1))
        }
        Err(error) => Err(error),
    }
}

fn sequence_gap(previous: Option<u16>, next: u16) -> Result<u16> {
    let Some(previous) = previous else {
        return Ok(0);
    };
    let distance = next.wrapping_sub(previous);
    ensure!(distance != 0, "video returned the previous frame sequence");
    ensure!(distance < 32768, "video frame sequence moved backwards");
    Ok(distance - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bad_frame() -> anyhow::Error {
        protocol::frame_sequence(&[0; 16], 16).unwrap_err()
    }
    #[test]
    fn valid_frame_does_not_restart_or_consume_recovery() {
        let mut calls = 0;
        assert_eq!(
            recover_frame_once(
                &mut calls,
                &AtomicBool::new(false),
                |s| {
                    *s += 1;
                    Ok(42)
                },
                |_| panic!("valid frame must not restart")
            )
            .unwrap(),
            (42, 0)
        );
        assert_eq!(calls, 1);
    }
    #[test]
    fn framing_recovery_discards_bad_frame_and_restarts_only_once() {
        let mut calls = (0, 0);
        let result = recover_frame_once(
            &mut calls,
            &AtomicBool::new(false),
            |s| {
                s.0 += 1;
                if s.0 == 1 {
                    Err(bad_frame())
                } else {
                    Ok(vec![42])
                }
            },
            |s| {
                s.1 += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, (vec![42], 1));
        assert_eq!(calls, (2, 1));
        calls = (0, 0);
        let error = recover_frame_once::<_, ()>(
            &mut calls,
            &AtomicBool::new(false),
            |s| {
                s.0 += 1;
                Err(bad_frame())
            },
            |s| {
                s.1 += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            error
                .downcast_ref::<protocol::FrameBoundaryError>()
                .is_some()
        );
        assert_eq!(calls, (2, 1));
    }
    #[test]
    fn framing_recovery_never_retries_unrelated_errors_or_failed_restart() {
        let mut calls = (0, 0);
        assert!(
            recover_frame_once::<_, ()>(
                &mut calls,
                &AtomicBool::new(false),
                |s| {
                    s.0 += 1;
                    anyhow::bail!("USB disconnected")
                },
                |s| {
                    s.1 += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(calls, (1, 0));
        calls = (0, 0);
        assert!(
            recover_frame_once::<_, ()>(
                &mut calls,
                &AtomicBool::new(false),
                |s| {
                    s.0 += 1;
                    Err(bad_frame())
                },
                |s| {
                    s.1 += 1;
                    anyhow::bail!("stop failed")
                }
            )
            .is_err()
        );
        assert_eq!(calls, (1, 1));
    }
    #[test]
    fn framing_recovery_respects_cancel_before_restart_and_replacement() {
        for cancel_in_restart in [false, true] {
            let cancel = AtomicBool::new(false);
            let mut calls = (0, 0);
            assert!(
                recover_frame_once::<_, ()>(
                    &mut calls,
                    &cancel,
                    |s| {
                        s.0 += 1;
                        if !cancel_in_restart {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        Err(bad_frame())
                    },
                    |s| {
                        s.1 += 1;
                        cancel.store(true, Ordering::Relaxed);
                        Ok(())
                    }
                )
                .is_err()
            );
            assert_eq!(calls, (1, u32::from(cancel_in_restart)));
        }
    }
    #[test]
    fn model_profiles_keep_distinct_limits() {
        use crate::asi::direct::{asi662, asi676};
        let mut settings = Settings::default();
        validate(&settings, &asi676::PROFILE).unwrap();
        assert!(validate(&settings, &asi662::PROFILE).is_err());
        settings.width = 64;
        settings.height = 64;
        settings.x = 2;
        validate(&settings, &asi676::PROFILE).unwrap();
        assert!(validate(&settings, &asi662::PROFILE).is_err());
        settings.x = 0;
        settings.offset = 201;
        assert!(validate(&settings, &asi676::PROFILE).is_err());
        validate(&settings, &asi662::PROFILE).unwrap();
    }
    #[test]
    fn sequence_tracks_gaps_wrap_and_rejects_stale_frames() {
        assert_eq!(sequence_gap(None, 24).unwrap(), 0);
        assert_eq!(sequence_gap(Some(24), 25).unwrap(), 0);
        assert_eq!(sequence_gap(Some(24), 28).unwrap(), 3);
        assert_eq!(sequence_gap(Some(u16::MAX), 0).unwrap(), 0);
        assert!(sequence_gap(Some(24), 24).is_err());
        assert!(sequence_gap(Some(24), 23).is_err());
    }
    #[test]
    fn stop_interrupts_exposure_wait_without_hardware() {
        assert!(
            wait_until(
                Instant::now(),
                Duration::from_secs(30),
                &AtomicBool::new(true)
            )
            .is_err()
        );
    }
    #[test]
    fn fractional_fps_is_an_acquisition_cap_and_wait_is_cancellable() {
        assert_eq!(frame_interval(0.5).unwrap(), Duration::from_secs(2));
        assert_eq!(frame_interval(0.01).unwrap(), Duration::from_secs(100));
        for invalid in [0.0, -1.0, 0.001, 121.0, f64::INFINITY, f64::NAN] {
            assert!(frame_interval(invalid).is_err());
        }
        let mut pacer = Pacer::default();
        pacer.wait(0.01, &AtomicBool::new(false)).unwrap();
        pacer.completed();
        assert!(pacer.wait(0.01, &AtomicBool::new(true)).is_err());
        // An expired slot does not create catch-up credits.
        pacer.previous = Some(Instant::now() - Duration::from_secs(3));
        pacer.wait(0.5, &AtomicBool::new(false)).unwrap();
        pacer.completed();
        assert!(pacer.previous.unwrap().elapsed() < Duration::from_secs(1));
    }
}
