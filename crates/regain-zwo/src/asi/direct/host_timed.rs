//! SDK 1.41 ASI2600/6200 host-timed handshake and clock restoration.
//! Cancellation is observed between complete register transactions, never
//! between a synchronization pulse's edges or during clock restoration.
//!
//! Register notation: BC reads and BD writes the FPGA byte registers; B6
//! writes a separate sensor-side address space. "Gate" and "standby" describe
//! their observed acquisition roles, not manufacturer register definitions:
//! - BD00 mask 0x10 (bit 4): SDK synchronization pulse / full-stop control.
//!   Setting it is unsafe when preserving a retained frame; it is not a
//!   general-purpose "cancel" bit.
//! - BC23 mask 0x10: synchronization indication. The complete values 0x01 and
//!   0x15 are separately checked by acquisition for cleared/advertised retained
//!   state; the latter does not prove every sensor row has reached DDR.
//! - BD0b masks 0x01 and 0x10, BD19 mask 0x01: long-exposure gates. Clear all
//!   three in SDK order before acknowledging cancellation or reusing capture.
//! - BD19 also contains thermal controls: mask 0x80 disables cooling, mask
//!   0x40 enables the heater. Always read-modify-write its exposure bit.
//! - B6:019e (6200) / B6:01ee (2600): values 5 and 1 are observed standby and
//!   resume commands. Their individual bit definitions are not established.
//!
//! Evidence: SDK 1.41 x64 worker RVAs 1ed2cc..1ed60b (6200) and
//! 14c30c..14c636 (2600), plus the saved P25 Camera Kit USB exercise
//! exposure-2000000us (20260915T012106Z-31f0a25c). See
//! docs/camera-usb-registers.md for the register reference and
//! docs/asi6200-abort-cleanup.md for cancellation comparisons and acceptance.
use super::transport::Camera;
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Protocol {
    model: &'static str,
    // Sensor register, not FPGA BD19 despite the similar hexadecimal address.
    standby_register: u16,
    // SDK delay after queuing the first USB reads and before the first sync
    // pulse. Verified for the 6200 worker; keep other models independent.
    initial_sync_settle_ms: u64,
    // Distinct SDK constants for >1000 ms and the 1000 ms branch after
    // truncating exposure to whole milliseconds. They shorten the host wait
    // before restoration; they do not alter programmed sensor timing.
    // Their physical derivation is unknown, so do not infer or retune them
    // from USB transfer latency or a presumed shutter/readout delay.
    integration_compensation_ms: u32,
    one_second_compensation_ms: u32,
}
// SDK worker 1ed2cc..1ed60b; independent saved P25 USB trace.
pub(super) const ASI6200: Protocol = Protocol {
    model: "ASI6200",
    standby_register: 0x19e,
    initial_sync_settle_ms: 15,
    integration_compensation_ms: 200,
    one_second_compensation_ms: 200,
};
// SDK worker 14c30c..14c636, same pulse/gates with distinct sensor/timing.
// SDK 1.41 SHA256 0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a.
pub(super) const ASI2600: Protocol = Protocol {
    model: "ASI2600",
    standby_register: 0x1ee,
    initial_sync_settle_ms: 0,
    integration_compensation_ms: 400,
    one_second_compensation_ms: 205,
};

pub(super) trait Io {
    fn read(&self, register: u16) -> Result<u8>;
    fn write(&self, request: u8, register: u16, value: u8) -> Result<()>;
    fn service(&self) -> Result<()>;
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
    fn phase(&self, phase: &'static str);
}

impl Io for Camera {
    fn read(&self, register: u16) -> Result<u8> {
        Ok(self.vendor(0xbc, register, 0, 1)?[0])
    }
    fn write(&self, request: u8, register: u16, value: u8) -> Result<()> {
        self.vendor(request, register, u16::from(value), 0)?;
        Ok(())
    }
    fn service(&self) -> Result<()> {
        self.service_environment()
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn phase(&self, phase: &'static str) {
        Camera::phase(self, phase);
    }
}

/// Preserve cooler/auxiliary bits in BD19. SDK workers reach this sequence even
/// when their integration cancellation flag is set (6200: 1ed575..1ed60b;
/// 2600: cancellation at 14c551..14c55c branches to restoration at 14c5a0).
/// Do not call service() here: it can throw cancellation before clocks resume.
fn restore_clocks(io: &impl Io, protocol: Protocol) -> Result<()> {
    // SDK order is FPGA bit 0 -> 100 ms -> sensor resume -> 100 ms ->
    // FPGA 0b bit 4 -> FPGA 0b bit 0. Fresh reads preserve unrelated flags
    // and account for state changes caused by the preceding write.
    let flags = io.read(0x19)?;
    io.write(0xbd, 0x19, flags & !1)?;
    io.sleep(Duration::from_millis(100));
    io.write(0xb6, protocol.standby_register, 1)?;
    io.sleep(Duration::from_millis(100));
    let flags = io.read(0xb)?;
    io.write(0xbd, 0xb, flags & !0x10)?;
    let flags = io.read(0xb)?;
    io.write(0xbd, 0xb, flags & !1)?;
    verify_clocks(io, protocol)
}

fn verify_clocks(io: &impl Io, protocol: Protocol) -> Result<()> {
    let gate = io.read(0x19)?;
    let exposure = io.read(0xb)?;
    ensure!(
        gate & 1 == 0 && exposure & 0x11 == 0,
        "{} clocks did not resume: BC19={gate:#04x}, BC0b={exposure:#04x}",
        protocol.model
    );
    Ok(())
}

/// Also repair gates left by an earlier worker before arming another frame.
/// BC23=1 only proves DDR was cleared; it does not prove these gates are open.
pub(super) fn restore_if_gated(io: &impl Io, protocol: Protocol) -> Result<()> {
    let gate = io.read(0x19)?;
    let exposure = io.read(0xb)?;
    if gate & 1 != 0 || exposure & 0x11 != 0 {
        crate::asi::direct::diagnostics::log(
            "warning",
            "capture.stale_clocks",
            format_args!(
                "{} stop restoring stale clocks: BC19={gate:#04x}, BC0b={exposure:#04x}",
                protocol.model
            ),
        );
        io.phase("restoring_clocks");
        restore_clocks(io, protocol)?;
    }
    Ok(())
}

pub(super) fn expose(io: &impl Io, protocol: Protocol, microseconds: u32) -> Result<()> {
    // Callers use this path only for exposures >= 1 s; shorter exposures use
    // the sensor frame/shutter registers without these host-controlled gates.
    io.phase("synchronizing");
    let mut stage = "synchronizing";
    let result = (|| {
        // The ASI6200 SDK queues its first USB batch before Sleep(15), then
        // begins the bounded pulse loop (RVA 1ed2cc..1ed401). Do not service
        // cancellation here: preserve the complete pulse before its existing
        // integration checkpoint, followed by unconditional clock restoration.
        if protocol.initial_sync_settle_ms > 0 {
            io.sleep(Duration::from_millis(protocol.initial_sync_settle_ms));
        }
        let mut synchronized = false;
        for _ in 0..5 {
            // SDK bounded handshake: set BD00 bit 4, hold 5 ms, clear it,
            // wait 20 ms, then inspect BC23 bit 4. Complete both edges before
            // servicing cancellation; a half pulse is not a clean stop.
            let flags = io.read(0)?;
            io.write(0xbd, 0, flags | 0x10)?;
            io.sleep(Duration::from_millis(5));
            let flags = io.read(0)?;
            io.write(0xbd, 0, flags & !0x10)?;
            io.sleep(Duration::from_millis(20));
            if io.read(0x23)? & 0x10 != 0 {
                synchronized = true;
                break;
            }
        }
        ensure!(
            synchronized,
            "{} long exposure synchronization failed",
            protocol.model
        );
        let flags = io.read(0xb)?;
        io.write(0xbd, 0xb, flags | 1)?;
        io.phase("integrating");
        stage = "integrating";
        let exposure_ms = microseconds / 1000;
        if exposure_ms > 1000 {
            let trigger = io.now();
            let mut iteration = 0;
            loop {
                // Preserve SDK iteration placement (nominally 600/800/1000 ms
                // after integration starts). Actual times include USB latency.
                // These are staged gating operations, not three new exposures.
                match iteration {
                    6 => io.write(0xb6, protocol.standby_register, 5)?,
                    8 => {
                        let flags = io.read(0x19)?;
                        io.write(0xbd, 0x19, flags | 1)?;
                    }
                    10 => {
                        let flags = io.read(0xb)?;
                        io.write(0xbd, 0xb, flags | 0x10)?;
                    }
                    _ => {}
                }
                let elapsed = io.now().duration_since(trigger).as_millis();
                io.service()?;
                io.sleep(Duration::from_millis(100));
                iteration += 1;
                if elapsed >= u128::from(exposure_ms - protocol.integration_compensation_ms) {
                    break;
                }
            }
        } else {
            // SDK's 1000-whole-millisecond branch does not set the later gates.
            // Keep its separate model-specific compensation (200/205 ms).
            io.service()?;
            io.sleep(Duration::from_millis(u64::from(
                exposure_ms - protocol.one_second_compensation_ms,
            )));
        }
        Ok(())
    })();
    if let Err(error) = &result {
        let registers = [
            io.read(0).ok(),
            io.read(0xb).ok(),
            io.read(0x19).ok(),
            io.read(0x23).ok(),
        ];
        crate::asi::direct::diagnostics::details(
            "info",
            "capture.clock_cleanup",
            // The supervisor forwards message, not structured details, to NINA.
            format_args!(
                "{} {stage}, {microseconds} us: {error:#}; restoring clocks; BC00/0b/19/23={registers:02x?}",
                protocol.model
            ),
            serde_json::json!({"model":protocol.model,"phase":stage,"microseconds":microseconds,
                "BC00":registers[0],"BC0b":registers[1],
                "BC19":registers[2],"BC23":registers[3]}),
        );
    }
    // Includes synchronization/USB/environment errors, not just cancellation.
    // A partial register transaction must never be acknowledged as a clean stop.
    io.phase("restoring_clocks");
    let cleanup = restore_clocks(io, protocol);
    match (result, cleanup) {
        (Ok(()), cleanup) => cleanup,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => anyhow::bail!(
            "{} exposure failed: {error:#}; clock restoration failed: {cleanup:#}",
            protocol.model
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asi::direct::completion::CaptureCancelled;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, PartialEq)]
    enum Operation {
        Read(u16),
        Write(u8, u16, u8),
        Sleep(Duration),
    }

    struct Registers {
        fpga: RefCell<[u8; 0x24]>,
        standby: Cell<u8>,
        origin: Instant,
        elapsed: Cell<Duration>,
        cancel_at: Cell<Option<Duration>>,
        fail_service_at: Option<Duration>,
        synchronize: bool,
        ignore_gate_clear: bool,
        fail_write: Cell<Option<(u8, u16, u8)>>,
        writes: RefCell<Vec<(u8, u16, u8)>>,
        phases: RefCell<Vec<&'static str>>,
        operations: RefCell<Vec<Operation>>,
    }
    impl Default for Registers {
        fn default() -> Self {
            let mut fpga = [0; 0x24];
            fpga[0] = 0xc0;
            fpga[0xb] = 0x44; // Unrelated FPGA flags must survive.
            fpga[0x19] = 0xc0; // Cooler disabled (bit 7 inverted), heater enabled.
            fpga[0x23] = 1;
            Self {
                fpga: RefCell::new(fpga),
                standby: Cell::new(1),
                origin: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
                cancel_at: Cell::new(None),
                fail_service_at: None,
                synchronize: true,
                ignore_gate_clear: false,
                fail_write: Cell::new(None),
                writes: RefCell::default(),
                phases: RefCell::default(),
                operations: RefCell::default(),
            }
        }
    }
    impl Registers {
        fn assert_clocks_restored(&self) {
            assert_eq!(self.fpga.borrow()[0xb], 0x44);
            assert_eq!(self.fpga.borrow()[0x19], 0xc0);
            assert_eq!(self.standby.get(), 1);
            assert_eq!(self.fpga.borrow()[0] & 0x10, 0, "sync pulse left high");
        }
    }
    impl Io for Registers {
        fn read(&self, register: u16) -> Result<u8> {
            self.operations.borrow_mut().push(Operation::Read(register));
            Ok(self.fpga.borrow()[usize::from(register)])
        }
        fn write(&self, request: u8, register: u16, value: u8) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(Operation::Write(request, register, value));
            self.writes.borrow_mut().push((request, register, value));
            if self.fail_write.get() == Some((request, register, value)) {
                self.fail_write.set(None);
                anyhow::bail!("USB control transfer failed");
            }
            match request {
                0xbd => {
                    let mut fpga = self.fpga.borrow_mut();
                    let old = fpga[usize::from(register)];
                    if !(self.ignore_gate_clear && register == 0x19 && value & 1 == 0) {
                        fpga[usize::from(register)] = value;
                    }
                    if register == 0 && old & 0x10 != 0 && value & 0x10 == 0 {
                        // The next handshake requires running clocks. This
                        // catches stale gates surviving cancellation/reuse.
                        if self.synchronize && fpga[0x19] & 1 == 0 && fpga[0xb] & 0x11 == 0 {
                            fpga[0x23] = 0x11;
                        }
                    }
                }
                0xb6 if matches!(register, 0x19e | 0x1ee) => self.standby.set(value),
                _ => panic!("unexpected register request"),
            }
            Ok(())
        }
        fn service(&self) -> Result<()> {
            if self
                .cancel_at
                .get()
                .is_some_and(|at| self.elapsed.get() >= at)
            {
                return Err(CaptureCancelled.into());
            }
            if self
                .fail_service_at
                .is_some_and(|at| self.elapsed.get() >= at)
            {
                anyhow::bail!("temperature read failed");
            }
            Ok(())
        }
        fn now(&self) -> Instant {
            self.origin + self.elapsed.get()
        }
        fn sleep(&self, duration: Duration) {
            self.operations
                .borrow_mut()
                .push(Operation::Sleep(duration));
            self.elapsed.set(self.elapsed.get() + duration);
        }
        fn phase(&self, phase: &'static str) {
            self.phases.borrow_mut().push(phase);
        }
    }

    #[test]
    fn cancellation_at_sync_and_every_clock_gate_allows_another_exposure() {
        // Arrive before/during either sync edge, before standby, after each
        // gate, and late in integration. Fake time avoids sleeps in tests.
        for millis in [0, 2, 14, 15, 17, 39, 40, 640, 840, 1040, 1540] {
            let io = Registers::default();
            io.cancel_at.set(Some(Duration::from_millis(millis)));
            let error = expose(&io, ASI6200, 2_000_000).unwrap_err();
            assert!(error.is::<CaptureCancelled>(), "{millis}: {error:#}");
            io.assert_clocks_restored();
            assert_eq!(
                &io.writes.borrow()[..2],
                &[(0xbd, 0, 0xd0), (0xbd, 0, 0xc0)]
            );
            assert!(io.elapsed.get() < Duration::from_secs(2));
            io.cancel_at.set(None);
            io.fpga.borrow_mut()[0x23] = 1;
            expose(&io, ASI6200, 2_000_000).unwrap();
            io.assert_clocks_restored();
        }
    }

    #[test]
    fn asi6200_queued_pipe_settles_before_first_complete_sync_pulse() {
        use Operation::*;
        let io = Registers::default();
        expose(&io, ASI6200, 1_000_000).unwrap();
        assert_eq!(
            &io.operations.borrow()[..8],
            &[
                Sleep(Duration::from_millis(15)),
                Read(0),
                Write(0xbd, 0, 0xd0),
                Sleep(Duration::from_millis(5)),
                Read(0),
                Write(0xbd, 0, 0xc0),
                Sleep(Duration::from_millis(20)),
                Read(0x23),
            ]
        );
        let io = Registers::default();
        expose(&io, ASI2600, 1_000_000).unwrap();
        assert_eq!(io.operations.borrow()[0], Read(0));
    }

    #[test]
    fn successful_sequence_matches_sdk_clock_restore_order() {
        let io = Registers::default();
        expose(&io, ASI6200, 2_000_000).unwrap();
        io.assert_clocks_restored();
        // SDK 1.41 RVA 1ed44c..1ed60b, also observed in the saved P25
        // Camera Kit exposure-2000000us USB trace (20260915T012106Z).
        assert_eq!(
            *io.writes.borrow(),
            [
                (0xbd, 0, 0xd0),
                (0xbd, 0, 0xc0),
                (0xbd, 0xb, 0x45),
                (0xb6, 0x19e, 5),
                (0xbd, 0x19, 0xc1),
                (0xbd, 0xb, 0x55),
                (0xbd, 0x19, 0xc0),
                (0xb6, 0x19e, 1),
                (0xbd, 0xb, 0x45),
                (0xbd, 0xb, 0x44),
            ]
        );
        assert_eq!(
            *io.phases.borrow(),
            ["synchronizing", "integrating", "restoring_clocks"]
        );
    }

    #[test]
    fn cancellation_arriving_during_restoration_cannot_interrupt_it() {
        let io = Registers::default();
        io.cancel_at.set(Some(Duration::from_millis(2015)));
        expose(&io, ASI6200, 2_000_000).unwrap();
        io.assert_clocks_restored();
        // The acquisition caller observes the pending cancellation at its
        // next ready/readout checkpoint, after restoration has finished.
        assert!(io.service().unwrap_err().is::<CaptureCancelled>());
    }

    #[test]
    fn environment_error_also_restores_clocks_without_masking_error() {
        let io = Registers {
            fail_service_at: Some(Duration::from_millis(840)),
            ..Registers::default()
        };
        assert_eq!(
            expose(&io, ASI6200, 2_000_000).unwrap_err().to_string(),
            "temperature read failed"
        );
        io.assert_clocks_restored();
    }

    #[test]
    fn cancellation_restoration_preserves_every_cooler_and_heater_state() {
        for thermal_flags in [0, 0x40, 0x80, 0xc0] {
            let io = Registers::default();
            io.fpga.borrow_mut()[0x19] = thermal_flags;
            io.cancel_at.set(Some(Duration::from_millis(1040)));
            assert!(
                expose(&io, ASI6200, 2_000_000)
                    .unwrap_err()
                    .is::<CaptureCancelled>()
            );
            assert_eq!(io.fpga.borrow()[0x19], thermal_flags);
            assert_eq!(io.fpga.borrow()[0xb], 0x44);
            assert_eq!(io.standby.get(), 1);
        }
    }

    #[test]
    fn usb_failure_after_gating_also_unwinds_for_reuse() {
        let io = Registers::default();
        io.fail_write.set(Some((0xbd, 0xb, 0x55)));
        assert_eq!(
            expose(&io, ASI6200, 2_000_000).unwrap_err().to_string(),
            "USB control transfer failed"
        );
        io.assert_clocks_restored();
        io.fpga.borrow_mut()[0x23] = 1;
        expose(&io, ASI6200, 2_000_000).unwrap();
    }

    #[test]
    fn one_second_mode_honors_early_cancellation_and_restores_clocks() {
        let io = Registers::default();
        io.cancel_at.set(Some(Duration::ZERO));
        assert!(
            expose(&io, ASI6200, 1_000_000)
                .unwrap_err()
                .is::<CaptureCancelled>()
        );
        io.assert_clocks_restored();
        assert_eq!(io.elapsed.get(), Duration::from_millis(240));
    }

    #[test]
    fn stale_gates_are_repaired_without_changing_thermal_flags() {
        let io = Registers::default();
        io.fpga.borrow_mut()[0xb] |= 0x11;
        io.fpga.borrow_mut()[0x19] |= 1;
        io.standby.set(5);
        // Old cleanup could see BC23=1 here and report success.
        assert_eq!(io.read(0x23).unwrap(), 1);
        restore_if_gated(&io, ASI6200).unwrap();
        io.assert_clocks_restored();
        expose(&io, ASI6200, 2_000_000).unwrap();
    }

    #[test]
    fn unsuccessful_readback_is_not_acknowledged_as_clean_cancellation() {
        let io = Registers {
            ignore_gate_clear: true,
            ..Registers::default()
        };
        io.cancel_at.set(Some(Duration::from_millis(840)));
        let error = expose(&io, ASI6200, 2_000_000).unwrap_err();
        assert!(!error.is::<CaptureCancelled>());
        assert!(error.to_string().contains("clocks did not resume"));
        assert!(restore_if_gated(&io, ASI6200).is_err());
    }

    #[test]
    fn synchronization_failure_remains_bounded_and_restores_clocks() {
        let io = Registers {
            synchronize: false,
            ..Registers::default()
        };
        assert_eq!(
            expose(&io, ASI6200, 2_000_000).unwrap_err().to_string(),
            "ASI6200 long exposure synchronization failed"
        );
        assert_eq!(io.elapsed.get(), Duration::from_millis(340));
        io.assert_clocks_restored();
    }

    #[test]
    fn asi2600_cancellation_at_every_gate_allows_another_exposure() {
        for millis in [0, 2, 24, 25, 625, 825, 1025, 1525] {
            for thermal_flags in [0, 0x40, 0x80, 0xc0] {
                let io = Registers::default();
                io.fpga.borrow_mut()[0x19] = thermal_flags;
                io.cancel_at.set(Some(Duration::from_millis(millis)));
                assert!(
                    expose(&io, ASI2600, 2_000_000)
                        .unwrap_err()
                        .is::<CaptureCancelled>()
                );
                assert_eq!(io.fpga.borrow()[0x19], thermal_flags);
                assert_eq!(io.fpga.borrow()[0xb], 0x44);
                assert_eq!(io.standby.get(), 1);
                assert_eq!(io.fpga.borrow()[0] & 0x10, 0);
                io.cancel_at.set(None);
                io.fpga.borrow_mut()[0x23] = 1;
                expose(&io, ASI2600, 2_000_000).unwrap();
            }
        }
    }

    #[test]
    fn profiles_preserve_independent_sdk_sensor_registers_and_timing() {
        for (protocol, register, long_ms, one_second_ms) in
            [(ASI6200, 0x19e, 2140, 1040), (ASI2600, 0x1ee, 1925, 1020)]
        {
            let io = Registers::default();
            expose(&io, protocol, 2_000_000).unwrap();
            assert_eq!(io.elapsed.get(), Duration::from_millis(long_ms));
            assert_eq!(
                *io.writes.borrow(),
                [
                    (0xbd, 0, 0xd0),
                    (0xbd, 0, 0xc0),
                    (0xbd, 0xb, 0x45),
                    (0xb6, register, 5),
                    (0xbd, 0x19, 0xc1),
                    (0xbd, 0xb, 0x55),
                    (0xbd, 0x19, 0xc0),
                    (0xb6, register, 1),
                    (0xbd, 0xb, 0x45),
                    (0xbd, 0xb, 0x44),
                ]
            );
            let io = Registers::default();
            expose(&io, protocol, 1_000_000).unwrap();
            assert_eq!(io.elapsed.get(), Duration::from_millis(one_second_ms));
            io.assert_clocks_restored();
        }
    }

    #[test]
    fn asi2600_restoration_and_errors_keep_model_identity_and_reuse_contract() {
        let io = Registers::default();
        io.cancel_at.set(Some(Duration::from_millis(1900)));
        expose(&io, ASI2600, 2_000_000).unwrap();
        io.assert_clocks_restored();
        assert!(io.service().unwrap_err().is::<CaptureCancelled>());

        let io = Registers {
            synchronize: false,
            ..Registers::default()
        };
        assert_eq!(
            expose(&io, ASI2600, 2_000_000).unwrap_err().to_string(),
            "ASI2600 long exposure synchronization failed"
        );
        io.assert_clocks_restored();

        let io = Registers::default();
        io.fail_write.set(Some((0xbd, 0xb, 0x55)));
        assert_eq!(
            expose(&io, ASI2600, 2_000_000).unwrap_err().to_string(),
            "USB control transfer failed"
        );
        io.assert_clocks_restored();

        let io = Registers {
            ignore_gate_clear: true,
            ..Registers::default()
        };
        io.cancel_at.set(Some(Duration::from_millis(825)));
        let error = expose(&io, ASI2600, 2_000_000).unwrap_err();
        assert!(!error.is::<CaptureCancelled>());
        assert!(error.to_string().contains("ASI2600 clocks did not resume"));
        assert!(restore_if_gated(&io, ASI2600).is_err());
    }
}
